use coshell_types::ComponentStatus;
use thiserror::Error;

pub mod server_overview;
pub mod ssh;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("driver '{0}' is not installed")]
    NotInstalled(String),
}

pub fn component_status() -> ComponentStatus {
    let runtime = coshell_platform::runtime_profile();
    ComponentStatus::ready(
        "drivers",
        format!(
            "driver registry initialized for {}-{}",
            runtime.operating_system, runtime.architecture
        ),
    )
}

pub fn accepts_read_only_query(sql: &str) -> bool {
    coshell_sql::is_read_only_statement(sql)
}
