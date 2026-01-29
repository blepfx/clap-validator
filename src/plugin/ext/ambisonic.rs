use crate::plugin::{ext::Extension, instance::Plugin};
use clap_sys::ext::ambisonic::{CLAP_EXT_AMBISONIC, CLAP_EXT_AMBISONIC_COMPAT, clap_plugin_ambisonic};
use std::{ffi::CStr, ptr::NonNull};

#[allow(unused)]
pub struct Ambisonic<'a> {
    plugin: &'a Plugin<'a>,
    ambisonic: NonNull<clap_plugin_ambisonic>,
}

impl<'a> Extension<&'a Plugin<'a>> for Ambisonic<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_AMBISONIC, CLAP_EXT_AMBISONIC_COMPAT];

    type Struct = clap_plugin_ambisonic;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            ambisonic: extension_struct,
        }
    }
}
