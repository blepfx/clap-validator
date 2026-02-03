use crate::plugin::ext::Extension;
use crate::plugin::instance::Plugin;
use crate::plugin::util::clap_call;
use clap_sys::ext::surround::*;
use std::ffi::CStr;
use std::ptr::NonNull;

pub struct Surround<'a> {
    plugin: &'a Plugin<'a>,
    surround: NonNull<clap_plugin_surround>,
}

impl<'a> Extension for Surround<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_SURROUND, CLAP_EXT_SURROUND_COMPAT];

    type Plugin = &'a Plugin<'a>;
    type Struct = clap_plugin_surround;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            surround: extension_struct,
        }
    }
}

impl<'a> Surround<'a> {
    #[tracing::instrument(name = "clap_plugin_surround::is_channel_mask_supported", level = 1, skip(self))]
    pub fn is_channel_mask_supported(&self, channel_mask: u64) -> bool {
        let surround = self.surround.as_ptr();
        let plugin = self.plugin.as_ptr();

        unsafe {
            clap_call! {
                surround=>is_channel_mask_supported(
                    plugin,
                    channel_mask
                )
            }
        }
    }

    #[tracing::instrument(name = "clap_plugin_surround::get_channel_map", level = 1, skip(self))]
    pub fn get_channel_map(&self, is_input: bool, port_index: u32, channel_count: u32) -> Vec<u8> {
        let surround = self.surround.as_ptr();
        let plugin = self.plugin.as_ptr();

        unsafe {
            let mut channel_map = vec![0u8; channel_count as usize];
            let channels_real = clap_call! {
                surround=>get_channel_map(
                    plugin,
                    is_input,
                    port_index,
                    channel_map.as_mut_ptr(),
                    channel_count
                )
            };

            channel_map.truncate(channels_real as usize);
            channel_map
        }
    }
}
