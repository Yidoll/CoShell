//! Explicit rollout gates for shell-client stages.
//!
//! A gate permits an actual operation to run; it never implements that operation
//! or asserts that it is available. Entry points must call the gate before any
//! side effect. All stages are disabled until explicitly configured by the host.

use crate::error_codes;
use coshell_types::ErrorEnvelope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Connections,
    TerminalSessions,
    ServerOverview,
    CommandGuard,
    CommandHistory,
    WebDavBackup,
}

impl Feature {
    pub const ALL: [Self; 6] = [
        Self::Connections,
        Self::TerminalSessions,
        Self::ServerOverview,
        Self::CommandGuard,
        Self::CommandHistory,
        Self::WebDavBackup,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Connections => 0,
            Self::TerminalSessions => 1,
            Self::ServerOverview => 2,
            Self::CommandGuard => 3,
            Self::CommandHistory => 4,
            Self::WebDavBackup => 5,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FeatureFlags {
    enabled: [bool; 6],
}

impl FeatureFlags {
    /// Return a new configuration with a single stage enabled.
    pub fn with_enabled(mut self, feature: Feature) -> Self {
        self.enabled[feature.index()] = true;
        self
    }

    pub fn is_enabled(&self, feature: Feature) -> bool {
        self.enabled[feature.index()]
    }

    /// Call the supplied operation only when its gate is enabled.
    pub fn run_if_enabled<T>(
        &self,
        feature: Feature,
        operation_id: Option<&str>,
        operation: impl FnOnce() -> Result<T, ErrorEnvelope>,
    ) -> Result<T, ErrorEnvelope> {
        if !self.is_enabled(feature) {
            return Err(error_codes::unsupported(operation_id));
        }
        operation()
    }
}
