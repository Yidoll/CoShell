//! Bounded remote-file operations and one-use production confirmations.
//! Callers must obtain session identity and connection version from the runtime.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MAX_FILE: usize = 16 * 1024 * 1024;
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Mutation {
    Mkdir {
        path: String,
    },
    Create {
        path: String,
    },
    Remove {
        path: String,
        directory: bool,
    },
    Rename {
        path: String,
        destination: String,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    Write {
        path: String,
        data: Vec<u8>,
        overwrite: bool,
        expected_digest: Option<String>,
    },
}
impl Mutation {
    pub fn valid(&self) -> bool {
        fn path(p: &str) -> bool {
            p.starts_with('/') && p.len() <= 4096 && !p.contains('\0')
        }
        match self {
            Self::Mkdir { path: p } | Self::Create { path: p } | Self::Remove { path: p, .. } => {
                path(p)
            }
            Self::Rename {
                path: p,
                destination,
            } => path(p) && path(destination) && p != destination,
            Self::Chmod { path: p, mode } => path(p) && *mode <= 0o777,
            Self::Write {
                path: p,
                data,
                overwrite,
                expected_digest,
            } => {
                path(p)
                    && data.len() <= MAX_FILE
                    && (!*overwrite
                        || expected_digest.as_ref().is_some_and(|d| {
                            d.len() == 64 && d.bytes().all(|c| c.is_ascii_hexdigit())
                        }))
            }
        }
    }
}
struct Pending {
    session: String,
    version: u64,
    mutation: Mutation,
    deadline: Instant,
    confirmed: bool,
}
pub struct MutationGate {
    pending: Mutex<HashMap<String, Pending>>,
}
impl Default for MutationGate {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}
impl MutationGate {
    pub fn prepare(
        &self,
        session: &str,
        version: u64,
        mutation: Mutation,
        confirmation_required: bool,
    ) -> Result<String, &'static str> {
        if session.is_empty() || !mutation.valid() {
            return Err("REMOTE_FILES_INVALID");
        };
        let mut entries = self.pending.lock().map_err(|_| "REMOTE_FILES_ERROR")?;
        let now = Instant::now();
        entries.retain(|_, v| v.deadline > now);
        if entries.len() >= 2 {
            return Err("REMOTE_FILES_BUSY");
        };
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).map_err(|_| "REMOTE_FILES_ERROR")?;
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        if entries.contains_key(&token) {
            return Err("REMOTE_FILES_ERROR");
        };
        entries.insert(
            token.clone(),
            Pending {
                session: session.into(),
                version,
                mutation,
                deadline: now + Duration::from_secs(120),
                confirmed: !confirmation_required,
            },
        );
        Ok(token)
    }
    pub fn confirm(&self, session: &str, version: u64, token: &str) -> Result<(), &'static str> {
        let mut entries = self.pending.lock().map_err(|_| "REMOTE_FILES_ERROR")?;
        let entry = entries
            .get_mut(token)
            .ok_or("REMOTE_FILES_CONFIRMATION_INVALID")?;
        if entry.deadline <= Instant::now() || entry.session != session || entry.version != version
        {
            return Err("REMOTE_FILES_CONFIRMATION_INVALID");
        }
        entry.confirmed = true;
        Ok(())
    }
    pub fn consume(
        &self,
        session: &str,
        version: u64,
        token: &str,
    ) -> Result<Mutation, &'static str> {
        let mut entries = self.pending.lock().map_err(|_| "REMOTE_FILES_ERROR")?;
        let item = entries
            .remove(token)
            .ok_or("REMOTE_FILES_CONFIRMATION_INVALID")?;
        if item.deadline <= Instant::now()
            || item.session != session
            || item.version != version
            || !item.confirmed
        {
            return Err("REMOTE_FILES_CONFIRMATION_INVALID");
        };
        Ok(item.mutation)
    }
    pub fn cancel_session(&self, session: &str) {
        if let Ok(mut entries) = self.pending.lock() {
            entries.retain(|_, v| v.session != session);
        }
    }
    pub fn cancel(&self, session: &str, token: &str) {
        if let Ok(mut entries) = self.pending.lock() {
            if entries.get(token).is_some_and(|p| p.session == session) {
                entries.remove(token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permit_is_one_use_and_bound_to_session_and_version() {
        let gate = MutationGate::default();
        let m = Mutation::Remove {
            path: "/tmp/a".into(),
            directory: false,
        };
        let token = gate.prepare("s", 1, m.clone(), true).unwrap();
        assert!(gate.consume("other", 1, &token).is_err());
        assert!(gate.consume("s", 1, &token).is_err());
        let token = gate.prepare("s", 1, m.clone(), true).unwrap();
        assert!(gate.consume("s", 2, &token).is_err());
        let token = gate.prepare("s", 1, m.clone(), true).unwrap();
        assert!(gate.consume("s", 1, &token).is_err());
        let token = gate.prepare("s", 1, m.clone(), true).unwrap();
        gate.confirm("s", 1, &token).unwrap();
        assert!(gate.consume("s", 1, &token).is_ok());
        assert!(gate.consume("s", 1, &token).is_err());
    }
    #[test]
    fn validates_bounds_and_rejects_nul() {
        assert!(!Mutation::Create {
            path: "/tmp/\0x".into()
        }
        .valid());
        assert!(!Mutation::Create {
            path: "relative".into()
        }
        .valid());
        assert!(!Mutation::Write {
            path: "/x".into(),
            data: vec![0; MAX_FILE + 1],
            overwrite: false,
            expected_digest: None,
        }
        .valid());
    }
}
