//! Abstractions for the different extensions. The extension `Foo` comes with a `Foo` and a
//! `FooAudioThread` struct. The former contains functions that can be called from the main thread,
//! while the latter contains functions that can be called from the audio thread.

use std::ffi::CStr;
use std::ptr::NonNull;

pub mod ambisonic;
pub mod audio_ports;
pub mod audio_ports_activation;
pub mod audio_ports_config;
pub mod configurable_audio_ports;
pub mod latency;
pub mod note_ports;
pub mod params;
pub mod preset_load;
pub mod state;
pub mod surround;
pub mod tail;
pub mod thread_pool;
pub mod voice_info;

/// An abstraction for a CLAP plugin extension. `P` here is the plugin type. In practice, this is
/// either `Plugin`, `PluginShared` or `PluginAudioThread`. Abstractions for main thread functions will implement
/// this trait for `Plugin`, abstractions for audio thread functions will implement this trait
/// for `PluginAudioThread` and abstractions for thread-safe functions will implement this trait for
/// `PluginShared`.
pub trait Extension<P> {
    /// The list of C-string IDs for the extension.
    const IDS: &'static [&'static CStr];

    /// The type of the C-struct for the extension.
    type Struct;

    /// Construct the extension for the plugin type `P`. This allows the abstraction to be limited
    /// to only work with the main thread `&Plugin` or the audio thread `&PluginAudioThread`.
    ///
    /// # Safety
    /// The extension struct pointer must be a valid pointer to the correct extension struct for
    /// the plugin instance and given `IDS`.
    unsafe fn new(plugin: P, extension_struct: NonNull<Self::Struct>) -> Self;
}
