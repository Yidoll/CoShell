use coshell_platform::data_dir::{resolve_data_dir, DataDirError, HostPlatform};
use std::path::Path;

#[test]
fn resolves_native_user_data_paths_without_creating_files() {
    let base = std::env::temp_dir();
    let windows = resolve_data_dir(HostPlatform::Windows, Some(&base)).unwrap();
    assert_eq!(windows, base.join("CoShell"));
    let mac = resolve_data_dir(HostPlatform::MacOs, Some(&base)).unwrap();
    assert_eq!(mac, base.join("Library/Application Support/CoShell"));
}

#[test]
fn rejects_missing_or_relative_user_base() {
    assert_eq!(
        resolve_data_dir(HostPlatform::MacOs, None),
        Err(DataDirError::MissingBase)
    );
    assert_eq!(
        resolve_data_dir(HostPlatform::Windows, Some(Path::new("relative"))),
        Err(DataDirError::InvalidBase)
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_default_reads_localappdata() {
    use coshell_platform::data_dir::default_data_dir;
    let base = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA required on Windows");
    assert_eq!(
        default_data_dir().unwrap(),
        std::path::PathBuf::from(base).join("CoShell")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_default_reads_home() {
    use coshell_platform::data_dir::default_data_dir;
    let base = std::env::var_os("HOME").expect("HOME required on macOS");
    assert_eq!(
        default_data_dir().unwrap(),
        std::path::PathBuf::from(base).join("Library/Application Support/CoShell")
    );
}
