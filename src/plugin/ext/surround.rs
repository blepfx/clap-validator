use crate::plugin::{ext::Extension, instance::Plugin};
use clap_sys::ext::surround::{CLAP_EXT_SURROUND, CLAP_EXT_SURROUND_COMPAT, clap_plugin_surround};
use std::{ffi::CStr, ptr::NonNull};

#[allow(unused)]
pub struct Surround<'a> {
    plugin: &'a Plugin<'a>,
    surround: NonNull<clap_plugin_surround>,
}

impl<'a> Extension<&'a Plugin<'a>> for Surround<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_SURROUND, CLAP_EXT_SURROUND_COMPAT];

    type Struct = clap_plugin_surround;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            surround: extension_struct,
        }
    }
}
