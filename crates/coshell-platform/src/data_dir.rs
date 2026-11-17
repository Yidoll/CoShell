//! Determine the user data directory without creating it or accessing secrets.
use std::{
    fmt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPlatform {
    Windows,
    MacOs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataDirError {
    MissingBase,
    InvalidBase,
    UnsupportedPlatform,
}

impl fmt::Display for DataDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DataDirError {}

pub fn resolve_data_dir(
    platform: HostPlatform,
    user_base: Option<&Path>,
) -> Result<PathBuf, DataDirError> {
    let base = user_base.ok_or(DataDirError::MissingBase)?;
    if !base.is_absolute() {
        return Err(DataDirError::InvalidBase);
    }
    Ok(match platform {
        HostPlatform::Windows => base.join("CoShell"),
        HostPlatform::MacOs => base
            .join("Library")
            .join("Application Support")
            .join("CoShell"),
    })
}

pub fn default_data_dir() -> Result<PathBuf, DataDirError> {
    #[cfg(target_os = "windows")]
    {
        return resolve_data_dir(
            HostPlatform::Windows,
            std::env::var_os("LOCALAPPDATA").as_deref().map(Path::new),
        );
    }
    #[cfg(target_os = "macos")]
    {
        return resolve_data_dir(
            HostPlatform::MacOs,
            std::env::var_os("HOME").as_deref().map(Path::new),
        );
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Err(DataDirError::UnsupportedPlatform)
    }
}
