use crate::{
    plugin::{
        ext::Extension,
        instance::{Plugin, PluginStatus},
    },
    util::unsafe_clap_call,
};
use clap_sys::ext::latency::{clap_plugin_latency, CLAP_EXT_LATENCY};
use std::{ffi::CStr, ptr::NonNull};

pub struct Latency<'a> {
    plugin: &'a Plugin<'a>,
    latency: NonNull<clap_plugin_latency>,
}

impl<'a> Extension<&'a Plugin<'a>> for Latency<'a> {
    const EXTENSION_ID: &'static CStr = CLAP_EXT_LATENCY;

    type Struct = clap_plugin_latency;

    fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            latency: extension_struct,
        }
    }
}

impl<'a> Latency<'a> {
    pub fn get(&self) -> u32 {
        assert!(
            self.plugin.status() >= PluginStatus::Activating,
            "The 'latency' extension's 'get' function can only be called while the plugin is \
             activating or active. This is a bug in the validator."
        );

        let latency = self.latency.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe_clap_call! { latency=>get(plugin) }
    }
}
