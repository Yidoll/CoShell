//! In-memory admission primitive only. This does not send commands or protect
//! a transport. Integration must obtain authoritative connection/session state
//! and atomically revalidate it with the actual write; caller-provided state is
//! not a trust boundary. Restart drops all permits and operation deduplication.
use coshell_shell::{assess, redact, RiskLevel, RULE_VERSION};
use coshell_types::CommandIntent;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_OPERATIONS: usize = 4096;
const MAX_COMMAND: usize = 64 * 1024;
const MAX_ID: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveConnection {
    pub session_id: String,
    pub connection_id: String,
    pub version: u64,
    pub production: bool,
    pub always_confirm: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub level: RiskLevel,
    pub categories: Vec<&'static str>,
    pub rule_version: &'static str,
    pub redacted_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    ConfirmationRequired(Assessment),
    Permitted(Assessment, String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    InvalidInput,
    Conflict,
    Expired,
    InvalidPermit,
    AlreadyConsumed,
    NotPrepared,
    RandomUnavailable,
    Capacity,
    Internal,
}
struct Entry {
    command_hash: [u8; 32],
    live: LiveConnection,
    assessment: Assessment,
    permit: Option<String>,
    consumed: bool,
}
struct Slot {
    deadline: Instant,
    entry: Option<Entry>,
}
pub struct CommandGate {
    ttl: Duration,
    entries: Mutex<HashMap<String, Slot>>,
}
impl CommandGate {
    pub fn new(ttl: Duration) -> Result<Self, GateError> {
        if ttl.is_zero() || ttl > Duration::from_secs(300) {
            return Err(GateError::InvalidInput);
        }
        Ok(Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        })
    }
    fn binding(intent: &CommandIntent, live: &LiveConnection) -> Result<[u8; 32], GateError> {
        if [
            &intent.operation_id,
            &intent.session_id,
            &intent.connection_id,
        ]
        .iter()
        .any(|v| v.is_empty() || v.len() > MAX_ID)
            || intent.command.is_empty()
            || intent.command.len() > MAX_COMMAND
        {
            return Err(GateError::InvalidInput);
        }
        if intent.session_id != live.session_id
            || intent.connection_id != live.connection_id
            || intent.connection_version != live.version
        {
            return Err(GateError::Conflict);
        }
        Ok(Sha256::digest(intent.command.as_bytes()).into())
    }
    fn check(entry: &Entry, hash: [u8; 32], live: &LiveConnection) -> Result<(), GateError> {
        if entry.command_hash != hash
            || &entry.live != live
            || entry.assessment.rule_version != RULE_VERSION
        {
            return Err(GateError::Conflict);
        }
        if entry.consumed {
            return Err(GateError::AlreadyConsumed);
        }
        Ok(())
    }
    fn token() -> Result<String, GateError> {
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| GateError::RandomUnavailable)?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
    /// Reserve an unpredictable operation ID. Expired slots are reclaimed;
    /// an absent ID can never be used to create an operation.
    pub fn issue_operation(&self) -> Result<String, GateError> {
        let mut entries = self.entries.lock().map_err(|_| GateError::Internal)?;
        let now = Instant::now();
        entries.retain(|_, slot| now < slot.deadline);
        if entries.len() >= MAX_OPERATIONS {
            return Err(GateError::Capacity);
        }
        let deadline = now.checked_add(self.ttl).ok_or(GateError::Internal)?;
        loop {
            let id = Self::token()?;
            if !entries.contains_key(&id) {
                entries.insert(
                    id.clone(),
                    Slot {
                        deadline,
                        entry: None,
                    },
                );
                return Ok(id);
            }
        }
    }
    pub fn prepare_current(
        &self,
        intent: &CommandIntent,
        live: &LiveConnection,
    ) -> Result<Admission, GateError> {
        let hash = Self::binding(intent, live)?;
        let mut entries = self.entries.lock().map_err(|_| GateError::Internal)?;
        let slot = entries
            .get_mut(&intent.operation_id)
            .ok_or(GateError::NotPrepared)?;
        if Instant::now() >= slot.deadline {
            return Err(GateError::Expired);
        }
        if let Some(entry) = &slot.entry {
            Self::check(entry, hash, live)?;
            return Ok(match &entry.permit {
                Some(token) => Admission::Permitted(entry.assessment.clone(), token.clone()),
                None => Admission::ConfirmationRequired(entry.assessment.clone()),
            });
        }
        let result = assess(&intent.command);
        let confirmation = (live.production && result.level != RiskLevel::Safe)
            || (live.always_confirm && result.level != RiskLevel::Safe);
        let assessment = Assessment {
            level: result.level,
            categories: result.categories,
            rule_version: result.rule_version,
            redacted_digest: redact(&intent.command).digest,
        };
        let permit = if confirmation {
            None
        } else {
            Some(Self::token()?)
        };
        let response = match &permit {
            Some(token) => Admission::Permitted(assessment.clone(), token.clone()),
            None => Admission::ConfirmationRequired(assessment.clone()),
        };
        slot.entry = Some(Entry {
            command_hash: hash,
            live: live.clone(),
            assessment,
            permit,
            consumed: false,
        });
        Ok(response)
    }
    pub fn confirm_current(
        &self,
        intent: &CommandIntent,
        live: &LiveConnection,
    ) -> Result<String, GateError> {
        let hash = Self::binding(intent, live)?;
        let mut entries = self.entries.lock().map_err(|_| GateError::Internal)?;
        let slot = entries
            .get_mut(&intent.operation_id)
            .ok_or(GateError::NotPrepared)?;
        if Instant::now() >= slot.deadline {
            return Err(GateError::Expired);
        }
        let entry = slot.entry.as_mut().ok_or(GateError::NotPrepared)?;
        Self::check(entry, hash, live)?;
        if entry.permit.is_none() {
            entry.permit = Some(Self::token()?);
        }
        Ok(entry.permit.as_ref().unwrap().clone())
    }
    /// Consume once before attempting a write. Never retry an uncertain write.
    /// This primitive does not itself perform or authorize any transport I/O.
    pub fn consume_current(
        &self,
        intent: &CommandIntent,
        live: &LiveConnection,
        permit: &str,
    ) -> Result<(), GateError> {
        let hash = Self::binding(intent, live)?;
        let mut entries = self.entries.lock().map_err(|_| GateError::Internal)?;
        let slot = entries
            .get_mut(&intent.operation_id)
            .ok_or(GateError::NotPrepared)?;
        if Instant::now() >= slot.deadline {
            return Err(GateError::Expired);
        }
        let entry = slot.entry.as_mut().ok_or(GateError::NotPrepared)?;
        Self::check(entry, hash, live)?;
        if entry.permit.as_deref() != Some(permit) {
            return Err(GateError::InvalidPermit);
        }
        entry.consumed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_gate_recovers_expired_slots_without_reopening_old_ids() {
        let gate = CommandGate::new(Duration::from_secs(30)).unwrap();
        let id = gate.issue_operation().unwrap();
        let live = LiveConnection {
            session_id: "s".into(),
            connection_id: "c".into(),
            version: 1,
            production: true,
            always_confirm: false,
        };
        let intent = CommandIntent {
            session_id: "s".into(),
            connection_id: "c".into(),
            connection_version: 1,
            command: "rm -rf /tmp/x".into(),
            operation_id: id.clone(),
        };
        gate.prepare_current(&intent, &live).unwrap();
        let token = gate.confirm_current(&intent, &live).unwrap();
        gate.consume_current(&intent, &live, &token).unwrap();
        for _ in 1..MAX_OPERATIONS {
            gate.issue_operation().unwrap();
        }
        assert_eq!(gate.issue_operation(), Err(GateError::Capacity));
        {
            let mut entries = gate.entries.lock().unwrap();
            for slot in entries.values_mut() {
                slot.deadline = Instant::now() - Duration::from_secs(1);
            }
        }
        assert_eq!(
            gate.prepare_current(&intent, &live),
            Err(GateError::Expired)
        );
        let fresh = gate.issue_operation().unwrap();
        assert_ne!(fresh, id);
        assert_eq!(
            gate.prepare_current(&intent, &live),
            Err(GateError::NotPrepared)
        );
        assert_eq!(
            gate.consume_current(&intent, &live, &token),
            Err(GateError::NotPrepared)
        );
        let unissued = CommandIntent {
            operation_id: "unissued".into(),
            ..intent
        };
        assert_eq!(
            gate.prepare_current(&unissued, &live),
            Err(GateError::NotPrepared)
        );
        let other = CommandGate::new(Duration::from_secs(30)).unwrap();
        let cross_gate = CommandIntent {
            operation_id: fresh,
            ..unissued
        };
        assert_eq!(
            other.prepare_current(&cross_gate, &live),
            Err(GateError::NotPrepared)
        );
    }
}
