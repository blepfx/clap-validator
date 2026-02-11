//! Contains most of the boilerplate around testing audio processing.

use crate::debug::{Span, record};
use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::note_ports::{NotePortConfig, NotePorts};
use crate::plugin::ext::tail::Tail;
use crate::plugin::instance::{CallbackEvent, ProcessStatus};
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, ProcessScope};
use crate::tests::TestStatus;
use crate::tests::rng::{NoteGenerator, new_prng};
use anyhow::{Context, Result};
use either::Either;
use rand::RngExt;

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
        AudioBuffers::new_in_place_f32(&audio_ports_config, BUFFER_SIZE)?
    } else {
        AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE)
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..5 {
            process.audio_buffers().fill_white_noise(&mut prng);
            process.run()?;
        }

        Ok(())
    })?;

    plugin.poll_callback(|_| Ok(()))?;

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

    plugin.poll_callback(|_| Ok(()))?;

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

    let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
    let mut audio_buffers = if in_place {
        AudioBuffers::new_in_place_f64(&audio_ports_config, BUFFER_SIZE)?
    } else {
        AudioBuffers::new_out_of_place_f64(&audio_ports_config, BUFFER_SIZE)
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..5 {
            process.audio_buffers().fill_white_noise(&mut prng);
            process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
            process.run()?;
        }

        Ok(())
    })?;

    plugin.poll_callback(|_| Ok(()))?;

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
            process.audio_buffers().fill_white_noise(&mut prng);
            process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
            process.run()?;
        }

        Ok(())
    })?;

    plugin.poll_callback(|_| Ok(()))?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingSampleRates`.
pub fn test_process_varying_sample_rates(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const SAMPLE_RATES: &[f64] = &[
        8000.0, 22050.0, 44100.0, 48000.0, 88200.0, 96000.0, 192000.0, 384000.0, 768000.0, 1234.5678, 12345.678,
        45678.901, 123456.78,
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
        let _span = Span::begin("SampleRate", record! { sample_rate: sample_rate });

        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
                let mut process = ProcessScope::with_sample_rate(&plugin, &mut audio_buffers, sample_rate)?;

                for _ in 0..5 {
                    process.audio_buffers().fill_white_noise(&mut prng);
                    process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| format!("Error while processing with {:.2}hz sample rate", sample_rate))?;
    }

    plugin.poll_callback(|_| Ok(()))?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingBlockSizes`.
pub fn test_process_varying_block_sizes(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const BLOCK_SIZES: &[u32] = &[1, 256, 1024, 4096, 16384, 1536, 10, 17, 2027];

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
        let _span = Span::begin("BlockSize", record! { buffer_size: buffer_size });

        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, buffer_size);
                let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;
                let num_iters = (16384 / buffer_size).min(5);

                for _ in 0..num_iters {
                    process.audio_buffers().fill_white_noise(&mut prng);
                    process.add_events(note_rng.generate_events(&mut prng, buffer_size));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| format!("Error while processing with buffer size of {}", buffer_size))?;
    }

    plugin.poll_callback(|_| Ok(()))?;

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
        let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        for _ in 0..20 {
            let buffer_size = if prng.random_bool(0.8) {
                prng.random_range(2..=MAX_BUFFER_SIZE)
            } else {
                1
            };

            process.audio_buffers().fill_white_noise(&mut prng);
            process.add_events(note_rng.generate_events(&mut prng, buffer_size));
            process
                .run_with_block_size(buffer_size)
                .with_context(|| format!("Error while processing with buffer size of {}", buffer_size))?;
        }

        Ok(())
    })?;

    plugin.poll_callback(|_| Ok(()))?;

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
        let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        // first run, the "control"
        let span = Span::begin("RunControl", ());
        process.audio_buffers().fill_white_noise(&mut new_prng());
        process.add_events(note_rng.generate_events(&mut new_prng(), BUFFER_SIZE));
        process.run()?;
        span.finish(());

        let output_control = process
            .audio_buffers()
            .iter()
            .filter(|x| x.port().output().is_some())
            .cloned()
            .collect::<Vec<_>>();

        process.restart();
        note_rng.reset();

        // second run, deactivate and reactivate the plugin, see if the output changes
        let span = Span::begin("RunReactivate", ());
        process.audio_buffers().fill_white_noise(&mut new_prng());
        process.add_events(note_rng.generate_events(&mut new_prng(), BUFFER_SIZE));
        process.run()?;
        span.finish(());

        let output_reactivated = process
            .audio_buffers()
            .iter()
            .filter(|x| x.port().output().is_some())
            .cloned()
            .collect::<Vec<_>>();

        process.reset();
        note_rng.reset();

        // third run, reset the plugin, see if the output matches the control run
        let span = Span::begin("RunReset", ());
        process.audio_buffers().fill_white_noise(&mut new_prng());
        process.add_events(note_rng.generate_events(&mut new_prng(), BUFFER_SIZE));
        process.run()?;
        span.finish(());

        let output_reset = process
            .audio_buffers()
            .iter()
            .filter(|x| x.port().output().is_some())
            .cloned()
            .collect::<Vec<_>>();

        if output_control
            .iter()
            .zip(output_reactivated.iter())
            .any(|(a, b)| !a.is_same(b))
        {
            return Ok(TestStatus::Warning {
                details: Some(String::from(
                    "Plugin output does not seem to be deterministic after reactivation",
                )),
            });
        }

        if output_control
            .iter()
            .zip(output_reset.iter())
            .any(|(a, b)| !a.is_same(b))
        {
            anyhow::bail!("Plugin output differs after reset");
        }

        Ok(TestStatus::Success { details: None })
    })?;

    plugin.poll_callback(|_| Ok(()))?;

    Ok(result)
}

/// The test for `PluginTestCase::ProcessSleepConstantMask`.
pub fn test_process_sleep_constant_mask(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

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
        None => NotePortConfig::default(),
    };

    let mut has_received_constant_output = false;
    let mut has_received_constant_flag = false;

    let mut check_buffers = |buffers: &AudioBuffers| -> Result<()> {
        for buffer in buffers.iter() {
            let Some(output) = buffer.port().output() else {
                continue;
            };

            for channel in 0..buffer.channels() {
                let is_constant = check_channel_quiet(buffer.channel(channel), true);
                let marked_constant = buffer.get_output_constant_mask().is_channel_constant(channel);

                if marked_constant && let Err(db) = is_constant {
                    anyhow::bail!(
                        "The plugin has marked output port {output}, channel {channel} as constant, but it contains \
                         non-constant data ({db:.2} dBFS)",
                    );
                }

                if marked_constant {
                    has_received_constant_flag |= true;
                }

                if is_constant.is_ok() {
                    has_received_constant_output |= true;
                }
            }
        }

        Ok(())
    };

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
        let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        // block 1: silent inputs, see what the plugin does
        let span = Span::begin("BlockPrerollSilent", ());
        process.run()?;
        check_buffers(process.audio_buffers()).context("Block preroll silent")?;
        span.finish(());

        // block 2: randomize inputs, see if the plugin tracks constant channels
        let span = Span::begin("BlockActiveInput", ());
        process.audio_buffers().fill_white_noise(&mut prng);
        process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
        process.run()?;
        check_buffers(process.audio_buffers()).context("Block random input")?;
        span.finish(());

        // block 3-40: silent inputs again, see if the plugin updates the constant mask accordingly
        // 40 blocks to give the output tail to fully decay to silence if there is any reverb/delay
        let span = Span::begin("BlockTailSilent", ());
        process.audio_buffers().fill_silence();
        process.add_events(note_rng.stop_all_voices(0));
        for _ in 3..=40 {
            process.run()?;
            check_buffers(process.audio_buffers())?;
        }
        span.finish(());

        Ok(())
    })?;

    plugin.poll_callback(|_| Ok(()))?;

    if !has_received_constant_flag && has_received_constant_output {
        return Ok(TestStatus::Warning {
            details: Some(String::from(
                "The plugin does not seem to set the constant mask during processing.",
            )),
        });
    }

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessSleepProcessStatus`.
pub fn test_process_sleep_process_status(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

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
        None => NotePortConfig::default(),
    };

    let mut has_ever_slept = false;

    plugin.on_audio_thread(|plugin| -> Result<()> {
        let tail = plugin.get_extension::<Tail>();

        let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, BUFFER_SIZE);
        let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-4..=64);
        let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

        let mut is_sleeping = false;
        let mut quiet_time = 0;

        for is_quiet in [true, false, true, false, true, true] {
            let _span = if is_quiet {
                Span::begin("BlockQuiet", ())
            } else {
                Span::begin("BlockActive", ())
            };

            for _ in 0..10 {
                if is_quiet {
                    process.add_events(note_rng.stop_all_voices(0));
                    process.audio_buffers().fill_silence();
                } else {
                    process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.audio_buffers().fill_white_noise(&mut prng);
                }

                plugin.poll_callback(|_, event| match event {
                    CallbackEvent::RequestProcess => {
                        is_sleeping = false;
                        Ok(())
                    }

                    _ => Ok(()),
                })?;

                let status = process.run()?;

                if is_sleeping && is_quiet {
                    for buffer in process.audio_buffers().iter() {
                        let Some(output) = buffer.port().output() else {
                            continue;
                        };

                        for channel in 0..buffer.channels() {
                            let is_constant = check_channel_quiet(buffer.channel(channel), true);
                            if let Err(db) = is_constant {
                                anyhow::bail!(
                                    "The plugin is sleeping but output port {output}, channel {channel} contains \
                                     non-constant data ({db:.2} dBFS)",
                                );
                            }
                        }
                    }
                }

                has_ever_slept |= is_sleeping;

                match status {
                    ProcessStatus::Continue => is_sleeping = false,
                    ProcessStatus::Sleep => is_sleeping = true,
                    ProcessStatus::ContinueIfNotQuiet => {
                        let is_output_quiet = process
                            .audio_buffers()
                            .iter()
                            .filter(|b| b.port().output().is_some())
                            .all(|b| b.get_output_constant_mask().are_all_channels_constant(b.channels()));

                        is_sleeping = is_output_quiet;
                    }

                    ProcessStatus::Tail => {
                        let tail = match &tail {
                            Some(tail) => tail.get(),
                            None => {
                                anyhow::bail!(
                                    "Plugin returned `CLAP_PROCESS_TAIL` process status but does not implement the \
                                     'tail' extension."
                                );
                            }
                        };

                        is_sleeping = tail < quiet_time;
                        if is_quiet {
                            quiet_time += BUFFER_SIZE;
                        } else {
                            quiet_time = 0;
                        }
                    }
                }
            }
        }

        Ok(())
    })?;

    plugin.poll_callback(|_| Ok(()))?;

    if !has_ever_slept {
        return Ok(TestStatus::Warning {
            details: Some(String::from("The plugin never went to sleep during the test.")),
        });
    }

    Ok(TestStatus::Success { details: None })
}

/// A channel is considered quiet if the signal (excluding first 32 samples) is below -80 dbfs, ignoring DC.
///
/// This function is designed to be very lenient in what it considers "quiet", to avoid false positives.
/// Returns `Ok(())` if the channel is quiet, or `Err(max_amplitude_in_db)` if not.
fn check_channel_quiet(channel: Either<&[f32], &[f64]>, ignore_dc: bool) -> Result<(), f64> {
    /// -60 dbfs
    const QUIET_THRESHOLD: f64 = 0.001;

    let (min, max) = match channel {
        Either::Right(x) => x.iter().fold((f64::MAX, f64::MIN), |(min, max), &sample| {
            (min.min(sample.abs()), max.max(sample.abs()))
        }),
        Either::Left(x) => {
            let (min, max) = x.iter().fold((f32::MAX, f32::MIN), |(min, max), &sample| {
                (min.min(sample.abs()), max.max(sample.abs()))
            });

            (min as f64, max as f64)
        }
    };

    let range = if ignore_dc { (max - min) * 0.5 } else { max.max(-min) };

    if range < QUIET_THRESHOLD {
        Ok(())
    } else {
        Err(20.0 * range.log10())
    }
}
