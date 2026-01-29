use crate::plugin::{ext::Extension, instance::Plugin};
use clap_sys::ext::audio_ports_activation::*;
use std::{ffi::CStr, ptr::NonNull};

/// Abstraction for the `audio-ports-activation` extension covering the main thread functionality.
pub struct AudioPortsActivation<'a> {
    plugin: &'a Plugin<'a>,
    audio_ports_activation: NonNull<clap_plugin_audio_ports_activation>,
}

impl<'a> Extension<&'a Plugin<'a>> for AudioPortsActivation<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_AUDIO_PORTS_ACTIVATION, CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT];

    type Struct = clap_plugin_audio_ports_activation;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            audio_ports_activation: extension_struct,
        }
    }
}
