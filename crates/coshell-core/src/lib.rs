use coshell_types::{ComponentStatus, HealthReport, HealthState};

pub mod auto_backup;
pub mod backup_inspection;
pub mod backup_snapshot;
pub mod command_intent;
pub mod directory_outbox;
pub mod publication_outbox;
pub mod remote_files;
pub mod restore_secrets;
pub mod saved_commands;

pub mod connections;
pub mod desktop_runtime;
pub mod error_codes;
pub mod feature_flags;
pub mod host_key;
pub mod overview;
pub mod secret_compensation;
pub mod session;
pub mod ssh_key_store;
mod ssh_password_store;
pub mod webdav_config;
pub mod webdav_test;

#[derive(Debug, Clone, Default)]
pub struct RuntimeService;

impl RuntimeService {
    pub fn new() -> Self {
        Self
    }

    pub fn health(&self) -> HealthReport {
        let runtime = coshell_platform::runtime_profile();
        HealthReport {
            service: "coshell".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            state: HealthState::Ready,
            components: vec![
                ComponentStatus::ready("core", "application orchestration initialized"),
                ComponentStatus::ready(
                    "platform",
                    format!(
                        "{}-{}; {}",
                        runtime.operating_system,
                        runtime.architecture,
                        coshell_platform::capability()
                    ),
                ),
                coshell_sql::component_status(),
                coshell_drivers::component_status(),
                coshell_formats::component_status(),
                coshell_plugin_runtime::component_status(),
                ComponentStatus::ready("ai", coshell_ai_provider::capability()),
            ],
        }
    }
}

pub use coshell_types::{
    ComponentStatus as SharedComponentStatus, HealthReport as SharedHealthReport,
    HealthState as SharedHealthState,
};

#[cfg(test)]
mod tests {
    use super::RuntimeService;
    use coshell_types::HealthState;

    #[test]
    fn reports_all_initialized_boundaries() {
        let report = RuntimeService::new().health();
        assert_eq!(report.state, HealthState::Ready);
        assert_eq!(report.components.len(), 7);
    }
}
