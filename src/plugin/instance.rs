//! Abstractions for single CLAP plugin instances for main thread interactions.

use super::ext::Extension;
use super::library::{PluginLibrary, PluginMetadata};
use crate::plugin::preset_discovery::LocationValue;
use crate::util::{self, check_null_ptr, unsafe_clap_call};
use anyhow::{Context, Result};
use audio_thread::PluginAudioThread;
use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORTS_RESCAN_NAMES, CLAP_EXT_AUDIO_PORTS, clap_host_audio_ports,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency};
use clap_sys::ext::note_ports::{
    CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI,
    CLAP_NOTE_DIALECT_MIDI_MPE, CLAP_NOTE_PORTS_RESCAN_ALL, CLAP_NOTE_PORTS_RESCAN_NAMES,
    clap_host_note_ports, clap_note_dialect,
};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_RESCAN_ALL, CLAP_PARAM_RESCAN_INFO, CLAP_PARAM_RESCAN_TEXT,
    CLAP_PARAM_RESCAN_VALUES, clap_host_params, clap_param_clear_flags, clap_param_rescan_flags,
};
use clap_sys::ext::preset_load::{CLAP_EXT_PRESET_LOAD, clap_host_preset_load};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_host_tail};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::voice_info::{CLAP_EXT_VOICE_INFO, clap_host_voice_info};
use clap_sys::factory::plugin_factory::clap_plugin_factory;
use clap_sys::factory::preset_discovery::clap_preset_discovery_location_kind;
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::clap_plugin;
use clap_sys::version::CLAP_VERSION;
use crossbeam::atomic::AtomicCell;
use crossbeam::queue::SegQueue;
use std::ffi::{CStr, c_char, c_void};
use std::marker::PhantomData;
use std::panic::resume_unwind;
use std::pin::Pin;
use std::ptr::NonNull;
use std::sync::Arc;
use std::thread::ThreadId;

pub mod audio_thread;
pub mod process;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallbackEvent {
    RequestProcess,
    RequestFlush,
    RescanParamsValues,
    RescanParamsText,
    RescanParamsInfo,
    RescanParamsAll,
    RescanAudioPortsNames,
    RescanAudioPortsAll,
    RescanNotePortsNames,
    RescanNotePortsAll,
    ChangedLatency,
    ChangedTail,
    ChangedVoiceInfo,
    ChangedState,
}

/// The plugin's current lifecycle state. This is checked extensively to ensure that the plugin is
/// in the correct state, and things like double activations can't happen. `Plugin` and
/// `PluginAudioThread` will drop down to the previous state automatically when the object is
/// dropped and the stop processing or deactivate functions have not yet been calle.d
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginStatus {
    #[default]
    Uninitialized,
    Deactivated,
    Activating,
    Activated,
    Processing,
}

impl PluginStatus {
    #[track_caller]
    pub fn assert_is(&self, expected: PluginStatus) {
        if *self != expected {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?}, \
                 must be {:?}). This is a bug in the validator.",
                self, expected
            )
        }
    }

    #[track_caller]
    pub fn assert_is_not(&self, unexpected: PluginStatus) {
        if *self == unexpected {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?}, \
                 must not be {:?}). This is a bug in the validator.",
                self, unexpected
            )
        }
    }

    #[track_caller]
    pub fn assert_active(&self) {
        if *self < PluginStatus::Activated {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?}, \
                 must be activated). This is a bug in the validator.",
                self
            )
        }
    }

    #[track_caller]
    pub fn assert_inactive(&self) {
        if *self >= PluginStatus::Activated {
            panic!(
                "Invalid plugin function call while the plugin is in an incorrect state ({:?}, \
                 must be deactivated). This is a bug in the validator.",
                self
            )
        }
    }
}

/// A CLAP plugin instance. The plugin will be deinitialized when this object is dropped. All
/// functions here are callable only from the main thread. Use the
/// [`on_audio_thread()`][Self::on_audio_thread()] method to spawn an audio thread.
///
/// All functions on `Plugin` and the objects created from it will panic if the plugin is not in the
/// correct state.
pub struct Plugin<'lib> {
    handle: NonNull<clap_plugin>,

    /// Information about this plugin instance stored on the host. This keeps track of things like
    /// audio thread IDs, whether the plugin has pending callbacks, and what state it is in.
    state: Pin<Arc<InstanceState>>,

    /// The CLAP plugin library this plugin instance was created from. This field is not used
    /// directly, but keeping a reference to the library here prevents the plugin instance from
    /// outliving the library.
    _library: &'lib PluginLibrary,

    /// To honor CLAP's thread safety guidelines, the thread this object was created from is
    /// designated the 'main thread', and this object cannot be shared with other threads. The
    /// [`on_audio_thread()`][Self::on_audio_thread()] method spawns an audio thread that is able to call
    /// the plugin's audio thread functions.
    _send_sync_marker: PhantomData<*const ()>,
}

impl Drop for Plugin<'_> {
    fn drop(&mut self) {
        if let Some(error) = self.state.callback_error.take() {
            log::warn!(
                "The validator's host has detected a callback error but this error has not been \
                 used as part of the test result. This could be a clap-validator bug. The error \
                 message is: {error}"
            )
        }

        // Make sure the plugin is in the correct state before it gets destroyed
        match self.status() {
            PluginStatus::Uninitialized | PluginStatus::Deactivated => (),
            PluginStatus::Activated => self.deactivate(),
            status => panic!(
                "The plugin was in an invalid state '{status:?}' when the instance got dropped, \
                 this is a clap-validator bug"
            ),
        }

        // TODO: We can't handle host callbacks that happen in between these two functions, but the
        //       plugin really shouldn't be making callbacks in deactivate()
        let plugin = self.as_ptr();
        unsafe_clap_call! { plugin=>destroy(plugin) };
    }
}

impl<'lib> Plugin<'lib> {
    /// Create a plugin instance and return the still uninitialized plugin. Returns an error if the
    /// plugin could not be created. The plugin instance will be registered with the host, and
    /// unregistered when this object is dropped again.
    pub fn new(
        library: &'lib PluginLibrary,
        factory: &clap_plugin_factory,
        plugin_id: &CStr,
    ) -> Result<Self> {
        let state = InstanceState::new();
        let plugin = unsafe_clap_call! {
            factory=>create_plugin(factory, state.clap_host_ptr(), plugin_id.as_ptr())
        };

        if plugin.is_null() {
            anyhow::bail!(
                "'clap_plugin_factory::create_plugin({plugin_id:?})' returned a null pointer."
            );
        }

        Ok(Plugin {
            handle: NonNull::new(plugin as *mut clap_plugin).unwrap(),
            state,

            _library: library,
            _send_sync_marker: PhantomData,
        })
    }

    /// Get the raw pointer to the `clap_plugin` instance.
    pub fn as_ptr(&self) -> *const clap_plugin {
        self.handle.as_ptr()
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

    /// Get the reference to a thread-safe object containing information about this plugin instance.
    pub fn state(&self) -> &InstanceState {
        &self.state
    }

    /// The plugin's current initialization status.
    pub fn status(&self) -> PluginStatus {
        self.state.status.load()
    }

    /// Handle any pending main-thread callbacks for this plugin.
    /// Returns an error if there is a callback error pending.
    pub fn handle_callback(&self) -> Result<()> {
        if self.state.requested_callback.swap(false) {
            let plugin = self.as_ptr();
            unsafe_clap_call! { plugin=>on_main_thread(plugin) };
        }

        if let Some(error) = self.state.callback_error.take() {
            anyhow::bail!(error);
        }

        Ok(())
    }

    /// Get the _main thread_ extension abstraction for the extension `T`, if the plugin supports
    /// this extension. Returns `None` if it does not. The plugin needs to be initialized using
    /// [`init()`][Self::init()] before this may be called.
    pub fn get_extension<'a, T: Extension<&'a Self>>(&'a self) -> Option<T> {
        self.status().assert_is_not(PluginStatus::Uninitialized);

        let plugin = self.as_ptr();
        for id in T::IDS {
            let extension_ptr = unsafe_clap_call! { plugin=>get_extension(plugin, id.as_ptr()) };

            if !extension_ptr.is_null() {
                return unsafe {
                    Some(T::new(
                        self,
                        NonNull::new(extension_ptr as *mut T::Struct).unwrap(),
                    ))
                };
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
    pub fn on_audio_thread<'a, T: Send, F: FnOnce(PluginAudioThread<'a>) -> T + Send>(
        &'a self,
        f: F,
    ) -> T {
        struct SendWrapper<'lib>(&'lib Plugin<'lib>);

        // SAFETY: We artificially impose `!Send`+`!Sync` requirements on `Plugin` and
        //         `PluginAudioThread` to prevent them from being shared with other
        //         threads. But we'll need to temporarily lift that restriction in order
        //         to create this `PluginAudioThread`.
        unsafe impl<'lib> Send for SendWrapper<'lib> {}
        unsafe impl<'lib> Sync for SendWrapper<'lib> {}

        impl<'lib> SendWrapper<'lib> {
            fn get(&self) -> &'lib Plugin<'lib> {
                self.0
            }
        }

        self.status().assert_is(PluginStatus::Activated);

        let is_running = AtomicCell::new(true);
        let send_wrapper = SendWrapper(self);

        crossbeam::scope(|s| {
            let audio_thread = s
                .builder()
                .name(String::from("audio-thread"))
                .spawn(|_| {
                    struct SetFalseOnDrop<'a>(&'a AtomicCell<bool>);
                    impl<'a> Drop for SetFalseOnDrop<'a> {
                        fn drop(&mut self) {
                            self.0.store(false);
                        }
                    }

                    let this = send_wrapper.get();

                    // So we know when to stop handling callbacks on the main thread
                    // even if the audio thread panics
                    let _guard = SetFalseOnDrop(&is_running);

                    // This is used to check that calls are run from an audio thread
                    this.state
                        .audio_thread_id
                        .store(Some(std::thread::current().id()));

                    f(PluginAudioThread::new(this))
                })
                .expect("Unable to spawn an audio thread");

            // Handle callbacks requests on the main thread while the audio thread is running
            while is_running.load() {
                if self.state.requested_callback.swap(false) {
                    let plugin = self.as_ptr();
                    unsafe_clap_call! { plugin=>on_main_thread(plugin) };
                }

                std::thread::sleep(std::time::Duration::from_millis(1));
            }

            audio_thread
                .join()
                .unwrap_or_else(|panic_info| resume_unwind(panic_info))
        })
        .unwrap()
    }

    /// Initialize the plugin. This needs to be called before doing anything else.
    pub fn init(&self) -> Result<()> {
        self.status().assert_is(PluginStatus::Uninitialized);

        let plugin = self.as_ptr();
        if unsafe_clap_call! { plugin=>init(plugin) } {
            // If the plugin never calls `request_callback`, the validator won't catch this
            anyhow::ensure!(
                unsafe { (*plugin).on_main_thread.is_some() },
                "clap_plugin::on_main_thread is null"
            );

            self.state.status.store(PluginStatus::Deactivated);
            Ok(())
        } else {
            anyhow::bail!("'clap_plugin::init()' returned false.")
        }
    }

    /// Activate the plugin. Returns an error if the plugin returned `false`. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn activate(
        &self,
        sample_rate: f64,
        min_buffer_size: usize,
        max_buffer_size: usize,
    ) -> Result<()> {
        self.status().assert_is(PluginStatus::Deactivated);

        // Apparently 0 is invalid here
        assert!(min_buffer_size >= 1);
        assert!(max_buffer_size >= min_buffer_size);

        // we need to track the `Activating` state to validate that we call clap_host_latency::changed only within the activation call.
        self.state.status.store(PluginStatus::Activating);

        let plugin = self.as_ptr();
        if unsafe_clap_call! {
            plugin=>activate(plugin, sample_rate, min_buffer_size as u32, max_buffer_size as u32)
        } {
            self.state.status.store(PluginStatus::Activated);
            Ok(())
        } else {
            self.state.status.store(PluginStatus::Deactivated);
            anyhow::bail!("'clap_plugin::activate()' returned false.")
        }
    }

    /// Deactivate the plugin. See
    /// [plugin.h](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) for the
    /// preconditions.
    pub fn deactivate(&self) {
        self.status().assert_is(PluginStatus::Activated);

        let plugin = self.as_ptr();
        unsafe_clap_call! { plugin=>deactivate(plugin) };

        self.state.status.store(PluginStatus::Deactivated);
    }
}

/// Runtime information about a plugin instance. This keeps track of pending callbacks and things
/// like audio threads. It also contains the plugin's unique `clap_host` struct so host callbacks
/// can be linked back to this specific plugin instance.
pub struct InstanceState {
    pub callback_events: SegQueue<CallbackEvent>,
    pub callback_error: AtomicCell<Option<String>>,

    /// The plugin's current state in terms of activation and processing status.
    pub status: AtomicCell<PluginStatus>,

    /// The plugin instance's main thread. Used for the main thread checks.
    pub main_thread_id: ThreadId,

    /// The plugin instance's audio thread, if it has one. Used for the audio thread checks.
    pub audio_thread_id: AtomicCell<Option<ThreadId>>,

    /// Whether the plugin has called `clap_host::request_callback()` and expects
    /// `clap_plugin::on_main_thread()` to be called on the main thread.
    pub requested_callback: AtomicCell<bool>,

    /// Whether the plugin has called `clap_host::request_restart()` and expects the plugin to be
    /// deactivated and subsequently reactivated.
    pub requested_restart: AtomicCell<bool>,

    clap_host: clap_host,
    clap_host_audio_ports: clap_host_audio_ports,
    clap_host_note_ports: clap_host_note_ports,
    clap_host_params: clap_host_params,
    clap_host_preset_load: clap_host_preset_load,
    clap_host_state: clap_host_state,
    clap_host_thread_check: clap_host_thread_check,
    clap_host_latency: clap_host_latency,
    clap_host_tail: clap_host_tail,
    clap_host_voice_info: clap_host_voice_info,
}

impl InstanceState {
    pub fn new() -> Pin<Arc<Self>> {
        let main_thread = std::thread::current().id();
        let instance = Arc::pin(InstanceState {
            callback_events: SegQueue::new(),
            callback_error: AtomicCell::new(None),

            status: AtomicCell::new(PluginStatus::Uninitialized),
            main_thread_id: main_thread,
            audio_thread_id: AtomicCell::new(None),
            requested_callback: AtomicCell::new(false),
            requested_restart: AtomicCell::new(false),

            clap_host: clap_host {
                clap_version: CLAP_VERSION,
                // This is populated with a pointer to the `Arc<Self>`'s data after creating the Arc
                host_data: std::ptr::null_mut(),
                name: c"clap-validator".as_ptr(),
                vendor: c"Robbert van der Helm".as_ptr(),
                url: c"https://github.com/free-audio/clap-validator".as_ptr(),
                version: c"0.1.0".as_ptr(), //TODO: use crate version
                get_extension: Some(Self::get_extension),
                request_restart: Some(Self::request_restart),
                request_process: Some(Self::request_process),
                request_callback: Some(Self::request_callback),
            },

            clap_host_audio_ports: clap_host_audio_ports {
                is_rescan_flag_supported: Some(Self::ext_audio_ports_is_rescan_flag_supported),
                rescan: Some(Self::ext_audio_ports_rescan),
            },
            clap_host_note_ports: clap_host_note_ports {
                supported_dialects: Some(Self::ext_note_ports_supported_dialects),
                rescan: Some(Self::ext_note_ports_rescan),
            },
            clap_host_preset_load: clap_host_preset_load {
                on_error: Some(Self::ext_preset_load_on_error),
                loaded: Some(Self::ext_preset_load_loaded),
            },
            clap_host_params: clap_host_params {
                rescan: Some(Self::ext_params_rescan),
                clear: Some(Self::ext_params_clear),
                request_flush: Some(Self::ext_params_request_flush),
            },
            clap_host_state: clap_host_state {
                mark_dirty: Some(Self::ext_state_mark_dirty),
            },
            clap_host_thread_check: clap_host_thread_check {
                is_main_thread: Some(Self::ext_thread_check_is_main_thread),
                is_audio_thread: Some(Self::ext_thread_check_is_audio_thread),
            },
            clap_host_latency: clap_host_latency {
                changed: Some(Self::ext_latency_changed),
            },
            clap_host_tail: clap_host_tail {
                changed: Some(Self::ext_tail_changed),
            },
            clap_host_voice_info: clap_host_voice_info {
                changed: Some(Self::ext_voice_info_changed),
            },
        });

        // Now that the Arc is pinned in memory, we can store a pointer to it in the clap_host struct
        // so it can be retrieved in host callbacks
        unsafe {
            (&raw const instance.clap_host.host_data)
                .cast_mut()
                .write(&*instance as *const _ as *mut std::ffi::c_void);
        }

        instance
    }

    pub fn clap_host_ptr(&self) -> *const clap_host {
        &self.clap_host as *const clap_host
    }

    #[track_caller]
    pub unsafe fn from_clap_host<'a>(host: *const clap_host) -> &'a Self {
        unsafe {
            let state = (*host).host_data as *const InstanceState;
            &*state
        }
    }

    /// Set the callback error field if it does not already contain a value. Earlier errors are not
    /// overwritten.
    fn set_callback_error(&self, error: impl Into<String>) {
        if let Some(old_error) = self.callback_error.swap(Some(error.into())) {
            self.callback_error.store(Some(old_error));
        }
    }

    /// Checks whether this is the main thread. If it is not, then an error indicating this can be
    /// retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_main_thread(&self, function_name: &str) {
        let current_thread_id = std::thread::current().id();
        if current_thread_id != self.main_thread_id {
            self.set_callback_error(format!(
                "'{}' may only be called from the main thread (thread {:?}), but it was called \
                 from thread {:?}.",
                function_name, self.main_thread_id, current_thread_id
            ));
        }
    }

    /// Checks whether this is the audio thread. If it is not, then an error indicating this can be
    /// retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_audio_thread(&self, function_name: &str) {
        let current_thread_id = std::thread::current().id();
        if self.audio_thread_id.load() != Some(current_thread_id) {
            if current_thread_id == self.main_thread_id {
                self.set_callback_error(format!(
                    "'{function_name}' may only be called from an audio thread, but it was called \
                     from the main thread."
                ));
            } else {
                self.set_callback_error(format!(
                    "'{function_name}' may only be called from an audio thread, but it was called \
                     from an unknown thread."
                ));
            }
        }
    }

    /// Checks whether this is **not** the audio thread. If it is, then an error indicating this can
    /// be retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_not_audio_thread(&self, function_name: &str) {
        let current_thread_id = std::thread::current().id();
        if self.audio_thread_id.load() == Some(current_thread_id) {
            self.set_callback_error(format!(
                "'{function_name}' was called from an audio thread, this is not allowed.",
            ));
        }
    }

    unsafe extern "C" fn get_extension(
        host: *const clap_host,
        extension_id: *const c_char,
    ) -> *const c_void {
        //check_null_ptr!(host, (*host).host_data, extension_id);
        let this = unsafe { InstanceState::from_clap_host(host) };

        // Right now there's no way to have the host only expose certain extensions. We can always
        // add that when test cases need it.
        let extension_id_cstr = unsafe { CStr::from_ptr(extension_id) };
        if extension_id_cstr == CLAP_EXT_AUDIO_PORTS {
            &this.clap_host_audio_ports as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_NOTE_PORTS {
            &this.clap_host_note_ports as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_PRESET_LOAD {
            &this.clap_host_preset_load as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_PARAMS {
            &this.clap_host_params as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_STATE {
            &this.clap_host_state as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_THREAD_CHECK {
            &this.clap_host_thread_check as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_LATENCY {
            &this.clap_host_latency as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_TAIL {
            &this.clap_host_tail as *const _ as *const c_void
        } else if extension_id_cstr == CLAP_EXT_VOICE_INFO {
            &this.clap_host_voice_info as *const _ as *const c_void
        } else {
            std::ptr::null()
        }
    }

    unsafe extern "C" fn request_restart(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        // This flag will be reset at the start of one of the `ProcessingTest::run*` functions, and
        // in the multi-iteration run function it will trigger a deactivate->reactivate cycle
        log::trace!("'clap_host::request_restart()' was called by the plugin, setting the flag");
        this.requested_restart.store(true);
    }

    unsafe extern "C" fn request_process(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        // Handling this within the context of the validator would be a bit messy. Do plugins use
        // this?
        log::trace!("'clap_host::request_process()' was called by the plugin");
        this.callback_events.push(CallbackEvent::RequestProcess);
    }

    unsafe extern "C" fn request_callback(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        // This this is either handled by `handle_callbacks_blocking()` while the audio thread is
        // active, or by an explicit call to `handle_callbacks_once()`. We print a warning if the
        // callback is not handled before the plugin is destroyed.
        log::trace!("'clap_host::request_callback()' was called by the plugin, setting the flag");
        this.requested_callback.store(true);
    }

    unsafe extern "C" fn ext_audio_ports_is_rescan_flag_supported(
        host: *const clap_host,
        _flag: u32,
    ) -> bool {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_audio_ports::is_rescan_flag_supported()");
        log::trace!("'clap_host_audio_ports::is_rescan_flag_supported()' was called");
        true
    }

    unsafe extern "C" fn ext_audio_ports_rescan(host: *const clap_host, flags: u32) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_audio_ports::rescan()");
        log::trace!("'clap_host_audio_ports::rescan()' was called");

        if flags & CLAP_AUDIO_PORTS_RESCAN_NAMES != 0 {
            this.callback_events
                .push(CallbackEvent::RescanAudioPortsNames);
        }

        if flags & !CLAP_AUDIO_PORTS_RESCAN_NAMES != 0 {
            if this.status.load() > PluginStatus::Activated {
                this.set_callback_error(
                    "'clap_host_audio_ports::rescan()' was called while the plugin was activated",
                );
            }

            this.callback_events
                .push(CallbackEvent::RescanAudioPortsAll);
        }
    }

    unsafe extern "C" fn ext_note_ports_supported_dialects(
        host: *const clap_host,
    ) -> clap_note_dialect {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_note_ports::supported_dialects()");
        log::trace!("'clap_host_note_ports::supported_dialects()' was called");

        CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE
    }

    unsafe extern "C" fn ext_note_ports_rescan(host: *const clap_host, flags: u32) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_note_ports::rescan()");
        log::trace!("'clap_host_note_ports::rescan()' was called");

        if flags & CLAP_NOTE_PORTS_RESCAN_NAMES != 0 {
            this.callback_events
                .push(CallbackEvent::RescanNotePortsNames);
        }

        if flags & CLAP_NOTE_PORTS_RESCAN_ALL != 0 {
            if this.status.load() > PluginStatus::Activated {
                this.set_callback_error(
                    "'clap_host_note_ports::rescan(CLAP_NOTE_PORTS_RESCAN_ALL)' was called while \
                     the plugin was activated",
                );
            }

            this.callback_events.push(CallbackEvent::RescanNotePortsAll);
        }
    }

    unsafe extern "C" fn ext_preset_load_on_error(
        host: *const clap_host,
        location_kind: clap_preset_discovery_location_kind,
        location: *const c_char,
        load_key: *const c_char,
        os_error: i32,
        msg: *const c_char,
    ) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_preset_load::on_error()");

        let location = unsafe { LocationValue::new(location_kind, location) }
            .context("'clap_host_preset_load::on_error()' called with invalid location parameters");
        let load_key = unsafe { util::cstr_ptr_to_optional_string(load_key) }.context(
            "'clap_host_preset_load::on_error()' called with an invalid load_key parameter",
        );
        let msg = unsafe { util::cstr_ptr_to_mandatory_string(msg) }
            .context("'clap_host_preset_load::on_error()' called with an invalid msg parameter");
        match (location, load_key, msg) {
            (Ok(location), Ok(Some(load_key)), Ok(msg)) => {
                this.set_callback_error(format!(
                    "'clap_host_preset_load::on_error()' called for {location} with load key \
                     {load_key}, OS error code {os_error}, and the following error message: {msg}"
                ));
            }
            (Ok(location), Ok(None), Ok(msg)) => {
                this.set_callback_error(format!(
                    "'clap_host_preset_load::on_error()' called for {location} with no load key, \
                     OS error code {os_error}, and the following error message: {msg}"
                ));
            }
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => {
                this.set_callback_error(format!("{err:#}"));
            }
        }
    }

    unsafe extern "C" fn ext_preset_load_loaded(
        host: *const clap_host,
        location_kind: clap_preset_discovery_location_kind,
        location: *const c_char,
        load_key: *const c_char,
    ) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_preset_load::loaded()");

        let location = unsafe { LocationValue::new(location_kind, location) }
            .context("'clap_host_preset_load::loaded()' called with invalid location parameters");
        let load_key = unsafe { util::cstr_ptr_to_optional_string(load_key) }
            .context("'clap_host_preset_load::loaded()' called with an invalid load_key parameter");
        match (location, load_key) {
            (Ok(_location), Ok(_load_key)) => {
                log::debug!("TODO: Handle 'clap_host_preset_load::loaded()'");
            }
            (Err(err), _) | (_, Err(err)) => {
                this.set_callback_error(format!("{err:#}"));
            }
        }
    }

    unsafe extern "C" fn ext_params_rescan(host: *const clap_host, flags: clap_param_rescan_flags) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_params::rescan()");
        log::trace!("'clap_host_params::rescan()' was called");

        if flags & CLAP_PARAM_RESCAN_VALUES != 0 {
            this.callback_events.push(CallbackEvent::RescanParamsValues);
        }

        if flags & CLAP_PARAM_RESCAN_TEXT != 0 {
            this.callback_events.push(CallbackEvent::RescanParamsText);
        }

        if flags & CLAP_PARAM_RESCAN_INFO != 0 {
            this.callback_events.push(CallbackEvent::RescanParamsInfo);
        }

        if flags & CLAP_PARAM_RESCAN_ALL != 0 {
            if this.status.load() > PluginStatus::Activated {
                this.set_callback_error(
                    "'clap_host_params::rescan(CLAP_PARAM_RESCAN_ALL)' was called while the \
                     plugin is activated",
                );
            }

            this.callback_events.push(CallbackEvent::RescanParamsAll);
        }
    }

    unsafe extern "C" fn ext_params_clear(
        host: *const clap_host,
        _param_id: clap_id,
        _flags: clap_param_clear_flags,
    ) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_params::clear()");
        log::debug!("TODO: Handle 'clap_host_params::clear()'");
    }

    unsafe extern "C" fn ext_params_request_flush(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_not_audio_thread("clap_host_params::request_flush()");
        log::trace!("'clap_host_params::request_flush()' was called");
        this.callback_events.push(CallbackEvent::RequestFlush);
    }

    unsafe extern "C" fn ext_state_mark_dirty(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_state::mark_dirty()");
        log::trace!("'clap_host_state::mark_dirty()' was called");
        this.callback_events.push(CallbackEvent::ChangedState);
    }

    unsafe extern "C" fn ext_thread_check_is_main_thread(host: *const clap_host) -> bool {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };
        this.main_thread_id == std::thread::current().id()
    }

    unsafe extern "C" fn ext_thread_check_is_audio_thread(host: *const clap_host) -> bool {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };
        this.audio_thread_id.load() == Some(std::thread::current().id())
    }

    unsafe extern "C" fn ext_latency_changed(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        if this.status.load() != PluginStatus::Activating {
            this.set_callback_error(
                "'clap_host_latency::changed()' must only be called within \
                 'clap_plugin::activate()'",
            );
        }

        this.assert_main_thread("clap_host_latency::changed()");
        log::trace!("'clap_host_latency::changed()' was called");
        this.callback_events.push(CallbackEvent::ChangedLatency);
    }

    unsafe extern "C" fn ext_tail_changed(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_audio_thread("clap_host_tail::changed()");
        log::trace!("'clap_host_tail::changed()' was called");
        this.callback_events.push(CallbackEvent::ChangedTail);
    }

    unsafe extern "C" fn ext_voice_info_changed(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceState::from_clap_host(host) };

        this.assert_main_thread("clap_host_voice_info::changed()");
        log::trace!("'clap_host_voice_info::changed()' was called");
        this.callback_events.push(CallbackEvent::ChangedVoiceInfo);
    }
}
