use crate::debug::record;
use crate::plugin::ext::Extension;
use crate::plugin::instance::Plugin;
use crate::plugin::util::clap_call;
use clap_sys::ext::ambisonic::*;
use std::ffi::CStr;
use std::mem::zeroed;
use std::ptr::NonNull;

pub struct Ambisonic<'a> {
    plugin: &'a Plugin<'a>,
    ambisonic: NonNull<clap_plugin_ambisonic>,
}

impl<'a> Extension for Ambisonic<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_AMBISONIC, CLAP_EXT_AMBISONIC_COMPAT];

    type Plugin = &'a Plugin<'a>;
    type Struct = clap_plugin_ambisonic;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            ambisonic: extension_struct,
        }
    }
}

impl<'a> Ambisonic<'a> {
    #[tracing::instrument(
        name = "clap_plugin_ambisonic::is_config_supported",
        level = 1,
        skip(self),
        fields(result)
    )]
    pub fn is_config_supported(&self, config: &clap_ambisonic_config) -> bool {
        let ambisonic = self.ambisonic.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe { record("result", clap_call! { ambisonic=>is_config_supported(plugin, config) }) }
    }

    #[tracing::instrument(name = "clap_plugin_ambisonic::get_config", level = 1, skip(self), fields(result))]
    pub fn get_config(&self, is_input: bool, port_index: u32) -> Option<clap_ambisonic_config> {
        let ambisonic = self.ambisonic.as_ptr();
        let plugin = self.plugin.as_ptr();

        unsafe {
            let mut config = clap_ambisonic_config { ..zeroed() };
            let result = clap_call! { ambisonic=>get_config(plugin, is_input, port_index, &mut config) };
            if result { Some(record("result", config)) } else { None }
        }
    }
}
