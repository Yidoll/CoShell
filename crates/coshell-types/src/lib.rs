use serde::{Deserialize, Serialize};

mod shell_client;
pub use shell_client::*;

/// Stable health state shared by desktop, web, CLI, and MCP transports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ready,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub name: String,
    pub state: HealthState,
    pub detail: String,
}

impl ComponentStatus {
    pub fn ready(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: HealthState::Ready,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub service: String,
    pub version: String,
    pub state: HealthState,
    pub components: Vec<ComponentStatus>,
}
