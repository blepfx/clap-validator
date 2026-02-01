//! Abstractions for single CLAP plugin instances for audio thread interactions.

use super::{Plugin, PluginStatus};
use crate::plugin::ext::Extension;
use crate::plugin::instance::{CallbackEvent, MainThreadTask, PluginShared};
use crate::plugin::util::clap_call;
use anyhow::Result;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::*;
use std::any::Any;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

/// An audio thread equivalent to [`Plugin`]. This version only allows audio thread functions to be
/// called. It can be constructed using [`Plugin::on_audio_thread()`].
pub struct PluginAudioThread<'a> {
    /// Information about this plugin instance stored on the host. This keeps track of things like
    /// audio thread IDs, whether the plugin has pending callbacks, and what state it is in.
    shared: Pin<Arc<PluginShared>>,

    _plugin_marker: PhantomData<&'a Plugin<'a>>,

    /// To honor CLAP's thread safety guidelines, the thread this object was created from is
    /// designated the 'audio thread', and this object cannot be shared with other threads.
    _send_sync_marker: PhantomData<*const ()>,
}

/// The equivalent of `clap_process_status`, minus the `CLAP_PROCESS_ERROR` value as this is already
/// treated as an error by `PluginAudioThread::process()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessStatus {
    Continue,
    ContinueIfNotQuiet,
    Tail,
    Sleep,
}

impl Drop for PluginAudioThread<'_> {
    fn drop(&mut self) {
        self.shared.audio_thread_id.store(None);
        self.shared.task_sender.send(MainThreadTask::StopAudioThread).unwrap();
    }
}

impl<'a> PluginAudioThread<'a> {
    pub(super) fn new(shared: Pin<Arc<PluginShared>>) -> PluginAudioThread<'a> {
        shared.audio_thread_id.store(Some(std::thread::current().id()));
        PluginAudioThread {
            shared,
            _plugin_marker: PhantomData,
            _send_sync_marker: PhantomData,
        }
    }

    /// Get the raw pointer to the `clap_plugin` instance.
    pub fn as_ptr(&self) -> *const clap_plugin {
        self.shared.clap_plugin_ptr()
    }

    /// Get the plugin's current initialization status.
    pub fn status(&self) -> PluginStatus {
        self.shared.status()
    }

    /// Get a reference to the plugin's shared state.
    pub fn shared(&self) -> &PluginShared {
        &self.shared
    }

    /// Get the _audio thread_ extension abstraction for the extension `T`, if the plugin supports
    /// this extension. Returns `None` if it does not. The plugin needs to be initialized using
    /// [`init()`][Self::init()] before this may be called.
    pub fn get_extension<T: Extension<&'a Self>>(&'a self) -> Option<T> {
        self.status().assert_is_not(PluginStatus::Uninitialized);

        let plugin = self.as_ptr();
        for id in T::IDS {
            let extension_ptr = unsafe {
                clap_call! { plugin=>get_extension(plugin, id.as_ptr()) }
            };

            if !extension_ptr.is_null() {
                return unsafe { Some(T::new(self, NonNull::new(extension_ptr as *mut T::Struct).unwrap())) };
            }
        }

        None
    }

    /// Dispatch a task to be executed on the main thread. This is a blocking call that will wait
    /// for the task to complete and return its result.
    ///
    /// TODO: this could be optimized and the 'static requirement dropped.
    pub fn on_main_thread<F: FnOnce(&Plugin) -> T + Send, T: Send + 'static>(&self, callback: F) -> T {
        struct Context<F, T> {
            sender: SyncSender<Result<T, Box<dyn Any + Send>>>,
            callback: F,
        }

        let (sender, recv) = std::sync::mpsc::sync_channel(0);
        let context = MaybeUninit::new(Context { sender, callback });

        self.shared
            .task_sender
            .send(MainThreadTask::Dispatch {
                data: context.as_ptr() as *mut (),
                call: |plugin, data| {
                    // Safety: we are the only ones with access to this pointer right now.
                    let context = unsafe { (data as *mut Context<F, T>).read() };
                    let result = catch_unwind(AssertUnwindSafe(|| (context.callback)(plugin)));
                    context.sender.send(result).unwrap();
                },
            })
            .unwrap();

        match recv.recv().unwrap() {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    pub fn poll_callback(&self, mut f: impl FnMut(&Plugin, CallbackEvent) -> Result<()> + Send) -> Result<()> {
        self.on_main_thread(|plugin| plugin.poll_callback(|event| f(plugin, event)))
    }

    /// Prepare for audio processing. Returns an error if the plugin returned `false`. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn start_processing(&self) -> Result<()> {
        self.status().assert_is(PluginStatus::Activated);

        let plugin = self.as_ptr();
        let result = unsafe {
            clap_call! { plugin=>start_processing(plugin) }
        };

        if result {
            self.shared.status.store(PluginStatus::Processing);
            Ok(())
        } else {
            anyhow::bail!("'clap_plugin::start_processing()' returned false.")
        }
    }

    /// Process audio. If the plugin returned either `CLAP_PROCESS_ERROR` or an unknown process
    /// status code, then this will return an error. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn process(&self, process_data: &clap_process) -> Result<ProcessStatus> {
        self.status().assert_is(PluginStatus::Processing);

        self.shared.is_currently_in_process_call.store(true);

        let plugin = self.as_ptr();
        let result = unsafe {
            clap_call! { plugin=>process(plugin, process_data) }
        };

        self.shared.is_currently_in_process_call.store(false);

        match result {
            CLAP_PROCESS_ERROR => {
                anyhow::bail!("The plugin returned 'CLAP_PROCESS_ERROR' from 'clap_plugin::process()'.")
            }
            CLAP_PROCESS_CONTINUE => Ok(ProcessStatus::Continue),
            CLAP_PROCESS_CONTINUE_IF_NOT_QUIET => Ok(ProcessStatus::ContinueIfNotQuiet),
            CLAP_PROCESS_TAIL => Ok(ProcessStatus::Tail),
            CLAP_PROCESS_SLEEP => Ok(ProcessStatus::Sleep),
            result => anyhow::bail!(
                "The plugin returned an unknown 'clap_process_status' value {result} from 'clap_plugin::process()'."
            ),
        }
    }

    /// Reset the internal state of the plugin.
    pub fn reset(&self) {
        self.status().assert_active();

        let plugin = self.as_ptr();
        unsafe {
            clap_call! { plugin=>reset(plugin) }
        };
    }

    /// Stop processing audio. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn stop_processing(&self) {
        self.status().assert_is(PluginStatus::Processing);

        let plugin = self.as_ptr();
        unsafe {
            clap_call! { plugin=>stop_processing(plugin) }
        };

        self.shared.status.store(PluginStatus::Activated);
    }
}
