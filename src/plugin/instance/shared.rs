use crate::panic::fail_test;
use crate::plugin::ext::Extension;
use crate::plugin::ext::audio_ports::AudioPorts;
use crate::plugin::ext::latency::Latency;
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::ext::params::Params;
use crate::plugin::ext::preset_load::PresetLoad;
use crate::plugin::ext::state::State;
use crate::plugin::ext::tail::Tail;
use crate::plugin::ext::thread_pool::ThreadPool;
use crate::plugin::ext::voice_info::VoiceInfo;
use crate::plugin::instance::{CallbackEvent, MainThreadTask, Plugin, PluginStatus};
use crate::plugin::preset_discovery::LocationValue;
use crate::plugin::util::{self, clap_call, object_tracker, validator_version};
use anyhow::{Context, Result};
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::preset_load::{CLAP_EXT_PRESET_LOAD, clap_host_preset_load};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state};
use clap_sys::ext::tail::{CLAP_EXT_TAIL, clap_host_tail};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::thread_pool::{CLAP_EXT_THREAD_POOL, clap_host_thread_pool};
use clap_sys::ext::voice_info::{CLAP_EXT_VOICE_INFO, clap_host_voice_info};
use clap_sys::factory::plugin_factory::clap_plugin_factory;
use clap_sys::factory::preset_discovery::clap_preset_discovery_location_kind;
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::clap_plugin;
use clap_sys::version::CLAP_VERSION;
use crossbeam::atomic::AtomicCell;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::ffi::{CStr, c_char, c_void};
use std::mem::offset_of;
use std::ptr::NonNull;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

/// Plugin instance state that is shared between the main thread, audio thread and any external unmanaged threads.
/// This struct also acts as the `clap_host` implementation for the plugin instance.
pub struct PluginShared {
    pub task_sender: Sender<MainThreadTask>,
    pub callback_sender: Sender<CallbackEvent>,
    pub callback_error: Mutex<Option<anyhow::Error>>,

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

    /// Whether the plugin is currently being called from within a process call. This is used to
    /// check that certain functions (like thread_pool::request_exec()) are called from the process function.
    pub is_currently_in_process_call: AtomicCell<bool>,

    clap_plugin: *const clap_plugin,
    clap_host: clap_host,
}

unsafe impl Send for PluginShared {}
unsafe impl Sync for PluginShared {}

impl Drop for PluginShared {
    fn drop(&mut self) {
        object_tracker::untrack(self.clap_host_ptr());
    }
}

impl PluginShared {
    /// Create a plugin instance and return the still uninitialized plugin. Returns an error if the
    /// plugin could not be created. The plugin instance will be registered with the host, and
    /// unregistered when this object is dropped again.
    ///
    /// # Safety
    /// The `factory` object must be valid.
    /// The caller must ensure that this is called from the OS main thread.
    pub unsafe fn create_plugin<'a>(factory: *const clap_plugin_factory, plugin_id: &CStr) -> Result<Plugin<'a>> {
        let (callback_sender, callback_receiver) = channel();
        let (task_sender, task_receiver) = channel();

        let shared = Arc::pin(PluginShared {
            task_sender,
            callback_sender,
            callback_error: Mutex::new(None),

            status: AtomicCell::new(PluginStatus::Uninitialized),
            main_thread_id: std::thread::current().id(),
            audio_thread_id: AtomicCell::new(None),
            requested_callback: AtomicCell::new(false),
            requested_restart: AtomicCell::new(false),
            is_currently_in_process_call: AtomicCell::new(false),

            clap_plugin: std::ptr::null(),
            clap_host: clap_host {
                clap_version: CLAP_VERSION,
                // This is populated with a pointer to the `Arc<Self>`'s data after creating the Arc
                host_data: std::ptr::null_mut(),
                name: c"clap-validator".as_ptr(),
                vendor: c"Robbert van der Helm".as_ptr(),
                url: c"https://github.com/free-audio/clap-validator".as_ptr(),
                version: validator_version().as_ptr(),
                get_extension: Some(Self::clap_get_extension),
                request_restart: Some(Self::clap_request_restart),
                request_process: Some(Self::clap_request_process),
                request_callback: Some(Self::clap_request_callback),
            },
        });

        // Now that the Arc is pinned in memory, we can store a pointer to it in the clap_host struct
        // so it can be retrieved in host callbacks
        unsafe {
            (&raw const shared.clap_host.host_data)
                .cast_mut()
                .write(&*shared as *const _ as *mut std::ffi::c_void);
        }

        // Add the clap_host to the tracker so it can be validated in callbacks
        object_tracker::track(shared.clap_host_ptr());

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

        Ok(Plugin {
            shared,
            callback_receiver,
            task_receiver,

            _library: std::marker::PhantomData,
            _thread: std::marker::PhantomData,
        })
    }

    /// Get a pointer to the `clap_host` struct for this plugin instance.
    pub fn clap_host_ptr(&self) -> *const clap_host {
        &self.clap_host as *const clap_host
    }

    /// Get a pointer to the plugin-provided `clap_plugin` struct for this plugin instance.
    pub fn clap_plugin_ptr(&self) -> *const clap_plugin {
        self.clap_plugin
    }

    /// Get a shared extension abstraction for the extension `T`, if the plugin supports this extension.
    pub fn get_extension<'a, T: Extension<&'a Self>>(&'a self) -> Option<T> {
        for id in T::IDS {
            let extension_ptr = unsafe {
                clap_call! { self.clap_plugin_ptr()=>get_extension(self.clap_plugin_ptr(), id.as_ptr()) }
            };

            if !extension_ptr.is_null() {
                return unsafe { Some(T::new(self, NonNull::new_unchecked(extension_ptr as *mut _))) };
            }
        }

        None
    }

    /// The plugin's current initialization status.
    pub fn status(&self) -> PluginStatus {
        self.status.load()
    }

    #[track_caller]
    fn wrap<R>(host: *const clap_host, function_name: &str, f: impl FnOnce(&Self) -> Result<R>) -> Option<R> {
        log::trace!("'{}' was called by the plugin", function_name);

        let state = unsafe {
            if let Err(e) = object_tracker::check(host) {
                fail_test!("{}: {}", function_name, e);
            }

            if (*host).host_data.wrapping_byte_add(offset_of!(Self, clap_host)) != host as *mut _ {
                fail_test!("{}: Malformed 'clap_host.host_data' pointer", function_name);
            }

            &*((*host).host_data as *const Self)
        };

        match f(state) {
            Ok(result) => Some(result),
            Err(error) => {
                let mut guard = state.callback_error.lock().unwrap();
                if guard.is_none() {
                    *guard = Some(error.context(function_name.to_string()));
                }

                None
            }
        }
    }

    /// Checks whether this is the main thread. If it is not, then an error indicating this can be
    /// retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_main_thread(&self) -> Result<()> {
        let current_thread_id = std::thread::current().id();

        anyhow::ensure!(
            current_thread_id == self.main_thread_id,
            "The function may only be called from the main thread (thread {:?}), but it was called from thread {:?}.",
            self.main_thread_id,
            current_thread_id
        );

        Ok(())
    }

    /// Checks whether this is the audio thread. If it is not, then an error indicating this can be
    /// retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_audio_thread(&self) -> Result<()> {
        let current_thread_id = std::thread::current().id();
        if self.audio_thread_id.load() != Some(current_thread_id) {
            if current_thread_id == self.main_thread_id {
                anyhow::bail!(
                    "This function may only be called from an audio thread, but it was called from the main thread."
                );
            } else {
                anyhow::bail!(
                    "This function may only be called from an audio thread, but it was called from an unknown thread \
                     ({:?}).",
                    current_thread_id
                );
            }
        }

        Ok(())
    }

    /// Checks whether this is **not** the audio thread. If it is, then an error indicating this can
    /// be retrieved using [`callback_error_check()`][Self::callback_error_check()]. Subsequent thread
    /// safety errors will not overwrite earlier ones.
    fn assert_not_audio_thread(&self) -> Result<()> {
        let current_thread_id = std::thread::current().id();
        if self.audio_thread_id.load() == Some(current_thread_id) {
            anyhow::bail!("This function was called from the audio thread, this is not allowed.");
        }
        Ok(())
    }

    /// Checks whether the plugin has the required extension(s). If it does not, then an error
    /// will be set. Subsequent errors will not overwrite earlier ones.
    fn assert_has_extension(&self, ids: &[&CStr]) -> Result<()> {
        anyhow::ensure!(
            self.status() != PluginStatus::Uninitialized,
            "Called while the plugin is uninitialized"
        );

        for id in ids {
            let extension_ptr = unsafe {
                clap_call! { self.clap_plugin_ptr()=>get_extension(self.clap_plugin_ptr(), id.as_ptr()) }
            };

            if !extension_ptr.is_null() {
                return Ok(()); // found it!
            }
        }

        anyhow::bail!("Plugin does not implement extension {}", ids[0].to_string_lossy());
    }
}

// Extensions
impl PluginShared {
    const EXT_AUDIO_PORTS: clap_host_audio_ports = clap_host_audio_ports {
        is_rescan_flag_supported: Some(Self::ext_audio_ports_is_rescan_flag_supported),
        rescan: Some(Self::ext_audio_ports_rescan),
    };

    const EXT_NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
        supported_dialects: Some(Self::ext_note_ports_supported_dialects),
        rescan: Some(Self::ext_note_ports_rescan),
    };

    const EXT_PRESET_LOAD: clap_host_preset_load = clap_host_preset_load {
        on_error: Some(Self::ext_preset_load_on_error),
        loaded: Some(Self::ext_preset_load_loaded),
    };

    const EXT_PARAMS: clap_host_params = clap_host_params {
        rescan: Some(Self::ext_params_rescan),
        clear: Some(Self::ext_params_clear),
        request_flush: Some(Self::ext_params_request_flush),
    };

    const EXT_STATE: clap_host_state = clap_host_state {
        mark_dirty: Some(Self::ext_state_mark_dirty),
    };

    const EXT_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
        is_audio_thread: Some(Self::ext_thread_check_is_audio_thread),
        is_main_thread: Some(Self::ext_thread_check_is_main_thread),
    };

    const EXT_THREAD_POOL: clap_host_thread_pool = clap_host_thread_pool {
        request_exec: Some(Self::ext_thread_pool_request_exec),
    };

    const EXT_LATENCY: clap_host_latency = clap_host_latency {
        changed: Some(Self::ext_latency_changed),
    };

    const EXT_TAIL: clap_host_tail = clap_host_tail {
        changed: Some(Self::ext_tail_changed),
    };

    const EXT_VOICE_INFO: clap_host_voice_info = clap_host_voice_info {
        changed: Some(Self::ext_voice_info_changed),
    };

    unsafe extern "C" fn clap_get_extension(host: *const clap_host, extension_id: *const c_char) -> *const c_void {
        // Right now there's no way to have the host only expose certain extensions. We can always
        // add that when test cases need it.
        Self::wrap(host, "clap_host::get_extension", |_| {
            if extension_id.is_null() {
                anyhow::bail!("Null extension ID");
            }

            let extension_id_cstr = unsafe { CStr::from_ptr(extension_id) };
            let extension_ptr = if extension_id_cstr == CLAP_EXT_AUDIO_PORTS {
                &Self::EXT_AUDIO_PORTS as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_NOTE_PORTS {
                &Self::EXT_NOTE_PORTS as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_PRESET_LOAD {
                &Self::EXT_PRESET_LOAD as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_PARAMS {
                &Self::EXT_PARAMS as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_STATE {
                &Self::EXT_STATE as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_THREAD_CHECK {
                &Self::EXT_THREAD_CHECK as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_THREAD_POOL {
                &Self::EXT_THREAD_POOL as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_LATENCY {
                &Self::EXT_LATENCY as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_TAIL {
                &Self::EXT_TAIL as *const _ as *const c_void
            } else if extension_id_cstr == CLAP_EXT_VOICE_INFO {
                &Self::EXT_VOICE_INFO as *const _ as *const c_void
            } else {
                std::ptr::null()
            };

            Ok(extension_ptr)
        })
        .unwrap_or_default()
    }

    unsafe extern "C" fn clap_request_restart(host: *const clap_host) {
        Self::wrap(host, "clap_host::request_restart", |this| {
            this.requested_restart.store(true);
            Ok(())
        });
    }

    unsafe extern "C" fn clap_request_process(host: *const clap_host) {
        Self::wrap(host, "clap_host::request_process", |this| {
            this.callback_sender.send(CallbackEvent::RequestProcess).unwrap();
            Ok(())
        });
    }

    unsafe extern "C" fn clap_request_callback(host: *const clap_host) {
        Self::wrap(host, "clap_host::request_callback", |this| {
            this.requested_callback.store(true);
            this.task_sender.send(MainThreadTask::CallbackRequest).unwrap();
            Ok(())
        });
    }

    unsafe extern "C" fn ext_audio_ports_is_rescan_flag_supported(host: *const clap_host, _flag: u32) -> bool {
        Self::wrap(host, "clap_host_audio_ports::is_rescan_flag_supported", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(AudioPorts::IDS)?;
            Ok(true)
        })
        .unwrap_or(false)
    }

    unsafe extern "C" fn ext_audio_ports_rescan(host: *const clap_host, flags: u32) {
        Self::wrap(host, "clap_host_audio_ports::rescan", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(AudioPorts::IDS)?;

            if flags & CLAP_AUDIO_PORTS_RESCAN_NAMES != 0 {
                this.callback_sender.send(CallbackEvent::AudioPortsRescanNames).unwrap();
            }

            if flags & !CLAP_AUDIO_PORTS_RESCAN_NAMES != 0 {
                anyhow::ensure!(
                    this.status() <= PluginStatus::Activated,
                    "Called while the plugin is active"
                );

                this.callback_sender.send(CallbackEvent::AudioPortsRescanAll).unwrap();
            }

            Ok(())
        });
    }

    unsafe extern "C" fn ext_note_ports_supported_dialects(host: *const clap_host) -> clap_note_dialect {
        Self::wrap(host, "clap_host_note_ports::supported_dialects", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(NotePorts::IDS)?;
            Ok(CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE)
        })
        .unwrap_or(0)
    }

    unsafe extern "C" fn ext_note_ports_rescan(host: *const clap_host, flags: u32) {
        Self::wrap(host, "clap_host_note_ports::rescan", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(NotePorts::IDS)?;

            if flags & CLAP_NOTE_PORTS_RESCAN_NAMES != 0 {
                this.callback_sender.send(CallbackEvent::NotePortsRescanNames).unwrap();
            }

            if flags & CLAP_NOTE_PORTS_RESCAN_ALL != 0 {
                anyhow::ensure!(
                    this.status() <= PluginStatus::Activated,
                    "Called while the plugin is active"
                );

                this.callback_sender.send(CallbackEvent::NotePortsRescanAll).unwrap();
            }

            Ok(())
        });
    }

    unsafe extern "C" fn ext_preset_load_on_error(
        host: *const clap_host,
        location_kind: clap_preset_discovery_location_kind,
        location: *const c_char,
        load_key: *const c_char,
        os_error: i32,
        msg: *const c_char,
    ) {
        Self::wrap(host, "clap_host_preset_load::on_error", |this| -> Result<()> {
            this.assert_main_thread()?;
            this.assert_has_extension(PresetLoad::IDS)?;

            let location = unsafe { LocationValue::new(location_kind, location) }
                .context("'clap_host_preset_load::on_error()' called with invalid location parameters")?;
            let load_key = unsafe { util::cstr_ptr_to_optional_string(load_key) }
                .context("'clap_host_preset_load::on_error()' called with an invalid load_key parameter")?;
            let msg = unsafe { util::cstr_ptr_to_mandatory_string(msg) }
                .context("'clap_host_preset_load::on_error()' called with an invalid msg parameter")?;

            if let Some(load_key) = &load_key {
                anyhow::bail!(
                    "Called for {location} with load key {load_key}, OS error code {os_error}, and the following \
                     error message: {msg}"
                );
            } else {
                anyhow::bail!(
                    "Called for {location} with no load key, OS error code {os_error}, and the following error \
                     message: {msg}"
                );
            }
        });
    }

    unsafe extern "C" fn ext_preset_load_loaded(
        host: *const clap_host,
        location_kind: clap_preset_discovery_location_kind,
        location: *const c_char,
        load_key: *const c_char,
    ) {
        Self::wrap(host, "clap_host_preset_load::loaded", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(PresetLoad::IDS)?;

            let _location = unsafe { LocationValue::new(location_kind, location) }
                .context("'Called with invalid location parameters")?;
            let _load_key = unsafe { util::cstr_ptr_to_optional_string(load_key) }
                .context("'Called with an invalid load_key parameter")?;

            log::debug!("TODO: Handle 'clap_host_preset_load::loaded()'");
            Ok(())
        });
    }

    unsafe extern "C" fn ext_params_rescan(host: *const clap_host, flags: clap_param_rescan_flags) {
        Self::wrap(host, "clap_host_params::rescan", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(Params::IDS)?;

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
                anyhow::ensure!(
                    this.status() <= PluginStatus::Activated,
                    "Called while the plugin is active"
                );

                this.callback_sender.send(CallbackEvent::ParamsRescanAll).unwrap();
            }

            Ok(())
        });
    }

    unsafe extern "C" fn ext_params_clear(host: *const clap_host, _param_id: clap_id, _flags: clap_param_clear_flags) {
        Self::wrap(host, "clap_host_params::clear", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(Params::IDS)?;
            log::debug!("TODO: Handle 'clap_host_params::clear()'");
            Ok(())
        });
    }

    unsafe extern "C" fn ext_params_request_flush(host: *const clap_host) {
        Self::wrap(host, "clap_host_params::request_flush", |this| {
            this.assert_not_audio_thread()?;
            this.assert_has_extension(Params::IDS)?;
            this.callback_sender.send(CallbackEvent::RequestFlush).unwrap();
            Ok(())
        });
    }

    unsafe extern "C" fn ext_state_mark_dirty(host: *const clap_host) {
        Self::wrap(host, "clap_host_state::mark_dirty", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(State::IDS)?;
            this.callback_sender.send(CallbackEvent::StateMarkDirty).unwrap();
            Ok(())
        });
    }

    unsafe extern "C" fn ext_thread_check_is_main_thread(host: *const clap_host) -> bool {
        Self::wrap(host, "clap_host_thread_check::is_main_thread", |this| {
            Ok(this.main_thread_id == std::thread::current().id())
        })
        .unwrap_or(false)
    }

    unsafe extern "C" fn ext_thread_check_is_audio_thread(host: *const clap_host) -> bool {
        Self::wrap(host, "clap_host_thread_check::is_audio_thread", |this| {
            Ok(this.audio_thread_id.load() == Some(std::thread::current().id()))
        })
        .unwrap_or(false)
    }

    unsafe extern "C" fn ext_latency_changed(host: *const clap_host) {
        Self::wrap(host, "clap_host_latency::changed", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(Latency::IDS)?;

            anyhow::ensure!(
                this.status() == PluginStatus::Activating,
                "Must only be called within 'clap_plugin::activate'"
            );

            this.callback_sender.send(CallbackEvent::LatencyChanged).unwrap();

            Ok(())
        });
    }

    unsafe extern "C" fn ext_tail_changed(host: *const clap_host) {
        Self::wrap(host, "clap_host_tail::changed", |this| {
            this.assert_audio_thread()?;
            this.assert_has_extension(Tail::IDS)?;
            this.callback_sender.send(CallbackEvent::TailChanged).unwrap();
            Ok(())
        });
    }

    unsafe extern "C" fn ext_voice_info_changed(host: *const clap_host) {
        Self::wrap(host, "clap_host_voice_info::changed", |this| {
            this.assert_main_thread()?;
            this.assert_has_extension(VoiceInfo::IDS)?;
            this.callback_sender.send(CallbackEvent::VoiceInfoChanged).unwrap();
            Ok(())
        });
    }

    unsafe extern "C" fn ext_thread_pool_request_exec(host: *const clap_host, num_tasks: u32) -> bool {
        Self::wrap(host, "clap_host_thread_pool::request_exec", |this| {
            this.assert_audio_thread()?;
            this.assert_has_extension(ThreadPool::IDS)?;

            // Ensure this is called from within the process() function
            // We already checked that we're on the audio thread, so this is sufficient
            anyhow::ensure!(
                this.is_currently_in_process_call.load(),
                "May only be called from within the audio thread's 'clap_plugin::process' function."
            );

            let extension = this.get_extension::<ThreadPool>().unwrap();
            (0..num_tasks).into_par_iter().for_each(|index| extension.exec(index));
            Ok(true)
        })
        .unwrap_or(false)
    }
}
