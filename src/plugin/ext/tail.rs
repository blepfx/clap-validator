use crate::plugin::ext::Extension;
use crate::plugin::instance::PluginAudioThread;
use crate::util::clap_call;
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_plugin_tail};
use std::ffi::CStr;
use std::ptr::NonNull;

#[allow(unused)]
pub struct Tail<'a> {
    plugin: &'a PluginAudioThread<'a>,
    tail: NonNull<clap_plugin_tail>,
}

impl<'a> Extension<&'a PluginAudioThread<'a>> for Tail<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_TAIL];

    type Struct = clap_plugin_tail;

    unsafe fn new(plugin: &'a PluginAudioThread<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            tail: extension_struct,
        }
    }
}

impl<'a> Tail<'a> {
    #[allow(unused)]
    pub fn get(&self) -> u32 {
        let tail = self.tail.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe {
            clap_call! { tail=>get(plugin) }
        }
    }
}
