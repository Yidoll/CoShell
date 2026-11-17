pub fn capability() -> String {
    let runtime = coshell_platform::runtime_profile();
    format!(
        "AI provider boundary initialized for {}-{}",
        runtime.operating_system, runtime.architecture
    )
}
