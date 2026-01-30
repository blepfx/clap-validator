use crate::{
    plugin::{
        ext::Extension,
        instance::{CallbackEvent, PluginAudioThread, PluginShared, PluginStatus},
        library::PluginMetadata,
    },
    util::clap_call,
};
use anyhow::Result;
use clap_sys::plugin::clap_plugin;
use std::{
    marker::PhantomData,
    panic::resume_unwind,
    pin::Pin,
    ptr::NonNull,
    sync::{Arc, mpsc::Receiver},
};

pub enum MainThreadTask {
    Dispatch(Box<dyn FnOnce(&Plugin) + Send>),
    CallbackRequest,
    StopAudioThread,
}

/// A CLAP plugin instance. The plugin will be deinitialized when this object is dropped. All
/// functions here are callable only from the main thread. Use the
/// [`on_audio_thread()`][Self::on_audio_thread()] method to spawn an audio thread.
///
/// All functions on `Plugin` and the objects created from it will panic if the plugin is not in the
/// correct state.
pub struct Plugin<'lib> {
    pub(super) callback_receiver: Receiver<CallbackEvent>,
    pub(super) task_receiver: Receiver<MainThreadTask>,

    /// Information about this plugin instance stored on the host. This keeps track of things like
    /// audio thread IDs, whether the plugin has pending callbacks, and what state it is in.
    pub(super) shared: Pin<Arc<PluginShared>>,

    /// The CLAP plugin library this plugin instance was created from. This field is not used
    /// directly, but keeping a reference to the library here prevents the plugin instance from
    /// outliving the library.
    pub(super) _library: PhantomData<&'lib ()>,

    /// To honor CLAP's thread safety guidelines, the thread this object was created from is
    /// designated the 'main thread', and this object cannot be shared with other threads. The
    /// [`on_audio_thread()`][Self::on_audio_thread()] method spawns an audio thread that is able to call
    /// the plugin's audio thread functions.
    pub(super) _thread: PhantomData<*const ()>,
}

impl Drop for Plugin<'_> {
    fn drop(&mut self) {
        if let Some(error) = self.shared.callback_error.lock().unwrap().take() {
            log::warn!(
                "The validator's host has detected a callback error but this error has not been used as part of the \
                 test result. This could be a clap-validator bug. The error message is: {error}"
            )
        }

        // Make sure the plugin is in the correct state before it gets destroyed
        match self.status() {
            PluginStatus::Uninitialized | PluginStatus::Deactivated => (),
            PluginStatus::Activated => self.deactivate(),
            status => log::warn!(
                "The plugin was in an invalid state '{status:?}' when the instance got dropped, this is a \
                 clap-validator bug"
            ),
        }

        self.handle_callback_unchecked();

        let plugin = self.as_ptr();
        unsafe {
            clap_call! { plugin=>destroy(plugin) }
        }
    }
}

impl<'lib> Plugin<'lib> {
    /// Get the raw pointer to the `clap_plugin` instance.
    pub fn as_ptr(&self) -> *const clap_plugin {
        self.shared.clap_plugin_ptr()
    }

    /// Get this plugin's metadata descriptor. In theory this should be the same as the one
    /// retrieved from the factory earlier.
    pub fn descriptor(&self) -> Result<PluginMetadata> {
        let plugin = self.as_ptr();
        let descriptor = unsafe { (*plugin).desc };
        if descriptor.is_null() {
            anyhow::bail!("The 'desc' field on the 'clap_plugin' struct is a null pointer.");
        }

        PluginMetadata::from_descriptor(unsafe { &*descriptor })
    }

    /// The plugin's current initialization status.
    pub fn status(&self) -> PluginStatus {
        self.shared.status.load()
    }

    /// Handle any pending main-thread callbacks for this plugin.
    /// Returns an error if there is a callback error pending.
    pub fn handle_callback(&self) -> Result<()> {
        self.handle_callback_unchecked();

        if let Some(error) = self.shared.callback_error.lock().unwrap().take() {
            anyhow::bail!(error);
        }

        // TODO:
        // while let Ok(event) = self.shared.callback_receiver.lock().unwrap().recv() {
        //     println!("{:?}", event);
        // }

        Ok(())
    }

    /// Get the _main thread_ extension abstraction for the extension `T`, if the plugin supports
    /// this extension. Returns `None` if it does not. The plugin needs to be initialized using
    /// [`init()`][Self::init()] before this may be called.
    pub fn get_extension<'a, T: Extension<&'a Self>>(&'a self) -> Option<T> {
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

    /// Execute some code for this plugin from an audio thread context. The closure receives a
    /// [`PluginAudioThread`], which disallows calling main thread functions, and permits calling
    /// audio thread functions.
    ///
    /// If whatever happens on the audio thread caused main-thread callback requests to be emited,
    /// then those will be handled concurrently.
    pub fn on_audio_thread<T: Send, F: FnOnce(PluginAudioThread) -> T + Send>(&self, f: F) -> T {
        let result = crossbeam::scope(|s| {
            let shared = self.shared.clone();

            let thread = s
                .builder()
                .name("audio_thread".into())
                .spawn(move |_| f(PluginAudioThread::new(shared)))
                .unwrap();

            // Handle callbacks requests on the main thread while the audio thread is running
            while let Ok(task) = self.task_receiver.recv() {
                match task {
                    MainThreadTask::Dispatch(func) => func(self),
                    MainThreadTask::CallbackRequest => self.handle_callback_unchecked(),
                    MainThreadTask::StopAudioThread => break,
                }
            }

            // Wait for the result, propagating panics
            thread.join()
        });

        self.handle_callback_unchecked();
        result.flatten().unwrap_or_else(|e| resume_unwind(e))
    }

    /// Initialize the plugin. This needs to be called before doing anything else.
    pub fn init(&self) -> Result<()> {
        self.status().assert_is(PluginStatus::Uninitialized);

        let plugin = self.as_ptr();
        let result = unsafe {
            clap_call! { plugin=>init(plugin) }
        };

        if result {
            // If the plugin never calls `request_callback`, the validator won't catch this
            anyhow::ensure!(
                unsafe { (*plugin).on_main_thread.is_some() },
                "clap_plugin::on_main_thread is null"
            );

            self.shared.status.store(PluginStatus::Deactivated);
            Ok(())
        } else {
            anyhow::bail!("'clap_plugin::init()' returned false.")
        }
    }

    /// Activate the plugin. Returns an error if the plugin returned `false`. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn activate(&self, sample_rate: f64, min_buffer_size: u32, max_buffer_size: u32) -> Result<()> {
        self.status().assert_is(PluginStatus::Deactivated);

        // Apparently 0 is invalid here
        assert!(min_buffer_size >= 1);
        assert!(max_buffer_size >= min_buffer_size);

        // we need to track the `Activating` state to validate that we call clap_host_latency::changed only within the activation call.
        self.shared.status.store(PluginStatus::Activating);

        let plugin = self.as_ptr();
        let result = unsafe {
            clap_call! { plugin=>activate(plugin, sample_rate, min_buffer_size, max_buffer_size) }
        };

        if result {
            self.shared.status.store(PluginStatus::Activated);
            Ok(())
        } else {
            self.shared.status.store(PluginStatus::Deactivated);
            anyhow::bail!("'clap_plugin::activate()' returned false.")
        }
    }

    /// Deactivate the plugin. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn deactivate(&self) {
        self.status().assert_is(PluginStatus::Activated);

        let plugin = self.as_ptr();
        unsafe {
            clap_call! { plugin=>deactivate(plugin) }
        }

        self.shared.status.store(PluginStatus::Deactivated);
    }

    fn handle_callback_unchecked(&self) {
        if self.shared.requested_callback.swap(false) {
            let plugin = self.as_ptr();
            unsafe {
                clap_call! { plugin=>on_main_thread(plugin) }
            };
        }
    }
}
