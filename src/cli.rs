mod config;
mod log;
mod panic;
mod print;
pub mod sandbox;
pub mod tracing;

pub use config::*;
pub use log::*;
pub use panic::*;
pub use print::*;

/// A temporary directory used by the validator. This is cleared when launching the validator.
pub fn validator_temp_dir() -> std::path::PathBuf {
    /// [`std::env::temp_dir`], but taking `XDG_RUNTIME_DIR` on Linux into account.
    fn temp_dir() -> std::path::PathBuf {
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)
            && dir.is_dir()
        {
            return dir;
        }

        std::env::temp_dir()
    }

    temp_dir().join("clap-validator")
}
