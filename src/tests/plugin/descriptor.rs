//! Tests surrounding plugin features.

use anyhow::{Context, Result};
use clap_sys::ext::*;
use clap_sys::plugin_features::{
    CLAP_PLUGIN_FEATURE_ANALYZER, CLAP_PLUGIN_FEATURE_AUDIO_EFFECT, CLAP_PLUGIN_FEATURE_INSTRUMENT,
    CLAP_PLUGIN_FEATURE_NOTE_DETECTOR, CLAP_PLUGIN_FEATURE_NOTE_EFFECT,
};
use std::collections::HashSet;
use std::ffi::CStr;

use crate::plugin::host::Host;
use crate::plugin::instance::Plugin;
use crate::plugin::library::PluginLibrary;
use crate::tests::TestStatus;
use crate::util::unsafe_clap_call;

/// Verifies that the descriptor stored in the factory and the descriptor stored on the plugin
/// object are equivalent.
pub fn test_consistency(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let metadata = library.metadata().with_context(|| {
        format!(
            "Could not fetch plugin metadata for '{}'",
            library.plugin_path().display()
        )
    })?;
    let factory_descriptor = metadata
        .plugins
        .into_iter()
        .find(|plugin_meta| plugin_meta.id == plugin_id)
        .expect("Incorrect plugin ID for metadata query, this is a bug in clap-validator");

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host)
        .context("Could not create the plugin instance")?;
    let plugin_descriptor = plugin.descriptor()?;

    if plugin_descriptor == factory_descriptor {
        Ok(TestStatus::Success { details: None })
    } else {
        Ok(TestStatus::Failed {
            details: Some(format!(
                "The 'clap_plugin_descriptor' stored on '{plugin_id}'s 'clap_plugin' object \
                 contains different values than the one returned by the factory."
            )),
        })
    }
}

/// Check that all of the required methods on `clap_plugin` are non-null.
pub fn test_methods_non_null(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    /// SAFETY:
    /// Assumes that extension 'T' is a repr(C) struct with function pointers only.
    unsafe fn check_extension<T: Copy>(plugin: &Plugin<'_>, extension: &CStr) -> Result<()> {
        let extension_ptr = unsafe_clap_call! { plugin.as_ptr()=>get_extension(plugin.as_ptr(), extension.as_ptr()) };
        if extension_ptr.is_null() {
            return Ok(());
        }

        let methods = std::slice::from_raw_parts(
            extension_ptr as *const *const (),
            std::mem::size_of::<T>() / std::mem::size_of::<*const ()>(),
        );

        for &method in methods.iter() {
            if method.is_null() {
                anyhow::bail!(
                    "Extension '{}' has a method that is null.",
                    extension.to_string_lossy()
                );
            }
        }

        Ok(())
    }

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host)
        .context("Could not create the plugin instance")?;

    // Check `clap_plugin` methods.
    // SAFETY: `plugin.as_ptr()` is guaranteed to be a valid pointer as long as `plugin` is alive.
    unsafe {
        let plugin = plugin.as_ptr();

        anyhow::ensure!((*plugin).init.is_some(), "clap_plugin::init is null");
        anyhow::ensure!((*plugin).destroy.is_some(), "clap_plugin::destroy is null");
        anyhow::ensure!((*plugin).process.is_some(), "clap_plugin::process is null");
        anyhow::ensure!((*plugin).reset.is_some(), "clap_plugin::reset is null");
        anyhow::ensure!(
            (*plugin).get_extension.is_some(),
            "clap_plugin::get_extension is null"
        );
        anyhow::ensure!(
            (*plugin).on_main_thread.is_some(),
            "clap_plugin::on_main_thread is null"
        );
        anyhow::ensure!(
            (*plugin).activate.is_some(),
            "clap_plugin::activate is null"
        );
        anyhow::ensure!(
            (*plugin).deactivate.is_some(),
            "clap_plugin::deactivate is null"
        );
        anyhow::ensure!(
            (*plugin).start_processing.is_some(),
            "clap_plugin::start_processing is null"
        );
        anyhow::ensure!(
            (*plugin).stop_processing.is_some(),
            "clap_plugin::stop_processing is null"
        );
    }

    plugin.init().context("Error during initialization")?;

    // Check known extensions.
    unsafe {
        check_extension::<ambisonic::clap_plugin_ambisonic>(
            &plugin,
            ambisonic::CLAP_EXT_AMBISONIC,
        )?;
        check_extension::<audio_ports::clap_plugin_audio_ports>(
            &plugin,
            audio_ports::CLAP_EXT_AUDIO_PORTS,
        )?;
        check_extension::<audio_ports_activation::clap_plugin_audio_ports_activation>(
            &plugin,
            audio_ports_activation::CLAP_EXT_AUDIO_PORTS_ACTIVATION,
        )?;
        check_extension::<audio_ports_config::clap_plugin_audio_ports_config_info>(
            &plugin,
            audio_ports_config::CLAP_EXT_AUDIO_PORTS_CONFIG_INFO,
        )?;
        check_extension::<audio_ports_config::clap_plugin_audio_ports_config>(
            &plugin,
            audio_ports_config::CLAP_EXT_AUDIO_PORTS_CONFIG,
        )?;
        check_extension::<configurable_audio_ports::clap_plugin_configurable_audio_ports>(
            &plugin,
            configurable_audio_ports::CLAP_EXT_CONFIGURABLE_AUDIO_PORTS,
        )?;
        check_extension::<context_menu::clap_plugin_context_menu>(
            &plugin,
            context_menu::CLAP_EXT_CONTEXT_MENU,
        )?;
        check_extension::<gui::clap_plugin_gui>(&plugin, gui::CLAP_EXT_GUI)?;
        check_extension::<note_name::clap_plugin_note_name>(
            &plugin,
            note_name::CLAP_EXT_NOTE_NAME,
        )?;
        check_extension::<note_ports::clap_plugin_note_ports>(
            &plugin,
            note_ports::CLAP_EXT_NOTE_PORTS,
        )?;
        check_extension::<params::clap_plugin_params>(&plugin, params::CLAP_EXT_PARAMS)?;
        check_extension::<param_indication::clap_plugin_param_indication>(
            &plugin,
            param_indication::CLAP_EXT_PARAM_INDICATION,
        )?;
        check_extension::<preset_load::clap_plugin_preset_load>(
            &plugin,
            preset_load::CLAP_EXT_PRESET_LOAD,
        )?;
        check_extension::<state::clap_plugin_state>(&plugin, state::CLAP_EXT_STATE)?;
        check_extension::<state_context::clap_plugin_state_context>(
            &plugin,
            state_context::CLAP_EXT_STATE_CONTEXT,
        )?;
        check_extension::<render::clap_plugin_render>(&plugin, render::CLAP_EXT_RENDER)?;
        check_extension::<remote_controls::clap_host_remote_controls>(
            &plugin,
            remote_controls::CLAP_EXT_REMOTE_CONTROLS,
        )?;
        check_extension::<surround::clap_plugin_surround>(&plugin, surround::CLAP_EXT_SURROUND)?;
        check_extension::<latency::clap_plugin_latency>(&plugin, latency::CLAP_EXT_LATENCY)?;
        check_extension::<tail::clap_plugin_tail>(&plugin, tail::CLAP_EXT_TAIL)?;
        check_extension::<posix_fd_support::clap_plugin_posix_fd_support>(
            &plugin,
            posix_fd_support::CLAP_EXT_POSIX_FD_SUPPORT,
        )?;
        check_extension::<timer_support::clap_plugin_timer_support>(
            &plugin,
            timer_support::CLAP_EXT_TIMER_SUPPORT,
        )?;
        check_extension::<thread_pool::clap_plugin_thread_pool>(
            &plugin,
            thread_pool::CLAP_EXT_THREAD_POOL,
        )?;
        check_extension::<voice_info::clap_plugin_voice_info>(
            &plugin,
            voice_info::CLAP_EXT_VOICE_INFO,
        )?;
    }

    Ok(TestStatus::Success { details: None })
}

/// Check whether the plugin's categories are consistent. Currently this just makes sure that the
/// plugin has one of the four main plugin category features.
pub fn test_features_categories(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let features = plugin_features(library, plugin_id)?;

    // These are stored in the bindings as C-compatible null terminated strings, but we'll need them
    // as regular string slices so we can compare them to
    let instrument_feature = CLAP_PLUGIN_FEATURE_INSTRUMENT.to_str().unwrap();
    let audio_effect_feature = CLAP_PLUGIN_FEATURE_AUDIO_EFFECT.to_str().unwrap();
    let note_detector_feature = CLAP_PLUGIN_FEATURE_NOTE_DETECTOR.to_str().unwrap();
    let note_effect_feature = CLAP_PLUGIN_FEATURE_NOTE_EFFECT.to_str().unwrap();
    let analyzer_feature = CLAP_PLUGIN_FEATURE_ANALYZER.to_str().unwrap();

    let has_main_category = features.iter().any(|feature| -> bool {
        feature == instrument_feature
            || feature == audio_effect_feature
            || feature == note_detector_feature
            || feature == note_effect_feature
            || feature == analyzer_feature
    });

    if has_main_category {
        Ok(TestStatus::Success { details: None })
    } else {
        anyhow::bail!(
            "The plugin needs to have at least one of thw following plugin category features: \
             \"{instrument_feature}\", \"{audio_effect_feature}\", \"{note_effect_feature}\", or \
             \"{analyzer_feature}\"."
        )
    }
}

/// Confirm that the plugin does not have any duplicate features.
pub fn test_features_duplicates(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut features = plugin_features(library, plugin_id)?;
    let unique_features: HashSet<&str> = features.iter().map(|feature| feature.as_str()).collect();

    if unique_features.len() == features.len() {
        Ok(TestStatus::Success { details: None })
    } else {
        // We'll sort the features first to make spotting the duplicates easier
        features.sort_unstable();

        anyhow::bail!("The plugin has duplicate features: {features:?}")
    }
}

/// Get the feature vector for a plugin in the library. Returns `None` if the plugin ID does not
/// exist in the library.
fn plugin_features(library: &PluginLibrary, plugin_id: &str) -> Result<Vec<String>> {
    library
        .metadata()
        .with_context(|| {
            format!(
                "Could not fetch plugin metadata for '{}'",
                library.plugin_path().display()
            )
        })
        .and_then(|metadata| {
            metadata
                .plugins
                .into_iter()
                .find(|plugin_meta| plugin_meta.id == plugin_id)
                .context("Incorrect plugin ID for metadata query, this is a bug in clap-validator")
        })
        .map(|metadata| metadata.features)
}
