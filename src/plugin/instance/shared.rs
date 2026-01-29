use crate::plugin::instance::{CallbackEvent, Plugin, PluginStatus};
use crate::plugin::preset_discovery::LocationValue;
use crate::util::{self, check_null_ptr, clap_call, validator_version};
use anyhow::{Context, Result};
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
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
use crossbeam_utils::atomic::AtomicCell;
use std::ffi::{CStr, c_char, c_void};
use std::pin::Pin;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

/// Plugin instance state that is shared between the main thread, audio thread and any external unmanaged threads.
/// This struct contains the `clap_host` and its extensions, as well as fields for tracking the plugin's state.
pub struct InstanceShared {
    pub task_sender: Sender<MainThreadTask>,
    pub callback_sender: Sender<CallbackEvent>,
    pub callback_error: Mutex<Option<String>>,

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

    clap_plugin: *const clap_plugin,
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

/// Information about a plugin instance's main thread.
pub struct InstanceMainThread {
    pub callback_receiver: Receiver<CallbackEvent>,
    pub task_receiver: Receiver<MainThreadTask>,
}

pub enum MainThreadTask {
    Dispatch { func: fn(&Plugin, *mut ()), data: *mut () },
    CallbackRequest,
    StopAudioThread,
}

impl InstanceShared {
    pub unsafe fn new(factory: &clap_plugin_factory, plugin_id: &CStr) -> Result<(Pin<Arc<Self>>, InstanceMainThread)> {
        let main_thread = std::thread::current().id();
        let (callback_sender, callback_receiver) = channel();
        let (task_sender, task_receiver) = channel();

        let shared = Arc::pin(InstanceShared {
            task_sender,
            callback_sender,
            callback_error: Mutex::new(None),

            status: AtomicCell::new(PluginStatus::Uninitialized),
            main_thread_id: main_thread,
            audio_thread_id: AtomicCell::new(None),
            requested_callback: AtomicCell::new(false),
            requested_restart: AtomicCell::new(false),

            clap_plugin: std::ptr::null(),
            clap_host: clap_host {
                clap_version: CLAP_VERSION,
                // This is populated with a pointer to the `Arc<Self>`'s data after creating the Arc
                host_data: std::ptr::null_mut(),
                name: c"clap-validator".as_ptr(),
                vendor: c"Robbert van der Helm".as_ptr(),
                url: c"https://github.com/free-audio/clap-validator".as_ptr(),
                version: validator_version().as_ptr(),
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

        let main = InstanceMainThread {
            callback_receiver,
            task_receiver,
        };

        // Now that the Arc is pinned in memory, we can store a pointer to it in the clap_host struct
        // so it can be retrieved in host callbacks
        unsafe {
            (&raw const shared.clap_host.host_data)
                .cast_mut()
                .write(&*shared as *const _ as *mut std::ffi::c_void);
        }

        let clap_plugin = unsafe {
            clap_call! {
                factory=>create_plugin(factory, shared.clap_host_ptr(), plugin_id.as_ptr())
            }
        };

        if clap_plugin.is_null() {
            anyhow::bail!("'clap_plugin_factory::create_plugin({plugin_id:?})' returned a null pointer.");
        }

        unsafe {
            (&raw const shared.clap_plugin).cast_mut().write(clap_plugin);
        }

        Ok((shared, main))
    }

    pub fn clap_host_ptr(&self) -> *const clap_host {
        &self.clap_host as *const clap_host
    }

    pub fn clap_plugin_ptr(&self) -> *const clap_plugin {
        self.clap_plugin
    }

    #[track_caller]
    unsafe fn from_clap_host<'a>(host: *const clap_host) -> &'a Self {
        unsafe {
            let state = (*host).host_data as *const InstanceShared;
            &*state
        }
    }

    /// Set the callback error field if it does not already contain a value. Earlier errors are not
    /// overwritten.
    fn set_callback_error(&self, error: impl Into<String>) {
        let mut guard = self.callback_error.lock().unwrap();
        if guard.is_none() {
            *guard = Some(error.into());
        }
    }

    /// Checks whether this is the main thread. If it is not, then an error indicating this can be
    /// retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_main_thread(&self, function_name: &str) {
        let current_thread_id = std::thread::current().id();
        if current_thread_id != self.main_thread_id {
            self.set_callback_error(format!(
                "'{}' may only be called from the main thread (thread {:?}), but it was called from thread {:?}.",
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
                    "'{function_name}' may only be called from an audio thread, but it was called from the main \
                     thread."
                ));
            } else {
                self.set_callback_error(format!(
                    "'{function_name}' may only be called from an audio thread, but it was called from an unknown \
                     thread."
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

    unsafe extern "C" fn get_extension(host: *const clap_host, extension_id: *const c_char) -> *const c_void {
        //check_null_ptr!(host, (*host).host_data, extension_id);
        let this = unsafe { InstanceShared::from_clap_host(host) };

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
        let this = unsafe { InstanceShared::from_clap_host(host) };

        // This flag will be reset at the start of one of the `ProcessingTest::run*` functions, and
        // in the multi-iteration run function it will trigger a deactivate->reactivate cycle
        log::trace!("'clap_host::request_restart()' was called by the plugin, setting the flag");
        this.requested_restart.store(true);
    }

    unsafe extern "C" fn request_process(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        // Handling this within the context of the validator would be a bit messy. Do plugins use
        // this?
        log::trace!("'clap_host::request_process()' was called by the plugin");
        this.callback_sender.send(CallbackEvent::RequestProcess).unwrap();
    }

    unsafe extern "C" fn request_callback(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        // This this is either handled by `handle_callbacks_blocking()` while the audio thread is
        // active, or by an explicit call to `handle_callbacks_once()`. We print a warning if the
        // callback is not handled before the plugin is destroyed.
        log::trace!("'clap_host::request_callback()' was called by the plugin, setting the flag");
        this.requested_callback.store(true);
        this.task_sender.send(MainThreadTask::CallbackRequest).unwrap();
    }

    unsafe extern "C" fn ext_audio_ports_is_rescan_flag_supported(host: *const clap_host, _flag: u32) -> bool {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_audio_ports::is_rescan_flag_supported()");
        log::trace!("'clap_host_audio_ports::is_rescan_flag_supported()' was called");
        true
    }

    unsafe extern "C" fn ext_audio_ports_rescan(host: *const clap_host, flags: u32) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_audio_ports::rescan()");
        log::trace!("'clap_host_audio_ports::rescan()' was called");

        if flags & CLAP_AUDIO_PORTS_RESCAN_NAMES != 0 {
            this.callback_sender.send(CallbackEvent::AudioPortsRescanNames).unwrap();
        }

        if flags & !CLAP_AUDIO_PORTS_RESCAN_NAMES != 0 {
            if this.status.load() > PluginStatus::Activated {
                this.set_callback_error("'clap_host_audio_ports::rescan()' was called while the plugin was activated");
            }

            this.callback_sender.send(CallbackEvent::AudioPortsRescanAll).unwrap();
        }
    }

    unsafe extern "C" fn ext_note_ports_supported_dialects(host: *const clap_host) -> clap_note_dialect {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_note_ports::supported_dialects()");
        log::trace!("'clap_host_note_ports::supported_dialects()' was called");

        CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE
    }

    unsafe extern "C" fn ext_note_ports_rescan(host: *const clap_host, flags: u32) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_note_ports::rescan()");
        log::trace!("'clap_host_note_ports::rescan()' was called");

        if flags & CLAP_NOTE_PORTS_RESCAN_NAMES != 0 {
            this.callback_sender.send(CallbackEvent::NotePortsRescanNames).unwrap();
        }

        if flags & CLAP_NOTE_PORTS_RESCAN_ALL != 0 {
            if this.status.load() > PluginStatus::Activated {
                this.set_callback_error(
                    "'clap_host_note_ports::rescan(CLAP_NOTE_PORTS_RESCAN_ALL)' was called while the plugin was \
                     activated",
                );
            }

            this.callback_sender.send(CallbackEvent::NotePortsRescanAll).unwrap();
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
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_preset_load::on_error()");

        let location = unsafe { LocationValue::new(location_kind, location) }
            .context("'clap_host_preset_load::on_error()' called with invalid location parameters");
        let load_key = unsafe { util::cstr_ptr_to_optional_string(load_key) }
            .context("'clap_host_preset_load::on_error()' called with an invalid load_key parameter");
        let msg = unsafe { util::cstr_ptr_to_mandatory_string(msg) }
            .context("'clap_host_preset_load::on_error()' called with an invalid msg parameter");
        match (location, load_key, msg) {
            (Ok(location), Ok(Some(load_key)), Ok(msg)) => {
                this.set_callback_error(format!(
                    "'clap_host_preset_load::on_error()' called for {location} with load key {load_key}, OS error \
                     code {os_error}, and the following error message: {msg}"
                ));
            }
            (Ok(location), Ok(None), Ok(msg)) => {
                this.set_callback_error(format!(
                    "'clap_host_preset_load::on_error()' called for {location} with no load key, OS error code \
                     {os_error}, and the following error message: {msg}"
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
        let this = unsafe { InstanceShared::from_clap_host(host) };

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
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_params::rescan()");
        log::trace!("'clap_host_params::rescan()' was called");

        if flags & CLAP_PARAM_RESCAN_VALUES != 0 {
            this.callback_sender.send(CallbackEvent::ParamsRescanValues).unwrap();
        }

        if flags & CLAP_PARAM_RESCAN_TEXT != 0 {
            this.callback_sender.send(CallbackEvent::ParamsRescanText).unwrap();
        }

        if flags & CLAP_PARAM_RESCAN_INFO != 0 {
            this.callback_sender.send(CallbackEvent::ParamsRescanInfo).unwrap();
        }

        if flags & CLAP_PARAM_RESCAN_ALL != 0 {
            if this.status.load() > PluginStatus::Activated {
                this.set_callback_error(
                    "'clap_host_params::rescan(CLAP_PARAM_RESCAN_ALL)' was called while the plugin is activated",
                );
            }

            this.callback_sender.send(CallbackEvent::ParamsRescanAll).unwrap();
        }
    }

    unsafe extern "C" fn ext_params_clear(host: *const clap_host, _param_id: clap_id, _flags: clap_param_clear_flags) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_params::clear()");
        log::debug!("TODO: Handle 'clap_host_params::clear()'");
    }

    unsafe extern "C" fn ext_params_request_flush(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_not_audio_thread("clap_host_params::request_flush()");
        log::trace!("'clap_host_params::request_flush()' was called");
        this.callback_sender.send(CallbackEvent::RequestFlush).unwrap();
    }

    unsafe extern "C" fn ext_state_mark_dirty(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_state::mark_dirty()");
        log::trace!("'clap_host_state::mark_dirty()' was called");
        this.callback_sender.send(CallbackEvent::StateMarkDirty).unwrap();
    }

    unsafe extern "C" fn ext_thread_check_is_main_thread(host: *const clap_host) -> bool {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };
        this.main_thread_id == std::thread::current().id()
    }

    unsafe extern "C" fn ext_thread_check_is_audio_thread(host: *const clap_host) -> bool {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };
        this.audio_thread_id.load() == Some(std::thread::current().id())
    }

    unsafe extern "C" fn ext_latency_changed(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        if this.status.load() != PluginStatus::Activating {
            this.set_callback_error(
                "'clap_host_latency::changed()' must only be called within 'clap_plugin::activate()'",
            );
        }

        this.assert_main_thread("clap_host_latency::changed()");
        log::trace!("'clap_host_latency::changed()' was called");
        this.callback_sender.send(CallbackEvent::LatencyChanged).unwrap();
    }

    unsafe extern "C" fn ext_tail_changed(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_audio_thread("clap_host_tail::changed()");
        log::trace!("'clap_host_tail::changed()' was called");
        this.callback_sender.send(CallbackEvent::TailChanged).unwrap();
    }

    unsafe extern "C" fn ext_voice_info_changed(host: *const clap_host) {
        check_null_ptr!(host, (*host).host_data);
        let this = unsafe { InstanceShared::from_clap_host(host) };

        this.assert_main_thread("clap_host_voice_info::changed()");
        log::trace!("'clap_host_voice_info::changed()' was called");
        this.callback_sender.send(CallbackEvent::VoiceInfoChanged).unwrap();
    }
}

unsafe impl Send for InstanceShared {}
unsafe impl Sync for InstanceShared {}
