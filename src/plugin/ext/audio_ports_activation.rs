use crate::{
    plugin::{ext::Extension, instance::Plugin},
    util::clap_call,
};
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

impl<'a> AudioPortsActivation<'a> {
    /// TODO: extra test where we do this while processing
    #[allow(unused)]
    pub fn can_activate_while_processing(&self) -> bool {
        let audio_ports_activation = self.audio_ports_activation.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe {
            clap_call! { audio_ports_activation=>can_activate_while_processing(plugin) }
        }
    }

    /// Activates or deactivates audio ports while inactive.
    pub fn set_active(&mut self, is_input: bool, port_index: u32, is_active: bool, sample_size: u32) -> bool {
        self.plugin.status().assert_inactive();

        let audio_ports_activation = self.audio_ports_activation.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe {
            clap_call! { audio_ports_activation=>set_active(plugin, is_input, port_index, is_active, sample_size) }
        }
    }
}
