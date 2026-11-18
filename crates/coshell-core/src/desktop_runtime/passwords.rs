use super::{
    error, id, ConnectionRecord, DesktopRuntime, Duration, HashMap, Mutex, Ordering, Secret,
};
use crate::ssh_password_store::{self as store, UNAVAILABLE};
use std::{sync::Arc, time::Instant};

const OFFER_TTL: Duration = Duration::from_secs(90);
const RESULT_TTL: Duration = Duration::from_secs(300);

struct Offer {
    session: String,
    record: ConnectionRecord,
    password: Secret,
    deadline: Instant,
}

struct Decision {
    operation: String,
    save: bool,
    result: Result<(), String>,
    deadline: Instant,
}

#[derive(Default)]
struct Offers {
    pending: HashMap<String, Offer>,
    decided: HashMap<String, Decision>,
}

#[derive(Default)]
pub(super) struct PasswordOffers(Arc<Mutex<Offers>>);

impl PasswordOffers {
    pub(super) fn discard_session(&self, session: &str) {
        if let Ok(mut offers) = self.0.lock() {
            offers.pending.retain(|_, offer| offer.session != session);
        }
    }

    pub(super) fn discard_connection(&self, connection: &str) {
        if let Ok(mut offers) = self.0.lock() {
            offers
                .pending
                .retain(|_, offer| offer.record.id != connection);
        }
    }
}

pub(super) fn same_target(a: &ConnectionRecord, b: &ConnectionRecord) -> bool {
    a.id == b.id
        && a.host == b.host
        && a.port == b.port
        && a.username == b.username
        && a.authentication == b.authentication
}

impl DesktopRuntime {
    pub(super) fn saved_password(
        &self,
        record: &ConnectionRecord,
    ) -> Result<Option<Secret>, String> {
        if record.authentication != "password" {
            return Ok(None);
        }
        let path = self.path.as_deref().ok_or_else(|| error(UNAVAILABLE))?;
        let platform = self.webdav_store().map_err(|_| error(UNAVAILABLE))?;
        store::load(path, &platform, &record.id).map_err(error)
    }

    pub async fn ssh_password_state(&self, connection_id: &str) -> Result<&'static str, String> {
        let _state = self.state.lock().await;
        let record = self
            .service
            .get(connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))?;
        Ok(match self.saved_password(&record) {
            Ok(Some(_)) => "available",
            Ok(None) => "missing",
            Err(_) => "unavailable",
        })
    }

    pub(super) fn offer_password(
        &self,
        session: &str,
        record: &ConnectionRecord,
        password: &[u8],
    ) -> Option<String> {
        let path = self.path.as_deref()?;
        self.webdav_store().ok()?;
        if !store::valid_password(password) {
            return None;
        }
        // A manual retry can replace an existing but unavailable or rejected password.
        if matches!(self.saved_password(record), Ok(None)) && store::declined(path, record).ok()? {
            return None;
        }
        let mut offers = self.passwords.0.lock().ok()?;
        let now = Instant::now();
        offers.pending.retain(|_, offer| offer.deadline > now);
        if offers.pending.len() >= 32
            || offers
                .pending
                .values()
                .any(|offer| same_target(&offer.record, record))
        {
            return None;
        }
        let token = id().ok()?;
        offers.pending.insert(
            token.clone(),
            Offer {
                session: session.to_owned(),
                record: record.clone(),
                password: Secret::new(password.to_vec()),
                deadline: now + OFFER_TTL,
            },
        );
        let weak = Arc::downgrade(&self.passwords.0);
        let expiring = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(OFFER_TTL).await;
            if let Some(offers) = weak.upgrade() {
                if let Ok(mut offers) = offers.lock() {
                    offers.pending.remove(&expiring);
                }
            }
        });
        Some(token)
    }

    pub async fn ssh_password_decision(
        &self,
        token: &str,
        operation: &str,
        save: bool,
    ) -> Result<(), String> {
        if operation.is_empty()
            || operation.len() > 128
            || !operation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(error("SSH_PASSWORD_DECISION_INVALID"));
        }
        let state = self.state.lock().await;
        let mut offers = self.passwords.0.lock().map_err(|_| error(UNAVAILABLE))?;
        let now = Instant::now();
        offers.decided.retain(|_, decision| decision.deadline > now);
        if let Some(decision) = offers.decided.get(token) {
            return if decision.operation == operation && decision.save == save {
                decision.result.clone()
            } else {
                Err(error("SSH_PASSWORD_DECISION_INVALID"))
            };
        }
        let offer = offers
            .pending
            .remove(token)
            .filter(|offer| offer.deadline > now)
            .ok_or_else(|| error("SSH_PASSWORD_DECISION_EXPIRED"))?;
        let result = (|| {
            let session = state
                .sessions
                .get(&offer.session)
                .filter(|session| !session.closed.load(Ordering::Acquire))
                .ok_or_else(|| error("SSH_PASSWORD_DECISION_EXPIRED"))?;
            let record = self
                .service
                .get(&session.connection_id)
                .map_err(|e| e.code)?
                .filter(|record| same_target(record, &offer.record))
                .ok_or_else(|| error("SSH_PASSWORD_DECISION_EXPIRED"))?;
            let path = self.path.as_deref().ok_or_else(|| error(UNAVAILABLE))?;
            if save {
                let platform = self.webdav_store().map_err(|_| error(UNAVAILABLE))?;
                store::save(path, &platform, &record.id, operation, &offer.password).map_err(error)
            } else {
                store::decline(path, &record).map_err(|_| error("SSH_PASSWORD_DECISION_FAILED"))
            }
        })();
        if offers.decided.len() >= 1024 {
            if let Some(oldest) = offers
                .decided
                .iter()
                .min_by_key(|(_, decision)| decision.deadline)
                .map(|(token, _)| token.clone())
            {
                offers.decided.remove(&oldest);
            }
        }
        offers.decided.insert(
            token.to_owned(),
            Decision {
                operation: operation.to_owned(),
                save,
                result: result.clone(),
                deadline: now + RESULT_TTL,
            },
        );
        result
    }

    pub(super) fn recover_password_cleanup(&self) {
        if let (Some(path), Ok(platform)) = (self.path.as_deref(), self.webdav_store()) {
            // The committed cleanup journal is retained on failure and retried at startup.
            let _ = crate::ssh_key_store::recover_all(path, &platform);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_flags::{Feature, FeatureFlags};

    #[tokio::test]
    async fn expired_or_unknown_offer_cannot_store_a_password() {
        let runtime = DesktopRuntime::from_connection(
            rusqlite::Connection::open_in_memory().unwrap(),
            FeatureFlags::default().with_enabled(Feature::Connections),
        )
        .unwrap();
        let record = ConnectionRecord {
            id: "connection".into(),
            name: "name".into(),
            host: "host".into(),
            port: 22,
            username: "user".into(),
            authentication: "password".into(),
            group_id: None,
            notes: String::new(),
            production: false,
            always_confirm: false,
            version: 0,
        };
        runtime.passwords.0.lock().unwrap().pending.insert(
            "expired".into(),
            Offer {
                session: "closed".into(),
                record,
                password: Secret::new(b"fixture".to_vec()),
                deadline: Instant::now() - Duration::from_secs(1),
            },
        );
        for token in ["expired", "unissued"] {
            assert_eq!(
                runtime
                    .ssh_password_decision(token, "op", true)
                    .await
                    .unwrap_err(),
                "SSH_PASSWORD_DECISION_EXPIRED"
            );
        }
        assert!(runtime.passwords.0.lock().unwrap().pending.is_empty());
    }
}
