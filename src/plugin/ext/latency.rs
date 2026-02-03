use crate::plugin::ext::Extension;
use crate::plugin::instance::{Plugin, PluginStatus};
use crate::plugin::util::clap_call;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use std::ffi::CStr;
use std::ptr::NonNull;

#[allow(unused)]
pub struct Latency<'a> {
    plugin: &'a Plugin<'a>,
    latency: NonNull<clap_plugin_latency>,
}

impl<'a> Extension for Latency<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_LATENCY];

    type Plugin = &'a Plugin<'a>;
    type Struct = clap_plugin_latency;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            latency: extension_struct,
        }
    }
}

impl<'a> Latency<'a> {
    #[allow(unused)]
    #[tracing::instrument(name = "clap_plugin_latency::get", level = 1, skip(self))]
    pub fn get(&self) -> u32 {
        self.plugin.status().assert_is_not(PluginStatus::Deactivated);

        let latency = self.latency.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe {
            clap_call! { latency=>get(plugin) }
        }
    }
}
