//! Contains functions for loading and interacting with CLAP plugins.

pub mod ext;
pub mod host;
pub mod instance;
pub mod library;
pub mod preset_discovery;

/// Used for asserting that the plugin is in the correct state when calling a function. Hard panics
/// if this is not the case. This is used to ensure the validator's correctness.
///
/// Requires a `.status()` method to exist on `$self`.
macro_rules! assert_plugin_state {
    ($self:expr, state == $expected:expr) => {
        let status = $self.status();
        if status != $expected {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?} != \
                 {:?}). This is a bug in the validator.",
                status, $expected
            )
        }
    };

    ($self:expr, state != $expected:expr) => {
        let status = $self.status();
        if status == $expected {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?} != \
                 {:?}). This is a bug in the validator.",
                status, $expected
            )
        }
    };

    ($self:expr, state < $expected:expr) => {
        let status = $self.status();
        if status >= $expected {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?} >= \
                 {:?}). This is a bug in the validator.",
                status, $expected
            )
        }
    };

    ($self:expr, state >= $expected:expr) => {
        let status = $self.status();
        if status < $expected {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?} <= \
                 {:?}). This is a bug in the validator.",
                status, $expected
            )
        }
    };
}

pub(crate) use assert_plugin_state;
