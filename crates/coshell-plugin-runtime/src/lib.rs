use coshell_types::ComponentStatus;

pub fn component_status() -> ComponentStatus {
    let runtime = coshell_platform::runtime_profile();
    ComponentStatus::ready(
        "plugins",
        format!(
            "plugin host initialized for {}-{}",
            runtime.operating_system, runtime.architecture
        ),
    )
}
