//! Tests surrounding state handling.

use super::PluginTestCase;
use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::params::Params;
use crate::plugin::ext::state::State;
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, Event, ProcessScope};
use crate::tests::plugin::params::generate_param_diff;
use crate::tests::rng::{ParamFuzzer, new_prng};
use crate::tests::{TestCase, TestStatus};
use anyhow::{Context, Result};
use clap_sys::id::clap_id;
use rand::RngExt;
use std::collections::BTreeMap;
use std::io::Write;

/// The file name we'll use to dump the expected state when a test fails.
const EXPECTED_STATE_FILE_NAME: &str = "state-expected";
/// The file name we'll use to dump the actual state when a test fails.
const ACTUAL_STATE_FILE_NAME: &str = "state-actual";

const BUFFER_SIZE: u32 = 512;

/// The test for `PluginTestCase::StateInvalidEmpty`.
pub fn test_state_invalid_empty(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;

    plugin.init().context("Error during initialization")?;
    let state = match plugin.get_extension::<State>() {
        Some(state) => state,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from("The plugin does not implement the 'state' extension.")),
            });
        }
    };

    let result = state.load(&[]);

    plugin.poll_callback(|_| Ok(()))?;

    match result {
        Ok(_) => Ok(TestStatus::Warning {
            details: Some(String::from(
                "The plugin returned true when 'clap_plugin_state::load()' was called when an empty state, this is \
                 likely a bug.",
            )),
        }),
        Err(_) => Ok(TestStatus::Success { details: None }),
    }
}

/// The test for `PluginTestCase::StateInvalidRandom`.
pub fn test_state_invalid_random(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;

    plugin.init().context("Error during initialization")?;

    let state = match plugin.get_extension::<State>() {
        Some(state) => state,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from("The plugin does not implement the 'state' extension.")),
            });
        }
    };

    plugin.poll_callback(|_| Ok(()))?;

    let mut random_data = vec![0u8; 1024 * 1024];
    let mut succeeded = false;

    for _ in 0..3 {
        prng.fill(&mut random_data[..]);
        succeeded |= state.load(&random_data).is_ok();
    }

    plugin.poll_callback(|_| Ok(()))?;

    match succeeded {
        false => Ok(TestStatus::Success { details: None }),
        true => Ok(TestStatus::Warning {
            details: Some(String::from(
                "The plugin loaded random bytes successfully, which is unexpected, but the plugin did not crash.",
            )),
        }),
    }
}

/// The test for `PluginTestCase::StateReproducibilityNullCookies` and `PluginTestCase::StateReproducibilityBasic`.
/// See the description of these test for a detailed explanation, but we essentially check if saving a loaded state results in the
/// same state file, and whether a plugin's parameters are the same after loading the state.
///
/// The `zero_out_cookies` parameter offers an alternative on this test that sends parameter change
/// events with all cookies set to null pointers. The plugin should behave identically when this
/// happens.
pub fn test_state_reproducibility(
    library: &PluginLibrary,
    plugin_id: &str,
    zero_out_cookies: bool,
    buffered_streams: bool,
    binary_equality: bool,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;

    // We'll drop and reinitialize the plugin later
    let (expected_state, expected_param_values) = {
        plugin.init().context("Error during initialization")?;

        let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
            Some(audio_ports) => audio_ports
                .config()
                .context("Error while querying 'audio-ports' IO configuration")?,
            None => AudioPortConfig::default(),
        };

        let params = match plugin.get_extension::<Params>() {
            Some(params) => params,
            None => {
                return Ok(TestStatus::Skipped {
                    details: Some(String::from("The plugin does not implement the 'params' extension.")),
                });
            }
        };
        let state = match plugin.get_extension::<State>() {
            Some(state) => state,
            None => {
                return Ok(TestStatus::Skipped {
                    details: Some(String::from("The plugin does not implement the 'state' extension.")),
                });
            }
        };

        plugin.poll_callback(|_| Ok(()))?;

        let param_infos = params
            .info()
            .context("Failure while fetching the plugin's parameters")?;

        // We can't compare the values from these events direclty as the plugin
        // may round the values during the parameter set
        let param_fuzzer = ParamFuzzer::new(&param_infos);
        let mut random_param_set_events: Vec<_> = param_fuzzer.randomize_params_at(&mut prng, 0).collect();

        // This is a variation on the test that checks whether the plugin handles null
        // pointer cookies correctly
        if zero_out_cookies {
            for event in &mut random_param_set_events {
                match event {
                    Event::ParamValue(event) => {
                        event.cookie = std::ptr::null_mut();
                    }
                    event => {
                        panic!("Unexpected event {event:?}")
                    }
                }
            }
        }

        plugin.on_audio_thread(|plugin| {
            let mut buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
            let mut process = ProcessScope::new(&plugin, &mut buffers)?;

            process.audio_buffers().fill_white_noise(&mut prng);
            process.add_events(random_param_set_events);
            process.run()
        })?;

        plugin.poll_callback(|_| Ok(()))?;

        // We'll check that the plugin has these sames values after reloading the state. These
        // values are rounded to the tenth decimal to provide some leeway in the serialization and
        // deserializatoin process.
        let expected_param_values: BTreeMap<clap_id, f64> = param_infos
            .keys()
            .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
            .collect::<Result<BTreeMap<clap_id, f64>>>()?;

        let expected_state = if buffered_streams {
            state.save_buffered(23)?
        } else {
            state.save()?
        };

        plugin.poll_callback(|_| Ok(()))?;

        (expected_state, expected_param_values)
    };

    // Now we'll recreate the plugin instance, load the state, and check whether the values are
    // consistent and whether saving the state again results in an idential state file. This ends up
    // being a bit of a lengthy test case because of this multiple initialization. Before
    // continueing, we'll make sure the first plugin instance no longer exists.
    drop(plugin);

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance a second time")?;
    plugin
        .init()
        .context("Error while initializing the second plugin instance")?;

    let params = match plugin.get_extension::<Params>() {
        Some(params) => params,
        None => {
            // I sure hope that no plugin will ever hit this
            return Ok(TestStatus::Skipped {
                details: Some(String::from("The plugin does not implement the 'params' extension.")),
            });
        }
    };

    let state = match plugin.get_extension::<State>() {
        Some(state) => state,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin's second instance does not implement the 'state' extension.",
                )),
            });
        }
    };

    plugin.poll_callback(|_| Ok(()))?;

    if buffered_streams {
        // This is a buffered load that only loads 17 bytes at a time. Why 17? Because.
        state.load_buffered(&expected_state, 17)?;
    } else {
        state.load(&expected_state)?;
    }

    plugin.poll_callback(|_| Ok(()))?;

    let actual_param_values: BTreeMap<clap_id, f64> = expected_param_values
        .keys()
        .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
        .collect::<Result<BTreeMap<clap_id, f64>>>()?;

    let test = if zero_out_cookies {
        PluginTestCase::StateReproducibilityNullCookies
    } else {
        PluginTestCase::StateReproducibilityBasic
    };

    if let Some(diff) = generate_param_diff(&actual_param_values, &expected_param_values, &params)? {
        anyhow::bail!(
            "After reloading the state, these parameter values do not match the old values: \n{}",
            diff
        );
    }

    plugin.poll_callback(|_| Ok(()))?;

    // Now for the moment of truth
    let actual_state = state.save()?;

    plugin.poll_callback(|_| Ok(()))?;

    if !binary_equality || actual_state == expected_state {
        Ok(TestStatus::Success { details: None })
    } else {
        let (expected_state_file_path, mut expected_state_file) =
            test.temporary_file(plugin_id, EXPECTED_STATE_FILE_NAME)?;
        let (actual_state_file_path, mut actual_state_file) = test.temporary_file(plugin_id, ACTUAL_STATE_FILE_NAME)?;

        expected_state_file.write_all(&expected_state)?;
        actual_state_file.write_all(&actual_state)?;

        Ok(TestStatus::Failed {
            details: Some(format!(
                "The saved state after loading differs from the original saved state. \nExpected: '{}'. \nActual: \
                 '{}'.",
                expected_state_file_path.display(),
                actual_state_file_path.display(),
            )),
        })
    }
}
