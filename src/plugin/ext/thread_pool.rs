use crate::plugin::ext::Extension;
use crate::plugin::instance::PluginShared;
use crate::plugin::util::clap_call;
use clap_sys::ext::thread_pool::{CLAP_EXT_THREAD_POOL, clap_plugin_thread_pool};
use std::ffi::CStr;
use std::ptr::NonNull;

pub struct ThreadPool<'a> {
    plugin: &'a PluginShared,
    tail: NonNull<clap_plugin_thread_pool>,
}

unsafe impl Send for ThreadPool<'_> {}
unsafe impl Sync for ThreadPool<'_> {}

impl<'a> Extension<&'a PluginShared> for ThreadPool<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_THREAD_POOL];

    type Struct = clap_plugin_thread_pool;

    unsafe fn new(plugin: &'a PluginShared, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            tail: extension_struct,
        }
    }
}

impl<'a> ThreadPool<'a> {
    pub fn exec(&self, task: u32) {
        let thread_pool = self.tail.as_ptr();
        let plugin = self.plugin.clap_plugin_ptr();
        unsafe {
            clap_call! { thread_pool=>exec(plugin, task) }
        }
    }
}
