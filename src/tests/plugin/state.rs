//! Tests surrounding state handling.

use anyhow::{Context, Result};
use clap_sys::id::clap_id;
use rand::Rng;
use std::collections::BTreeMap;
use std::io::Write;

use super::PluginTestCase;
use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::params::Params;
use crate::plugin::ext::state::State;
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, Event, EventQueue, ProcessScope};
use crate::tests::plugin::params::param_compare_approx;
use crate::tests::rng::{ParamFuzzer, new_prng};
use crate::tests::{TestCase, TestStatus};

/// The file name we'll use to dump the expected state when a test fails.
const EXPECTED_STATE_FILE_NAME: &str = "state-expected";
/// The file name we'll use to dump the actual state when a test fails.
const ACTUAL_STATE_FILE_NAME: &str = "state-actual";
/// The file name we'll use to dump parameter diffs when a test fails.
const PARAM_DIFF_FILE_NAME: &str = "param-diff.csv";

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

    plugin.handle_callback().context("An error occured during a callback")?;

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

    plugin.handle_callback().context("An error occured during a callback")?;

    let mut random_data = vec![0u8; 1024 * 1024];
    let mut succeeded = false;

    for _ in 0..3 {
        prng.fill(&mut random_data[..]);
        succeeded |= state.load(&random_data).is_ok();
    }

    plugin.handle_callback().context("An error occured during a callback")?;

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
pub fn test_state_reproducibility_basic(
    library: &PluginLibrary,
    plugin_id: &str,
    zero_out_cookies: bool,
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

        plugin.handle_callback().context("An error occured during a callback")?;

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

            process.audio_buffers().randomize(&mut prng);
            process.input_queue().add_events(random_param_set_events);
            process.run()
        })?;

        // We'll check that the plugin has these sames values after reloading the state. These
        // values are rounded to the tenth decimal to provide some leeway in the serialization and
        // deserializatoin process.
        let expected_param_values: BTreeMap<clap_id, f64> = param_infos
            .keys()
            .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
            .collect::<Result<BTreeMap<clap_id, f64>>>()?;

        let expected_state = state.save()?;

        plugin.handle_callback().context("An error occured during a callback")?;

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

    plugin.handle_callback().context("An error occured during a callback")?;

    state.load(&expected_state)?;

    plugin.handle_callback().context("An error occured during a callback")?;

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
        let (param_diff_file_path, mut param_diff_file) = test.temporary_file(plugin_id, PARAM_DIFF_FILE_NAME)?;
        param_diff_file.write_all(diff.as_bytes())?;

        anyhow::bail!(
            "After reloading the state, the plugin's parameter values do not match the old values when queried \
             through 'clap_plugin_params::get()'. \nDiff: '{}'.",
            param_diff_file_path.display(),
        );
    }

    // Now for the moment of truth
    let actual_state = state.save()?;

    plugin.handle_callback().context("An error occured during a callback")?;

    if actual_state == expected_state {
        Ok(TestStatus::Success { details: None })
    } else {
        let (expected_state_file_path, mut expected_state_file) =
            test.temporary_file(plugin_id, EXPECTED_STATE_FILE_NAME)?;
        let (actual_state_file_path, mut actual_state_file) = test.temporary_file(plugin_id, ACTUAL_STATE_FILE_NAME)?;

        expected_state_file.write_all(&expected_state)?;
        actual_state_file.write_all(&actual_state)?;

        Ok(TestStatus::Warning {
            details: Some(format!(
                "The saved state after loading differs from the original saved state. \nExpected: '{}'. \nActual: \
                 '{}'.",
                expected_state_file_path.display(),
                actual_state_file_path.display(),
            )),
        })
    }
}

/// The test for `PluginTestCase::StateReproducibilityFlush`.
pub fn test_state_reproducibility_flush(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;

    // We'll drop and reinitialize the plugin later. This first pass sets the values using the flush
    // function, and the second pass we'll compare this to uses the process function. We'll reuse
    // the parameter set events, but the cookies need to be updated first or they'll point to old
    // data.
    let (expected_state, old_random_param_set_events, expected_param_values) = {
        plugin.init().context("Error during initialization")?;

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

        plugin.handle_callback().context("An error occured during a callback")?;

        let param_infos = params
            .info()
            .context("Failure while fetching the plugin's parameters")?;

        // Make sure the flush does _something_. If nothing changes, then the plugin has not
        // implemented flush.
        let initial_param_values: BTreeMap<clap_id, f64> = param_infos
            .keys()
            .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
            .collect::<Result<BTreeMap<clap_id, f64>>>()?;

        // The same param set events will be passed to the flush function in this pass and to the
        // process fuction in the second pass
        let param_fuzzer = ParamFuzzer::new(&param_infos);
        let random_param_set_events: Vec<_> = param_fuzzer.randomize_params_at(&mut prng, 0).collect();

        let input_events = EventQueue::new();
        let output_events = EventQueue::new();

        input_events.add_events(random_param_set_events.clone());
        params.flush(&input_events, &output_events);

        plugin.handle_callback().context("An error occured during a callback")?;

        // We'll compare against these values in that second pass
        let expected_param_values: BTreeMap<clap_id, f64> = param_infos
            .keys()
            .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
            .collect::<Result<BTreeMap<clap_id, f64>>>()?;
        let expected_state = state.save()?;

        plugin.handle_callback().context("An error occured during a callback")?;

        // Plugins with no parameters at all should of course not trigger this error
        if expected_param_values == initial_param_values && !random_param_set_events.is_empty() {
            anyhow::bail!(
                "'clap_plugin_params::flush()' has been called with random parameter values, but the plugin's \
                 reported parameter values have not changed."
            )
        }

        (expected_state, random_param_set_events, expected_param_values)
    };

    // This works the same as the basic state reproducibility test, except that we load the values
    // using the process funciton instead of loading the state
    drop(plugin);

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance a second time")?;
    plugin
        .init()
        .context("Error while initializing the second plugin instance")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let params = match plugin.get_extension::<Params>() {
        Some(params) => params,
        None => {
            // I sure hope that no plugin will eer hit this
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin's second instance does not implement the 'params' extension.",
                )),
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

    plugin.handle_callback().context("An error occured during a callback")?;

    // NOTE: We can reuse random parameter set events, except that the cookie pointers may be
    //       different if the plugin uses those. So we need to update these cookies first.
    let param_infos = params
        .info()
        .context("Failure while fetching the plugin's parameters")?;
    let mut new_random_param_set_events = old_random_param_set_events;
    for event in new_random_param_set_events.iter_mut() {
        match event {
            Event::ParamValue(event) => {
                event.cookie = param_infos
                    .get(&event.param_id)
                    .with_context(|| {
                        format!(
                            "Expected the plugin to have a parameter with ID {}, but the parameter is missing",
                            event.param_id,
                        )
                    })?
                    .cookie;
            }
            event => panic!("Unexpected event {event:?}"),
        }
    }

    // In the previous pass we used flush, and here we use the process funciton
    plugin.on_audio_thread(|plugin| {
        let mut buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
        let mut process = ProcessScope::new(&plugin, &mut buffers)?;

        process.audio_buffers().randomize(&mut prng);
        process.input_queue().add_events(new_random_param_set_events);
        process.run()
    })?;

    let actual_param_values: BTreeMap<clap_id, f64> = expected_param_values
        .keys()
        .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
        .collect::<Result<BTreeMap<clap_id, f64>>>()?;

    if let Some(diff) = generate_param_diff(&actual_param_values, &expected_param_values, &params)? {
        let (param_diff_file_path, mut param_diff_file) =
            PluginTestCase::StateReproducibilityFlush.temporary_file(plugin_id, PARAM_DIFF_FILE_NAME)?;

        param_diff_file.write_all(diff.as_bytes())?;

        anyhow::bail!(
            "Setting the same parameter values through 'clap_plugin_params::flush()' and through the process function \
             results in different reported values when queried through 'clap_plugin_params::get_value()'. \nDiff: \
             '{}'.",
            param_diff_file_path.display(),
        );
    }

    let actual_state = state.save()?;

    plugin.handle_callback().context("An error occured during a callback")?;

    if actual_state == expected_state {
        Ok(TestStatus::Success { details: None })
    } else {
        let (expected_state_file_path, mut expected_state_file) =
            PluginTestCase::StateReproducibilityFlush.temporary_file(plugin_id, EXPECTED_STATE_FILE_NAME)?;
        let (actual_state_file_path, mut actual_state_file) =
            PluginTestCase::StateReproducibilityFlush.temporary_file(plugin_id, ACTUAL_STATE_FILE_NAME)?;

        expected_state_file.write_all(&expected_state)?;
        actual_state_file.write_all(&actual_state)?;

        Ok(TestStatus::Warning {
            details: Some(format!(
                "Sending the same parameter values to two different instances of the plugin resulted in different \
                 state files. \nExpected: '{}'. \nActual: '{}'.",
                expected_state_file_path.display(),
                actual_state_file_path.display(),
            )),
        })
    }
}

/// The test for `PluginTestCase::StateBufferedStreams`.
pub fn test_state_buffered_streams(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;

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

        let param_infos = params
            .info()
            .context("Failure while fetching the plugin's parameters")?;
        let param_fuzzer = ParamFuzzer::new(&param_infos);
        let random_param_set_events: Vec<_> = param_fuzzer.randomize_params_at(&mut prng, 0).collect();

        plugin.on_audio_thread(|plugin| {
            let mut buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
            let mut process = ProcessScope::new(&plugin, &mut buffers)?;

            process.audio_buffers().randomize(&mut prng);
            process.input_queue().add_events(random_param_set_events);
            process.run()
        })?;

        let expected_param_values: BTreeMap<clap_id, f64> = param_infos
            .keys()
            .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
            .collect::<Result<BTreeMap<clap_id, f64>>>()?;

        // This state file is saved without buffered writes. It's expected that the plugin
        // implements this correctly, so we can check if it handles buffered streams correctly by
        // treating this as the ground truth.
        let expected_state = state.save()?;

        plugin.handle_callback().context("An error occured during a callback")?;

        (expected_state, expected_param_values)
    };

    // Now we'll recreate the plugin instance, load the state using buffered reads, check the
    // parameter values, save it again using buffered writes, and then check whether the fir.
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
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin's second instance does not implement the 'params' extension.",
                )),
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

    plugin.handle_callback().context("An error occured during a callback")?;

    // This is a buffered load that only loads 17 bytes at a time. Why 17? Because.
    const BUFFERED_LOAD_MAX_BYTES: usize = 17;
    state.load_buffered(&expected_state, BUFFERED_LOAD_MAX_BYTES)?;
    plugin.handle_callback().context("An error occured during a callback")?;

    let actual_param_values: BTreeMap<clap_id, f64> = expected_param_values
        .keys()
        .map(|param_id| params.get(*param_id).map(|value| (*param_id, value)))
        .collect::<Result<BTreeMap<clap_id, f64>>>()?;

    if let Some(diff) = generate_param_diff(&actual_param_values, &expected_param_values, &params)? {
        let (param_diff_file_path, mut param_diff_file) =
            PluginTestCase::StateBufferedStreams.temporary_file(plugin_id, PARAM_DIFF_FILE_NAME)?;

        param_diff_file.write_all(diff.as_bytes())?;

        anyhow::bail!(
            "After reloading the state by allowing the plugin to read at most {BUFFERED_LOAD_MAX_BYTES} bytes at a \
             time, the plugin's parameter values do not match the old values when queried through \
             'clap_plugin_params::get()'. \nDiff: '{}'.",
            param_diff_file_path.display()
        );
    }

    // Because we're mean, we'll use a different prime number for the saving
    const BUFFERED_SAVE_MAX_BYTES: usize = 23;
    let actual_state = state.save_buffered(BUFFERED_SAVE_MAX_BYTES)?;

    plugin.handle_callback().context("An error occured during a callback")?;

    if actual_state == expected_state {
        Ok(TestStatus::Success { details: None })
    } else {
        let (expected_state_file_path, mut expected_state_file) =
            PluginTestCase::StateBufferedStreams.temporary_file(plugin_id, EXPECTED_STATE_FILE_NAME)?;
        let (actual_state_file_path, mut actual_state_file) =
            PluginTestCase::StateBufferedStreams.temporary_file(plugin_id, ACTUAL_STATE_FILE_NAME)?;

        expected_state_file.write_all(&expected_state)?;
        actual_state_file.write_all(&actual_state)?;

        Ok(TestStatus::Warning {
            details: Some(format!(
                "Re-saving the loaded state resulted in a different state file. The original state file being \
                 compared to was written unbuffered, reloaded by allowing the plugin to read only \
                 {BUFFERED_LOAD_MAX_BYTES} bytes at a time, and then written again by allowing the plugin to write \
                 only {BUFFERED_SAVE_MAX_BYTES} bytes at a time.\n Expected: '{}'.\n Actual: '{}'.",
                expected_state_file_path.display(),
                actual_state_file_path.display(),
            )),
        })
    }
}

/// Build a string containing all different values between two sets of values.
fn generate_param_diff(
    actual: &BTreeMap<clap_id, f64>,
    expected: &BTreeMap<clap_id, f64>,
    params: &Params,
) -> Result<Option<String>> {
    let param_infos = params.info()?;

    let diff = actual
        .iter()
        .filter_map(|(&param_id, &actual_value)| {
            let expected_value = expected[&param_id];
            if param_compare_approx(actual_value, expected_value) {
                return None;
            }

            let param_name = &param_infos[&param_id].name;
            let string_actual = params
                .value_to_text(param_id, actual_value)
                .ok()
                .flatten()
                .unwrap_or("<error>".to_string());
            let string_expected = params
                .value_to_text(param_id, expected_value)
                .ok()
                .flatten()
                .unwrap_or("<error>".to_string());

            Some(format!(
                "{}, {:?}, {:?}, {:.4}, {:?}, {:.4}",
                param_id, param_name, string_actual, actual_value, string_expected, expected_value,
            ))
        })
        .collect::<Vec<String>>();

    if diff.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "param-id, param-name, actual-string, actual-value, expected-string, expected-value\n{}",
            diff.join("\n")
        )))
    }
}
