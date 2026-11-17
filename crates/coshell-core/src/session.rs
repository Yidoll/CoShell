//! Bounded session lifecycle and output cache. No command input is exposed here.
use coshell_drivers::ssh::TerminalChannel;
use coshell_types::{SessionEvent, SessionEventPayload, SessionState};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{watch, Mutex};

const MAX_SAFE_SEQUENCE: u64 = 9_007_199_254_740_991;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    InvalidConfig,
    NotFound,
    Duplicate,
    InvalidState,
    Timeout,
    Transport,
    SequenceExhausted,
    Capacity,
}

struct CloseState {
    channel: Option<Arc<dyn TerminalChannel>>,
    result: Option<Result<(), SessionError>>,
}
struct Entry {
    state: SessionState,
    sequence: u64,
    events: VecDeque<SessionEvent>,
    scrollback: VecDeque<u8>,
    close: Arc<Mutex<CloseState>>,
    close_generation: u64,
    in_flight: Option<watch::Sender<Option<Result<(), SessionError>>>>,
}
pub struct SessionSupervisor {
    sessions: Arc<Mutex<HashMap<String, Entry>>>,
    event_limit: usize,
    scrollback_limit: usize,
    close_timeout: Duration,
    max_sessions: usize,
}

pub struct EventBatch {
    pub gap: bool,
    pub events: Vec<SessionEvent>,
}
pub struct ResyncSnapshot {
    pub state: SessionState,
    pub scrollback: Vec<u8>,
    pub watermark: u64,
}

impl SessionSupervisor {
    pub fn new(
        event_limit: usize,
        scrollback_limit: usize,
        close_timeout: Duration,
    ) -> Result<Self, SessionError> {
        Self::with_limits(64, event_limit, scrollback_limit, close_timeout)
    }
    /// Budget conservatively accounts for event payloads, 256-byte IDs, deque
    /// growth slack and fixed per-session bookkeeping. It excludes caller-owned
    /// channels, outstanding request futures and allocator/runtime overhead.
    pub fn with_limits(
        max_sessions: usize,
        event_limit: usize,
        scrollback_limit: usize,
        close_timeout: Duration,
    ) -> Result<Self, SessionError> {
        if max_sessions == 0
            || max_sessions > 64
            || event_limit < 2
            || event_limit > 512
            || scrollback_limit == 0
            || close_timeout.is_zero()
            || (|| {
                max_sessions.checked_mul(
                    event_limit
                        .checked_mul(scrollback_limit.checked_add(1024)?)?
                        .checked_add(scrollback_limit.checked_mul(2)?)?
                        .checked_add(8192)?,
                )
            })()
            .is_none_or(|n| n > 64 * 1024 * 1024)
        {
            return Err(SessionError::InvalidConfig);
        }
        Ok(Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            event_limit,
            scrollback_limit,
            close_timeout,
            max_sessions,
        })
    }
    pub async fn register(
        &self,
        id: &str,
        _connection: &str,
        _production: bool,
        channel: Arc<dyn TerminalChannel>,
    ) -> Result<(), SessionError> {
        if id.is_empty() || id.len() > 256 {
            return Err(SessionError::InvalidConfig);
        }
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(id) {
            return Err(SessionError::Duplicate);
        }
        if sessions.len() >= self.max_sessions {
            return Err(SessionError::Capacity);
        }
        let mut entry = Entry {
            state: SessionState::Connecting,
            sequence: 0,
            events: VecDeque::new(),
            scrollback: VecDeque::new(),
            close: Arc::new(Mutex::new(CloseState {
                channel: Some(channel),
                result: None,
            })),
            close_generation: 0,
            in_flight: None,
        };
        self.emit(
            id,
            &mut entry,
            SessionEventPayload::State {
                state: SessionState::Connecting,
            },
        )?;
        sessions.insert(id.into(), entry);
        Ok(())
    }
    fn emit(
        &self,
        id: &str,
        entry: &mut Entry,
        payload: SessionEventPayload,
    ) -> Result<(), SessionError> {
        if entry.sequence >= MAX_SAFE_SEQUENCE {
            return Err(SessionError::SequenceExhausted);
        }
        entry.sequence += 1;
        if entry.events.len() == self.event_limit {
            entry.events.pop_front();
        }
        entry.events.push_back(SessionEvent {
            session_id: id.into(),
            sequence: entry.sequence,
            payload,
        });
        Ok(())
    }
    pub async fn connected(&self, id: &str) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get_mut(id).ok_or(SessionError::NotFound)?;
        if entry.state != SessionState::Connecting {
            return Err(SessionError::InvalidState);
        }
        self.emit(
            id,
            entry,
            SessionEventPayload::State {
                state: SessionState::Connected,
            },
        )?;
        entry.state = SessionState::Connected;
        Ok(())
    }
    pub async fn output(&self, id: &str, bytes: &[u8]) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get_mut(id).ok_or(SessionError::NotFound)?;
        if entry.state != SessionState::Connected {
            return Err(SessionError::InvalidState);
        }
        let removed = entry
            .scrollback
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.scrollback_limit);
        let retain = bytes.len().min(self.scrollback_limit);
        let from = bytes.len() - retain;
        let needed = if removed > 0 { 2 } else { 1 };
        if entry.sequence > MAX_SAFE_SEQUENCE - needed {
            return Err(SessionError::SequenceExhausted);
        }
        for _ in 0..removed.min(entry.scrollback.len()) {
            entry.scrollback.pop_front();
        }
        entry.scrollback.extend(&bytes[from..]);
        if removed > 0 {
            self.emit(
                id,
                entry,
                SessionEventPayload::Truncated {
                    discarded_bytes: removed as u64,
                },
            )?;
        }
        self.emit(
            id,
            entry,
            SessionEventPayload::Output {
                data: bytes[from..].to_vec(),
            },
        )
    }
    pub async fn state(&self, id: &str) -> Result<SessionState, SessionError> {
        Ok(self
            .sessions
            .lock()
            .await
            .get(id)
            .ok_or(SessionError::NotFound)?
            .state)
    }
    pub async fn scrollback(&self, id: &str) -> Result<Vec<u8>, SessionError> {
        Ok(self
            .sessions
            .lock()
            .await
            .get(id)
            .ok_or(SessionError::NotFound)?
            .scrollback
            .iter()
            .copied()
            .collect())
    }
    pub async fn drain_events(&self, id: &str) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(self
            .sessions
            .lock()
            .await
            .get_mut(id)
            .ok_or(SessionError::NotFound)?
            .events
            .drain(..)
            .collect())
    }
    /// A gap means the caller must discard incremental output and reload scrollback.
    pub async fn events_since(&self, id: &str, sequence: u64) -> Result<EventBatch, SessionError> {
        let sessions = self.sessions.lock().await;
        let entry = sessions.get(id).ok_or(SessionError::NotFound)?;
        let earliest = entry
            .events
            .front()
            .map_or(entry.sequence.saturating_add(1), |e| e.sequence);
        Ok(EventBatch {
            gap: sequence > entry.sequence || sequence.saturating_add(1) < earliest,
            events: entry
                .events
                .iter()
                .filter(|e| e.sequence > sequence)
                .cloned()
                .collect(),
        })
    }
    pub async fn resync(&self, id: &str) -> Result<ResyncSnapshot, SessionError> {
        let sessions = self.sessions.lock().await;
        let entry = sessions.get(id).ok_or(SessionError::NotFound)?;
        Ok(ResyncSnapshot {
            state: entry.state,
            scrollback: entry.scrollback.iter().copied().collect(),
            watermark: entry.sequence,
        })
    }
    pub async fn release(&self, id: &str) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get(id).ok_or(SessionError::NotFound)?;
        if !matches!(
            entry.state,
            SessionState::Closed | SessionState::Disconnected
        ) {
            return Err(SessionError::InvalidState);
        }
        sessions.remove(id);
        Ok(())
    }
    pub async fn close(&self, id: &str) -> Result<(), SessionError> {
        self.terminate(id, SessionState::Closed).await
    }
    pub async fn disconnect(&self, id: &str) -> Result<(), SessionError> {
        self.terminate(id, SessionState::Disconnected).await
    }
    async fn terminate(&self, id: &str, state: SessionState) -> Result<(), SessionError> {
        let deadline = tokio::time::Instant::now() + self.close_timeout;
        let mut receiver = {
            let mut sessions = tokio::time::timeout_at(deadline, self.sessions.lock())
                .await
                .map_err(|_| SessionError::Timeout)?;
            let entry = sessions.get_mut(id).ok_or(SessionError::NotFound)?;
            if matches!(
                entry.state,
                SessionState::Closed | SessionState::Disconnected
            ) {
                return Ok(());
            }
            if let Some(sender) = &entry.in_flight {
                sender.subscribe()
            } else {
                if entry.state != SessionState::Closing {
                    let _ = self.emit(
                        id,
                        entry,
                        SessionEventPayload::State {
                            state: SessionState::Closing,
                        },
                    );
                    entry.state = SessionState::Closing;
                }
                entry.close_generation = entry.close_generation.saturating_add(1);
                let (sender, receiver) = watch::channel(None);
                entry.in_flight = Some(sender.clone());
                let close = entry.close.clone();
                let generation = entry.close_generation;
                let sessions = self.sessions.clone();
                let id = id.to_owned();
                let limit = self.event_limit;
                tokio::spawn(async move {
                    let result = close_channel(close, deadline).await;
                    let update = tokio::time::timeout_at(
                        deadline,
                        finish_state(sessions.clone(), &id, generation, state, result, limit),
                    )
                    .await;
                    let final_result = match update {
                        Ok(Ok(())) => result,
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(SessionError::Timeout),
                    };
                    sender.send_replace(Some(final_result));
                    if update.is_err() {
                        let _ = finish_state(sessions, &id, generation, state, result, limit).await;
                    }
                });
                receiver
            }
        };
        if let Some(result) = *receiver.borrow_and_update() {
            return result;
        }
        tokio::time::timeout_at(deadline, receiver.changed())
            .await
            .map_err(|_| SessionError::Timeout)?
            .map_err(|_| SessionError::Transport)?;
        let result = (*receiver.borrow()).ok_or(SessionError::Transport)?;
        result
    }
    pub async fn shutdown(&self) -> Result<(), SessionError> {
        let deadline = tokio::time::Instant::now() + self.close_timeout;
        let closes = {
            let mut sessions = tokio::time::timeout_at(deadline, self.sessions.lock())
                .await
                .map_err(|_| SessionError::Timeout)?;
            let mut closes = Vec::new();
            for (id, entry) in sessions.iter_mut() {
                if !matches!(
                    entry.state,
                    SessionState::Closed | SessionState::Disconnected
                ) {
                    if let Some(sender) = &entry.in_flight {
                        closes.push(sender.subscribe());
                        continue;
                    }
                    if entry.state != SessionState::Closing {
                        let _ = self.emit(
                            id,
                            entry,
                            SessionEventPayload::State {
                                state: SessionState::Closing,
                            },
                        );
                        entry.state = SessionState::Closing;
                    }
                    entry.close_generation = entry.close_generation.saturating_add(1);
                    let (sender, receiver) = watch::channel(None);
                    entry.in_flight = Some(sender.clone());
                    let sessions = self.sessions.clone();
                    let id = id.clone();
                    let generation = entry.close_generation;
                    let close = entry.close.clone();
                    let limit = self.event_limit;
                    tokio::spawn(async move {
                        let result = close_channel(close, deadline).await;
                        let update = tokio::time::timeout_at(
                            deadline,
                            finish_state(
                                sessions.clone(),
                                &id,
                                generation,
                                SessionState::Closed,
                                result,
                                limit,
                            ),
                        )
                        .await;
                        let final_result = match update {
                            Ok(Ok(())) => result,
                            Ok(Err(e)) => Err(e),
                            Err(_) => Err(SessionError::Timeout),
                        };
                        sender.send_replace(Some(final_result));
                        if update.is_err() {
                            let _ = finish_state(
                                sessions,
                                &id,
                                generation,
                                SessionState::Closed,
                                result,
                                limit,
                            )
                            .await;
                        }
                    });
                    closes.push(receiver);
                }
            }
            closes
        };
        let mut result = Ok(());
        for mut receiver in closes {
            if receiver.borrow().is_none() {
                if tokio::time::timeout_at(deadline, receiver.changed())
                    .await
                    .is_err()
                {
                    return Err(SessionError::Timeout);
                }
            }
            let outcome = *receiver.borrow();
            match outcome {
                Some(Ok(())) => {}
                Some(Err(e)) => result = Err(e),
                None => result = Err(SessionError::Transport),
            }
        }
        result
    }
}

async fn finish_state(
    sessions: Arc<Mutex<HashMap<String, Entry>>>,
    id: &str,
    generation: u64,
    success: SessionState,
    result: Result<(), SessionError>,
    limit: usize,
) -> Result<(), SessionError> {
    let mut sessions = sessions.lock().await;
    let entry = sessions.get_mut(id).ok_or(SessionError::NotFound)?;
    if generation == entry.close_generation {
        let next = if result.is_ok() {
            success
        } else {
            SessionState::CloseUnknown
        };
        if entry.sequence >= MAX_SAFE_SEQUENCE {
            // A wire sequence cannot advance, but resource state must converge
            // and release/retry must remain possible; callers resync on error.
            entry.state = next;
            entry.in_flight = None;
            return Err(SessionError::SequenceExhausted);
        }
        entry.sequence += 1;
        if entry.events.len() == limit {
            entry.events.pop_front();
        }
        entry.events.push_back(SessionEvent {
            session_id: id.into(),
            sequence: entry.sequence,
            payload: SessionEventPayload::State { state: next },
        });
        entry.state = next;
        entry.in_flight = None;
    }
    Ok(())
}

async fn close_channel(
    close: Arc<Mutex<CloseState>>,
    deadline: tokio::time::Instant,
) -> Result<(), SessionError> {
    let mut close = tokio::time::timeout_at(deadline, close.lock())
        .await
        .map_err(|_| SessionError::Timeout)?;
    if let Some(result) = close.result {
        return result;
    }
    let channel = close
        .channel
        .as_ref()
        .ok_or(SessionError::InvalidState)?
        .clone();
    let result = match tokio::time::timeout_at(deadline, channel.close()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(SessionError::Transport),
        Err(_) => Err(SessionError::Timeout),
    };
    if result.is_ok() {
        close.channel.take();
        close.result = Some(result);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestChannel(std::sync::atomic::AtomicUsize);
    struct DelayedChannel {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl TerminalChannel for DelayedChannel {
        fn read<'a>(&'a self) -> coshell_drivers::ssh::SshFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Ok(None) })
        }
        fn write<'a>(&'a self, _: &'a [u8]) -> coshell_drivers::ssh::SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn resize<'a>(
            &'a self,
            _: coshell_drivers::ssh::TerminalSize,
        ) -> coshell_drivers::ssh::SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self) -> coshell_drivers::ssh::SshFuture<'a, ()> {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.entered.notify_one();
                self.release.notified().await;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn late_state_update_converges_to_actual_close_result_for_close_and_shutdown() {
        for shutdown in [false, true] {
            let supervisor = Arc::new(
                SessionSupervisor::with_limits(1, 4, 8, Duration::from_millis(30)).unwrap(),
            );
            let channel = Arc::new(DelayedChannel {
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                calls: std::sync::atomic::AtomicUsize::new(0),
            });
            supervisor
                .register("a", "c", false, channel.clone())
                .await
                .unwrap();
            let task = {
                let supervisor = supervisor.clone();
                tokio::spawn(async move {
                    if shutdown {
                        supervisor.shutdown().await
                    } else {
                        supervisor.close("a").await
                    }
                })
            };
            channel.entered.notified().await;
            let lock = supervisor.sessions.lock().await;
            channel.release.notify_one();
            assert_eq!(task.await.unwrap(), Err(SessionError::Timeout));
            drop(lock);
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if supervisor.state("a").await.unwrap() == SessionState::Closed {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(channel.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            supervisor.release("a").await.unwrap();
        }
    }
    impl TerminalChannel for TestChannel {
        fn read<'a>(&'a self) -> coshell_drivers::ssh::SshFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Ok(None) })
        }
        fn write<'a>(&'a self, _: &'a [u8]) -> coshell_drivers::ssh::SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn resize<'a>(
            &'a self,
            _: coshell_drivers::ssh::TerminalSize,
        ) -> coshell_drivers::ssh::SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self) -> coshell_drivers::ssh::SshFuture<'a, ()> {
            Box::pin(async move {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn exhausted_sequence_closes_real_entry_and_other_shutdown_entries() {
        let supervisor = SessionSupervisor::with_limits(2, 4, 8, Duration::from_secs(1)).unwrap();
        let a = Arc::new(TestChannel(std::sync::atomic::AtomicUsize::new(0)));
        let b = Arc::new(TestChannel(std::sync::atomic::AtomicUsize::new(0)));
        supervisor
            .register("a", "c", false, a.clone())
            .await
            .unwrap();
        supervisor
            .register("b", "c", false, b.clone())
            .await
            .unwrap();
        supervisor
            .sessions
            .lock()
            .await
            .get_mut("a")
            .unwrap()
            .sequence = MAX_SAFE_SEQUENCE;
        assert_eq!(
            supervisor.shutdown().await,
            Err(SessionError::SequenceExhausted)
        );
        assert_eq!(a.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(b.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(supervisor.state("a").await.unwrap(), SessionState::Closed);
    }

    #[tokio::test]
    async fn exhausted_sequence_does_not_prevent_single_close() {
        let supervisor = SessionSupervisor::with_limits(1, 4, 8, Duration::from_secs(1)).unwrap();
        let channel = Arc::new(TestChannel(std::sync::atomic::AtomicUsize::new(0)));
        supervisor
            .register("a", "c", false, channel.clone())
            .await
            .unwrap();
        supervisor
            .sessions
            .lock()
            .await
            .get_mut("a")
            .unwrap()
            .sequence = MAX_SAFE_SEQUENCE;
        assert_eq!(
            supervisor.close("a").await,
            Err(SessionError::SequenceExhausted)
        );
        assert_eq!(channel.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            supervisor.resync("a").await.unwrap().state,
            SessionState::Closed
        );
        supervisor.release("a").await.unwrap();
    }

    #[tokio::test]
    async fn exhausted_sequence_does_not_prevent_final_state_or_release() {
        let (sender, _) = watch::channel(None);
        let mut entries = HashMap::new();
        entries.insert(
            "s".to_owned(),
            Entry {
                state: SessionState::Closing,
                sequence: MAX_SAFE_SEQUENCE,
                events: VecDeque::new(),
                scrollback: VecDeque::new(),
                close: Arc::new(Mutex::new(CloseState {
                    channel: None,
                    result: Some(Ok(())),
                })),
                close_generation: 1,
                in_flight: Some(sender),
            },
        );
        let entries = Arc::new(Mutex::new(entries));
        let result = finish_state(entries.clone(), "s", 1, SessionState::Closed, Ok(()), 8).await;
        assert_eq!(result, Err(SessionError::SequenceExhausted));
        let entries = entries.lock().await;
        assert_eq!(entries["s"].state, SessionState::Closed);
        assert!(entries["s"].in_flight.is_none());
    }
}
