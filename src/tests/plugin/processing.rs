//! Contains most of the boilerplate around testing audio processing.

use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, ProcessScope};
use crate::tests::TestStatus;
use crate::tests::rng::{NoteGenerator, new_prng};
use anyhow::{Context, Result};
use rand::Rng;

const BUFFER_SIZE: u32 = 512;

/// The test for `PluginTestCase::ProcessAudioOutOfPlaceBasic` and `PluginTestCase::ProcessAudioInPlaceBasic`.
pub fn test_process_audio_basic(library: &PluginLibrary, plugin_id: &str, in_place: bool) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports' extension.",
                )),
            });
        }
    };

    let mut audio_buffers = if in_place {
        AudioBuffers::new_in_place_f32(&audio_ports_config, BUFFER_SIZE)
    } else {
        AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE)
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..5 {
            process.audio_buffers().randomize(&mut prng);
            process.run()?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

// The test for `PluginTestCase::ProcessAudioOutOfPlaceDouble`.
pub fn test_process_audio_double(library: &PluginLibrary, plugin_id: &str, in_place: bool) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports' extension.",
                )),
            });
        }
    };

    let note_ports_config = plugin
        .get_extension::<NotePorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'note-ports' IO configuration")?
        .unwrap_or_default();

    plugin.handle_callback().context("An error occured during a callback")?;

    let has_double_support = audio_ports_config
        .inputs
        .iter()
        .chain(audio_ports_config.outputs.iter())
        .any(|port| port.supports_double_sample_size);

    if !has_double_support {
        return Ok(TestStatus::Skipped {
            details: Some(String::from("The plugin does not support 64-bit floating point audio.")),
        });
    }

    let mut note_rng = NoteGenerator::new(&note_ports_config);
    let mut audio_buffers = if in_place {
        AudioBuffers::new_in_place_f64(&audio_ports_config, BUFFER_SIZE)
    } else {
        AudioBuffers::new_out_of_place_f64(&audio_ports_config, BUFFER_SIZE)
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..5 {
            process.audio_buffers().randomize(&mut prng);
            process
                .input_queue()
                .add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
            process.run()?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessNoteOutOfPlaceBasic` and `PluginTestCase::ProcessNoteInconsistent`. This test is very similar to
/// `ProcessAudioOutOfPlaceBasic`, but it requires the `note-ports` extension, sends notes and/or
/// MIDI to the plugin, and doesn't require the `audio-ports` extension.
pub fn test_process_note_out_of_place(
    library: &PluginLibrary,
    plugin_id: &str,
    consistent: bool,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    // You can have note/MIDI-only plugins, so not having any audio ports is perfectly fine here
    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'note-ports' extension.",
                )),
            });
        }
    };

    if note_ports_config.inputs.is_empty() {
        return Ok(TestStatus::Skipped {
            details: Some(String::from(
                "The plugin implements the 'note-ports' extension but it does not have any input note ports.",
            )),
        });
    }

    // We'll fill the input event queue with (consistent) random CLAP note and/or MIDI
    // events depending on what's supported by the plugin supports

    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
    let mut note_rng = NoteGenerator::new(&note_ports_config);
    if !consistent {
        note_rng = note_rng.with_inconsistent_events();
    }

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..5 {
            process.audio_buffers().randomize(&mut prng);
            process
                .input_queue()
                .add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
            process.run()?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingSampleRates`.
pub fn test_process_varying_sample_rates(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const SAMPLE_RATES: &[f64] = &[
        1000.0, 10000.0, 22050.0, 32000.0, 44100.0, 48000.0, 88200.0, 96000.0, 192000.0, 384000.0, 768000.0, 1234.5678,
        12345.678, 45678.901, 123456.78,
    ];

    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = plugin
        .get_extension::<AudioPorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'audio-ports' IO configuration")?
        .unwrap_or_default();

    let note_ports_config = plugin
        .get_extension::<NotePorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'note-ports' IO configuration")?
        .unwrap_or_default();

    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);

    for &sample_rate in SAMPLE_RATES {
        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut note_rng = NoteGenerator::new(&note_ports_config);
                let mut process = ProcessScope::with_sample_rate(&plugin, &mut audio_buffers, sample_rate)?;

                for _ in 0..5 {
                    process.audio_buffers().randomize(&mut prng);
                    process
                        .input_queue()
                        .add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| format!("Error while processing with {:.2}hz sample rate", sample_rate))?;
    }

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingBlockSizes`.
pub fn test_process_varying_block_sizes(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const BLOCK_SIZES: &[u32] = &[1, 8, 32, 256, 512, 1024, 2048, 4096, 8192, 32768, 1536, 10, 17, 2027];

    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = plugin
        .get_extension::<AudioPorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'audio-ports' IO configuration")?
        .unwrap_or_default();

    let note_ports_config = plugin
        .get_extension::<NotePorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'note-ports' IO configuration")?
        .unwrap_or_default();

    for &buffer_size in BLOCK_SIZES {
        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, buffer_size);
                let mut note_rng = NoteGenerator::new(&note_ports_config);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;
                let num_iters = (32768 / buffer_size).min(5);

                for _ in 0..num_iters {
                    process.audio_buffers().randomize(&mut prng);
                    process
                        .input_queue()
                        .add_events(note_rng.generate_events(&mut prng, buffer_size));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| format!("Error while processing with buffer size of {}", buffer_size))?;
    }

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessRandomBlockSizes`.
pub fn test_process_random_block_sizes(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const MAX_BUFFER_SIZE: u32 = 2048;

    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = plugin
        .get_extension::<AudioPorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'audio-ports' IO configuration")?
        .unwrap_or_default();

    let note_ports_config = plugin
        .get_extension::<NotePorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'note-ports' IO configuration")?
        .unwrap_or_default();

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, MAX_BUFFER_SIZE);
        let mut note_rng = NoteGenerator::new(&note_ports_config);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..20 {
            let buffer_size = if prng.random_bool(0.8) {
                prng.random_range(2..=MAX_BUFFER_SIZE)
            } else {
                1
            };

            process.audio_buffers().randomize(&mut prng);
            process
                .input_queue()
                .add_events(note_rng.generate_events(&mut prng, buffer_size));
            process
                .run_with_block_size(buffer_size)
                .with_context(|| format!("Error while processing with buffer size of {}", buffer_size))?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessAudioConstantMask`.
pub fn test_process_audio_constant_mask(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports' extension.",
                )),
            });
        }
    };

    if audio_ports_config.inputs.is_empty() {
        return Ok(TestStatus::Skipped {
            details: Some(String::from("The plugin does not have any audio input ports.")),
        });
    }

    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
    let mut has_received_constant_output = false;
    let mut has_received_constant_flag = false;

    let mut check_buffers = |buffers: &AudioBuffers| -> Result<()> {
        for buffer in buffers.buffers() {
            let Some(output) = buffer.port().as_output() else {
                continue;
            };

            for channel in 0..buffer.channels() {
                let is_constant = (0..buffer.len()).all(|sample| buffer.get(channel, sample) == buffer.get(channel, 0)); // TODO: relax, allow small variations?

                let marked_constant = buffers.get_output_constant_mask(output).is_channel_constant(channel);

                if marked_constant && !is_constant {
                    anyhow::bail!(
                        "The plugin has marked output port {output}, channel {channel} as constant, but it contains \
                         non-constant data."
                    );
                }

                if marked_constant {
                    has_received_constant_flag |= true;
                }

                if is_constant {
                    has_received_constant_output |= true;
                }
            }
        }

        Ok(())
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        // block 1: silent inputs, see what the plugin does
        process.run()?;
        check_buffers(process.audio_buffers())?;

        // block 2: randomize inputs, see if the plugin tracks constant channels
        process.audio_buffers().randomize(&mut prng);
        process.run()?;
        check_buffers(process.audio_buffers())?;

        // block 3-40: silent inputs again, see if the plugin updates the constant mask accordingly
        // 40 blocks to give the output tail to fully decay to silence if there is any reverb/delay
        process.audio_buffers().silence_inputs();
        for _ in 3..=40 {
            process.run()?;
            check_buffers(process.audio_buffers())?;
        }

        Ok(())
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    if !has_received_constant_flag && has_received_constant_output {
        return Ok(TestStatus::Warning {
            details: Some(String::from(
                "The plugin does not seem to set the constant mask during processing.",
            )),
        });
    }

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessResetDeterminism`.
pub fn test_process_audio_reset_determinism(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const BUFFER_SIZE: u32 = 4096;

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = plugin
        .get_extension::<AudioPorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'audio-ports' IO configuration")?
        .unwrap_or_default();

    let note_ports_config = plugin
        .get_extension::<NotePorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'note-ports' IO configuration")?
        .unwrap_or_default();

    let result = plugin.on_audio_thread(|plugin| -> Result<TestStatus> {
        let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
        let mut note_rng = NoteGenerator::new(&note_ports_config);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        // first run, "control" run
        process.audio_buffers().randomize(&mut new_prng());
        process
            .input_queue()
            .add_events(note_rng.generate_events(&mut new_prng(), BUFFER_SIZE));
        process.run()?;
        let output_control = process.audio_buffers().clone();

        // second run, deactivate and reactivate the plugin, see if the output changes
        process.restart();
        process.audio_buffers().randomize(&mut new_prng());
        process
            .input_queue()
            .add_events(note_rng.generate_events(&mut new_prng(), BUFFER_SIZE));
        process.run()?;
        let output_reactivated = process.audio_buffers().clone();

        // third run, reset the plugin, see if the output matches the control run
        process.reset();
        process.audio_buffers().randomize(&mut new_prng());
        process
            .input_queue()
            .add_events(note_rng.generate_events(&mut new_prng(), BUFFER_SIZE));
        process.run()?;
        let output_reset = process.audio_buffers().clone();

        if !output_control.is_same(&output_reactivated) {
            return Ok(TestStatus::Warning {
                details: Some(String::from(
                    "Plugin output does not seem to be deterministic after reactivation",
                )),
            });
        }

        if !output_reactivated.is_same(&output_reset) {
            anyhow::bail!("Plugin output differs after reset");
        }

        Ok(TestStatus::Success { details: None })
    })?;

    plugin.handle_callback().context("An error occured during a callback")?;

    Ok(result)
}
