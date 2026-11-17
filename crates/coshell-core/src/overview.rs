//! Bounded per-connection single-flight overview refresh.
use crate::feature_flags::{Feature, FeatureFlags};
use coshell_drivers::ssh::{Cancellation, SshError};
use coshell_types::{ErrorEnvelope, OperationOutcome, ServerOverviewSnapshot};
use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{watch, Mutex};

pub type OverviewFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ServerOverviewSnapshot, SshError>> + Send + 'a>>;
pub type OverviewError = ErrorEnvelope;
/// Open an independent exec per call. Implementations must never block a
/// runtime thread: Tokio cannot preempt synchronous blocking work. Cancellation
/// must start cleanup; dropping the future must synchronously initiate release.
pub trait Collector: Send + Sync + 'static {
    fn collect<'a>(&'a self, connection_id: &'a str, cancel: Cancellation) -> OverviewFuture<'a>;
}

#[derive(Clone)]
enum ResultState {
    Running,
    Complete(Result<Arc<ServerOverviewSnapshot>, Failure>),
}
#[derive(Clone, Copy)]
enum Failure {
    Cancelled,
    Timeout,
    Transport,
}
struct Flight {
    generation: u64,
    cancel: Cancellation,
    waiters: HashMap<String, watch::Sender<ResultState>>,
    stopping: bool,
}
#[derive(Default)]
struct State {
    flights: HashMap<String, Flight>,
    cache: HashMap<String, ServerOverviewSnapshot>,
    receipts: HashMap<String, (String, ResultState)>,
    order: VecDeque<String>,
    generation: u64,
}
impl State {
    fn remember(&mut self, op: String, connection: String, result: ResultState) {
        if !self.receipts.contains_key(&op) {
            self.order.push_back(op.clone());
        }
        self.receipts.insert(op, (connection, result));
        while self.order.len() > 256 {
            if let Some(old) = self.order.pop_front() {
                self.receipts.remove(&old);
            }
        }
    }
}
pub struct RefreshHandle {
    operation_id: String,
    receiver: watch::Receiver<ResultState>,
}
impl RefreshHandle {
    pub async fn wait(mut self) -> Result<ServerOverviewSnapshot, OverviewError> {
        loop {
            let state = self.receiver.borrow_and_update().clone();
            if let ResultState::Complete(result) = state {
                return result
                    .map(|snapshot| (*snapshot).clone())
                    .map_err(|e| error(e, &self.operation_id));
            }
            if self.receiver.changed().await.is_err() {
                return Err(error(Failure::Transport, &self.operation_id));
            }
        }
    }
}
pub struct OverviewCoordinator<C: Collector> {
    flags: FeatureFlags,
    collector: Arc<C>,
    timeout: Duration,
    state: Arc<Mutex<State>>,
}
impl<C: Collector> OverviewCoordinator<C> {
    pub fn new(flags: FeatureFlags, collector: Arc<C>, timeout: Duration) -> Self {
        Self {
            flags,
            collector,
            timeout,
            state: Arc::new(Mutex::new(State::default())),
        }
    }
    pub async fn refresh(
        &self,
        connection: &str,
        operation: &str,
    ) -> Result<RefreshHandle, OverviewError> {
        if !self.flags.is_enabled(Feature::ServerOverview) {
            return Err(crate::error_codes::unsupported(Some(operation)));
        }
        if !valid_id(connection) || !valid_id(operation) || self.timeout.is_zero() {
            return Err(invalid(operation));
        }
        let mut state = self.state.lock().await;
        if let Some((existing, result)) = state.receipts.get(operation) {
            if existing != connection {
                return Err(invalid(operation));
            }
            let (_, receiver) = watch::channel(result.clone());
            return Ok(RefreshHandle {
                operation_id: operation.into(),
                receiver,
            });
        }
        if state
            .flights
            .iter()
            .any(|(id, f)| id != connection && f.waiters.contains_key(operation))
        {
            return Err(invalid(operation));
        }
        if let Some(flight) = state.flights.get_mut(connection) {
            if flight.stopping {
                return Err(stopping(operation));
            }
            if let Some(sender) = flight.waiters.get(operation) {
                return Ok(RefreshHandle {
                    operation_id: operation.into(),
                    receiver: sender.subscribe(),
                });
            }
            if flight.waiters.len() >= 256 {
                return Err(capacity(operation));
            }
            let (sender, receiver) = watch::channel(ResultState::Running);
            flight.waiters.insert(operation.into(), sender);
            return Ok(RefreshHandle {
                operation_id: operation.into(),
                receiver,
            });
        }
        let reserved = state
            .flights
            .keys()
            .filter(|id| !state.cache.contains_key(*id))
            .count();
        if state.flights.len() >= 64
            || (!state.cache.contains_key(connection) && state.cache.len() + reserved >= 256)
        {
            return Err(capacity(operation));
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| capacity(operation))?;
        let generation = state.generation;
        let cancel = Cancellation::new();
        let (sender, receiver) = watch::channel(ResultState::Running);
        state.flights.insert(
            connection.into(),
            Flight {
                generation,
                cancel: cancel.clone(),
                waiters: HashMap::from([(operation.into(), sender)]),
                stopping: false,
            },
        );
        let collector = self.collector.clone();
        let state = self.state.clone();
        let id = connection.to_owned();
        let timeout = self.timeout;
        tokio::spawn(async move {
            let worker_id = id.clone();
            let worker_cancel = cancel.clone();
            let mut worker =
                tokio::spawn(async move { collector.collect(&worker_id, worker_cancel).await });
            // The nested Result distinguishes outer cancellation/timeout from a
            // completed worker that itself returns Cancelled or Timeout.
            let outer = coshell_drivers::ssh::run_cancellable(timeout, &cancel, async {
                Ok((&mut worker).await)
            })
            .await;
            let result = match outer {
                Ok(joined) => joined
                    .unwrap_or(Err(SshError::Transport))
                    .map_err(map_failure),
                Err(err) => {
                    cancel.cancel();
                    let failure = map_failure(err);
                    {
                        let mut state = state.lock().await;
                        let waiters = if let Some(flight) = state
                            .flights
                            .get_mut(&id)
                            .filter(|f| f.generation == generation)
                        {
                            flight.stopping = true;
                            std::mem::take(&mut flight.waiters)
                        } else {
                            HashMap::new()
                        };
                        for (op, sender) in waiters {
                            let result = ResultState::Complete(Err(failure));
                            sender.send_replace(result.clone());
                            state.remember(op, id.clone(), result);
                        }
                    }
                    // Give cooperative channel closure a bounded window. On expiry,
                    // abort and await the worker so its future is dropped before
                    // another collection can start for this connection.
                    if tokio::time::timeout(Duration::from_millis(100), &mut worker)
                        .await
                        .is_err()
                    {
                        worker.abort();
                        let _ = worker.await;
                    }
                    let mut state = state.lock().await;
                    if state
                        .flights
                        .get(&id)
                        .is_some_and(|f| f.generation == generation)
                    {
                        if let Some(flight) = state.flights.remove(&id) {
                            for (op, _) in flight.waiters {
                                state.remember(op, id.clone(), ResultState::Complete(Err(failure)));
                            }
                        }
                    }
                    return;
                }
            };
            let mut state = state.lock().await;
            if state
                .flights
                .get(&id)
                .is_some_and(|f| f.generation == generation)
            {
                let flight = state.flights.remove(&id).expect("flight checked");
                if flight.stopping {
                    return;
                }
                let result = match result {
                    Ok(snapshot)
                        if snapshot.connection_id != id || !bounded_snapshot(&snapshot) =>
                    {
                        Err(Failure::Transport)
                    }
                    Ok(snapshot) => Ok(Arc::new(snapshot)),
                    Err(error) => Err(error),
                };
                if let Ok(snapshot) = &result {
                    state.cache.insert(id.clone(), (**snapshot).clone());
                }
                for (op, sender) in flight.waiters {
                    let completed = ResultState::Complete(result.clone());
                    sender.send_replace(completed.clone());
                    state.remember(op, id.clone(), completed);
                }
            }
        });
        Ok(RefreshHandle {
            operation_id: operation.into(),
            receiver,
        })
    }
    pub async fn cancel(&self, connection: &str, operation: &str) -> Result<(), OverviewError> {
        if !valid_id(connection) || !valid_id(operation) {
            return Err(invalid(operation));
        }
        let mut state = self.state.lock().await;
        if state
            .receipts
            .get(operation)
            .is_some_and(|(id, _)| id != connection)
            || state
                .flights
                .iter()
                .any(|(id, flight)| id != connection && flight.waiters.contains_key(operation))
        {
            return Err(invalid(operation));
        }
        let mut cancelled = None;
        if let Some(flight) = state.flights.get_mut(connection) {
            if let Some(sender) = flight.waiters.remove(operation) {
                let result = ResultState::Complete(Err(Failure::Cancelled));
                sender.send_replace(result.clone());
                cancelled = Some(result);
                if flight.waiters.is_empty() {
                    flight.stopping = true;
                    flight.cancel.cancel();
                }
            }
        }
        if let Some(result) = cancelled {
            state.remember(operation.into(), connection.into(), result);
        }
        Ok(())
    }
    pub async fn invalidate(&self, connection: &str) {
        let mut state = self.state.lock().await;
        state.cache.remove(connection);
        let cancelled = if let Some(flight) = state.flights.get_mut(connection) {
            flight.stopping = true;
            flight.cancel.cancel();
            std::mem::take(&mut flight.waiters)
        } else {
            HashMap::new()
        };
        for (op, sender) in cancelled {
            let result = ResultState::Complete(Err(Failure::Cancelled));
            sender.send_replace(result.clone());
            state.remember(op, connection.into(), result);
        }
        // Preserve bounded tombstones so old operation IDs cannot restart or
        // replay snapshots from before this connection was invalidated.
        for (id, result) in state.receipts.values_mut() {
            if id == connection {
                *result = ResultState::Complete(Err(Failure::Cancelled));
            }
        }
    }
    pub async fn cached(&self, connection: &str) -> Option<ServerOverviewSnapshot> {
        self.state.lock().await.cache.get(connection).cloned()
    }
}
fn map_failure(e: SshError) -> Failure {
    match e {
        SshError::Cancelled => Failure::Cancelled,
        SshError::Timeout => Failure::Timeout,
        _ => Failure::Transport,
    }
}
fn bounded_snapshot(s: &ServerOverviewSnapshot) -> bool {
    let text_ok = |v: &Option<String>| {
        v.as_ref()
            .is_none_or(|s| s.len() <= 256 && !s.chars().any(char::is_control))
    };
    let metric_ok = |v: &Option<coshell_types::Metric>| {
        v.as_ref().is_none_or(|m| {
            m.unit.len() <= 32
                && m.unavailable_reason.as_ref().is_none_or(|r| r.len() <= 256)
                && m.value.is_none_or(f64::is_finite)
        })
    };
    s.connection_id.len() <= 128
        && s.collected_at.len() <= 64
        && [
            &s.hostname,
            &s.operating_system,
            &s.kernel,
            &s.architecture,
            &s.boot_time,
        ]
        .into_iter()
        .all(text_ok)
        && [
            &s.uptime,
            &s.cpu_usage,
            &s.logical_cores,
            &s.memory_total,
            &s.memory_used,
            &s.load,
            &s.disk_total,
            &s.disk_used,
            &s.swap_total,
            &s.swap_used,
        ]
        .into_iter()
        .all(metric_ok)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn error(f: Failure, operation: &str) -> ErrorEnvelope {
    let (code, key, outcome, retry) = match f {
        Failure::Cancelled => (
            "OVERVIEW_CANCELLED",
            "errors.overview.cancelled",
            OperationOutcome::Cancelled,
            false,
        ),
        Failure::Timeout => (
            "OVERVIEW_TIMEOUT",
            "errors.overview.timeout",
            OperationOutcome::Failed,
            true,
        ),
        Failure::Transport => (
            "OVERVIEW_FAILED",
            "errors.overview.failed",
            OperationOutcome::Failed,
            true,
        ),
    };
    ErrorEnvelope {
        code: code.into(),
        message_key: key.into(),
        safe_detail: None,
        operation_id: Some(operation.into()),
        outcome,
        retryable: retry,
    }
}
fn invalid(op: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: "OVERVIEW_INVALID_INPUT".into(),
        message_key: "errors.overview.invalid".into(),
        safe_detail: None,
        operation_id: Some(op.into()),
        outcome: OperationOutcome::NotStarted,
        retryable: false,
    }
}
fn capacity(op: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: "OVERVIEW_CAPACITY".into(),
        message_key: "errors.overview.capacity".into(),
        safe_detail: None,
        operation_id: Some(op.into()),
        outcome: OperationOutcome::NotStarted,
        retryable: true,
    }
}
fn stopping(op: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: "OVERVIEW_STOPPING".into(),
        message_key: "errors.overview.stopping".into(),
        safe_detail: None,
        operation_id: Some(op.into()),
        outcome: OperationOutcome::NotStarted,
        retryable: true,
    }
}
