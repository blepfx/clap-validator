use crate::plugin::ext::Extension;
use crate::plugin::instance::Plugin;
use crate::plugin::util::clap_call;
use clap_sys::ext::voice_info::*;
use std::ffi::CStr;
use std::mem::zeroed;
use std::ptr::NonNull;

#[allow(unused)]
pub struct VoiceInfo<'a> {
    plugin: &'a Plugin<'a>,
    voice_info: NonNull<clap_plugin_voice_info>,
}

impl<'a> Extension for VoiceInfo<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_VOICE_INFO];

    type Plugin = &'a Plugin<'a>;
    type Struct = clap_plugin_voice_info;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            voice_info: extension_struct,
        }
    }
}

impl<'a> VoiceInfo<'a> {
    #[allow(unused)]
    pub fn get(&self) -> Option<clap_voice_info> {
        let voice_info = self.voice_info.as_ptr();
        let plugin = self.plugin.as_ptr();

        unsafe {
            let mut result = clap_voice_info { ..zeroed() };
            let success = clap_call! { voice_info=>get(plugin, &mut result) };
            if success { Some(result) } else { None }
        }
    }
}
