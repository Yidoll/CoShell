use serde::{Deserialize, Serialize};

pub mod data_dir;
pub mod secret_store;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfile {
    pub operating_system: &'static str,
    pub architecture: &'static str,
}

pub fn runtime_profile() -> RuntimeProfile {
    RuntimeProfile {
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
    }
}

pub fn capability() -> &'static str {
    "process, path, download, and host primitives"
}
