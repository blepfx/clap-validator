//! Contains most of the boilerplate around testing audio processing.

use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::ext::Extension;
use crate::plugin::host::Host;
use crate::plugin::instance::audio_thread::PluginAudioThread;
use crate::plugin::instance::process::{AudioBuffers, ProcessConfig, ProcessData};
use crate::plugin::instance::Plugin;
use crate::plugin::library::PluginLibrary;
use crate::tests::rng::{new_prng, NoteGenerator};
use crate::tests::TestStatus;
use anyhow::{Context, Result};
use rand::Rng;
use std::sync::atomic::Ordering;

/// A helper to handle the boilerplate that comes with testing a plugin's audio processing behavior.
/// Run the standard audio processing test for a still **deactivated** plugin. This calls the
/// process function `num_iters` times, and checks the output for consistency each time.
///
/// The `Preprocess` closure is called before each processing cycle to allow the process data to be
/// modified for the next process cycle.
///
/// Main-thread callbacks that were made to the plugin while the audio thread was active are
/// handled implicitly.
pub struct ProcessingTest<'a> {
    plugin: &'a Plugin<'a>,
    buffers: &'a mut AudioBuffers,
    config: ProcessConfig,
}

impl<'a> ProcessingTest<'a> {
    pub fn new(plugin: &'a Plugin<'a>, buffers: &'a mut AudioBuffers) -> Self {
        Self {
            plugin,
            buffers,
            config: ProcessConfig::default(),
        }
    }

    pub fn with_sample_rate(self, sample_rate: f64) -> Self {
        Self {
            config: ProcessConfig {
                sample_rate,
                ..self.config
            },
            ..self
        }
    }

    pub fn run<Callback>(self, mut callback: Callback) -> Result<()>
    where
        Callback: FnMut(&PluginAudioThread, &mut ProcessData) -> Result<bool> + Send,
    {
        // Handle callbacks the plugin may have made during init or these queries.
        self.plugin.host().handle_callbacks_once();

        self.plugin
            .state
            .requested_restart
            .store(false, Ordering::SeqCst);

        let buffer_size = self.buffers.len();
        let mut process_data = ProcessData::new(self.buffers, self.config);
        let mut running = true;
        while running {
            self.plugin
                .activate(self.config.sample_rate, 1, buffer_size)?;

            self.plugin.on_audio_thread(|plugin| -> Result<()> {
                plugin.start_processing()?;

                // This test can be repeated a couple of times
                // NOTE: We intentionally do not disable denormals here
                'processing: while running {
                    running &= callback(&plugin, &mut process_data)?;
                    process_data.advance_next(process_data.block_size);

                    // Restart processing as necessary
                    if plugin
                        .state()
                        .requested_restart
                        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        log::trace!(
                            "Restarting the plugin during processing cycle after a call to \
                             'clap_host::request_restart()'",
                        );
                        break 'processing;
                    }
                }

                plugin.stop_processing();

                Ok(())
            })?;

            self.plugin.deactivate();
        }

        // Handle callbacks the plugin may have made during deactivate
        self.plugin.host().handle_callbacks_once();

        Ok(())
    }

    /// Run the standard audio processing test for a still **deactivated** plugin. This calls the
    /// process function `num_iters` times, and checks the output for consistency each time.
    ///
    /// The `Preprocess` closure is called before each processing cycle to allow the process data to be
    /// modified for the next process cycle.
    ///
    /// Main-thread callbacks that were made to the plugin while the audio thread was active are
    /// handled implicitly.
    pub fn run_simple<Callback>(self, num_iters: usize, mut preprocess: Callback) -> Result<()>
    where
        Callback: FnMut(&mut ProcessData) -> Result<()> + Send,
    {
        let mut original_buffers = self.buffers.clone();
        let mut curr_iter = 0;

        self.run(|plugin, process| {
            curr_iter += 1;

            preprocess(process).with_context(|| {
                format!(
                    "Failed to preprocess cycle {} out of {}",
                    curr_iter, num_iters
                )
            })?;

            original_buffers.clone_from(&process.buffers);

            plugin.process(process).with_context(|| {
                format!("Failed to process cycle {} out of {}", curr_iter, num_iters)
            })?;

            check_process_call_consistency(process, &original_buffers, true).with_context(
                || {
                    format!(
                        "Failed to validate cycle {} out of {}",
                        curr_iter, num_iters
                    )
                },
            )?;

            Ok(curr_iter < num_iters)
        })
    }

    /// Run the standard audio processing test for a still **deactivated** plugin. This is identical
    /// to the [`run()`][Self::run()] function, except that it does exactly one processing cycle and
    /// thus non-copy values can be moved into the closure.
    ///
    /// Main-thread callbacks that were made to the plugin while the audio thread was active are
    /// handled implicitly.
    pub fn run_once<Preprocess>(self, preprocess: Preprocess) -> Result<()>
    where
        Preprocess: FnOnce(&mut ProcessData) -> Result<()> + Send,
    {
        let mut preprocess = Some(preprocess);
        self.run_simple(1, |data| match preprocess.take() {
            Some(preprocess) => preprocess(data),
            None => Ok(()),
        })
    }
}

/// The test for `PluginTestCase::ProcessAudioOutOfPlaceBasic`.
pub fn test_process_audio_out_of_place_basic(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(format!(
                    "The plugin does not implement the '{}' extension.",
                    AudioPorts::EXTENSION_ID.to_str().unwrap(),
                )),
            });
        }
    };

    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, 512);
    ProcessingTest::new(&plugin, &mut audio_buffers).run_simple(5, |process_data| {
        process_data.buffers.randomize(&mut prng);
        Ok(())
    })?;

    // The `Host` contains built-in thread safety checks
    host.callback_error_check()
        .context("An error occured during a host callback")?;
    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessAudioInPlaceBasic`.
pub fn test_process_audio_in_place_basic(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(format!(
                    "The plugin does not implement the '{}' extension.",
                    AudioPorts::EXTENSION_ID.to_str().unwrap(),
                )),
            });
        }
    };

    if audio_ports_config
        .inputs
        .iter()
        .all(|x| x.in_place_pair_idx.is_none())
    {
        return Ok(TestStatus::Skipped {
            details: Some(format!(
                "The plugin does not have any in-place audio port pairs.",
            )),
        });
    }

    let mut audio_buffers = AudioBuffers::new_in_place_f32(&audio_ports_config, 512);
    ProcessingTest::new(&plugin, &mut audio_buffers).run_simple(5, |process_data| {
        process_data.buffers.randomize(&mut prng);
        Ok(())
    })?;

    // The `Host` contains built-in thread safety checks
    host.callback_error_check()
        .context("An error occured during a host callback")?;
    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessNoteOutOfPlaceBasic`. This test is very similar to
/// `ProcessAudioOutOfPlaceBasic`, but it requires the `note-ports` extension, sends notes and/or
/// MIDI to the plugin, and doesn't require the `audio-ports` extension.
pub fn test_process_note_out_of_place_basic(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
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
                details: Some(format!(
                    "The plugin does not implement the '{}' extension.",
                    NotePorts::EXTENSION_ID.to_str().unwrap(),
                )),
            });
        }
    };
    if note_ports_config.inputs.is_empty() {
        return Ok(TestStatus::Skipped {
            details: Some(format!(
                "The plugin implements the '{}' extension but it does not have any input note \
                 ports.",
                NotePorts::EXTENSION_ID.to_str().unwrap()
            )),
        });
    }

    // We'll fill the input event queue with (consistent) random CLAP note and/or MIDI
    // events depending on what's supported by the plugin supports
    let mut note_event_rng = NoteGenerator::new(note_ports_config);
    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, 512);
    ProcessingTest::new(&plugin, &mut audio_buffers).run_simple(5, |process_data| {
        process_data.buffers.randomize(&mut prng);
        note_event_rng.fill_event_queue(
            &mut prng,
            &process_data.input_events,
            process_data.block_size,
        )?;
        Ok(())
    })?;

    host.callback_error_check()
        .context("An error occured during a host callback")?;
    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessNoteInconsistent`. This is the same test as
/// `ProcessAudioOutOfPlaceBasic`, but without requiring matched note on/off pairs and similar
/// invariants
pub fn test_process_note_inconsistent(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };
    let note_port_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(format!(
                    "The plugin does not implement the '{}' extension.",
                    NotePorts::EXTENSION_ID.to_str().unwrap(),
                )),
            });
        }
    };
    if note_port_config.inputs.is_empty() {
        return Ok(TestStatus::Skipped {
            details: Some(format!(
                "The plugin implements the '{}' extension but it does not have any input note \
                 ports.",
                NotePorts::EXTENSION_ID.to_str().unwrap()
            )),
        });
    }
    host.handle_callbacks_once();

    // This RNG (Random Note Generator) allows generates mismatching events
    let mut note_event_rng = NoteGenerator::new(note_port_config).with_inconsistent_events();
    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, 512);

    // TODO: Use in-place processing for this test
    ProcessingTest::new(&plugin, &mut audio_buffers).run_simple(5, |process_data| {
        process_data.buffers.randomize(&mut prng);
        note_event_rng.fill_event_queue(
            &mut prng,
            &process_data.input_events,
            process_data.block_size,
        )?;
        Ok(())
    })?;

    host.callback_error_check()
        .context("An error occured during a host callback")?;
    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingSampleRates`.
pub fn test_process_varying_sample_rates(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    const SAMPLE_RATES: &[f64] = &[
        1000.0, 10000.0, 22050.0, 32000.0, 44100.0, 48000.0, 88200.0, 96000.0, 192000.0, 384000.0,
        768000.0, 1234.5678, 12345.678, 45678.901, 123456.78,
    ];

    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => Some(
            note_ports
                .config()
                .context("Error while querying 'note-ports' IO configuration")?,
        ),
        None => None,
    };

    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, 512);
    for &sample_rate in SAMPLE_RATES {
        let mut note_event_rng = note_ports_config.clone().map(NoteGenerator::new);

        ProcessingTest::new(&plugin, &mut audio_buffers)
            .with_sample_rate(sample_rate)
            .run_simple(5, |process_data| {
                process_data.buffers.randomize(&mut prng);

                if let Some(note_event_rng) = note_event_rng.as_mut() {
                    note_event_rng.fill_event_queue(
                        &mut prng,
                        &process_data.input_events,
                        process_data.block_size,
                    )?;
                }

                Ok(())
            })
            .context(format!(
                "Error while processing with {:.2}hz sample rate",
                sample_rate
            ))?;

        host.callback_error_check()
            .context("An error occured during a host callback")?;
    }

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingBlockSizes`.
pub fn test_process_varying_block_sizes(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    const BLOCK_SIZES: &[u32] = &[
        1, 8, 32, 256, 512, 1024, 2048, 4096, 8192, 32768, 1536, 10, 17, 2027,
    ];

    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => Some(
            note_ports
                .config()
                .context("Error while querying 'note-ports' IO configuration")?,
        ),
        None => None,
    };

    for &buffer_size in BLOCK_SIZES {
        let mut note_event_rng = note_ports_config.clone().map(NoteGenerator::new);
        let mut audio_buffers =
            AudioBuffers::new_out_of_place_f32(&audio_ports_config, buffer_size as usize);
        let num_iters = (32768 / buffer_size).min(5);

        ProcessingTest::new(&plugin, &mut audio_buffers)
            .run_simple(num_iters as usize, |process_data| {
                process_data.buffers.randomize(&mut prng);

                if let Some(note_event_rng) = note_event_rng.as_mut() {
                    note_event_rng.fill_event_queue(
                        &mut prng,
                        &process_data.input_events,
                        process_data.block_size,
                    )?;
                }

                Ok(())
            })
            .context(format!(
                "Error while processing with buffer size of {}",
                buffer_size
            ))?;

        host.callback_error_check()
            .context("An error occured during a host callback")?;
    }

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessRandomBlockSizes`.
pub fn test_process_random_block_sizes(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    const MAX_BUFFER_SIZE: u32 = 2048;

    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => AudioPortConfig::default(),
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => Some(
            note_ports
                .config()
                .context("Error while querying 'note-ports' IO configuration")?,
        ),
        None => None,
    };

    let mut note_event_rng = note_ports_config.map(NoteGenerator::new);
    let mut audio_buffers =
        AudioBuffers::new_out_of_place_f32(&audio_ports_config, MAX_BUFFER_SIZE as usize);

    ProcessingTest::new(&plugin, &mut audio_buffers).run_simple(20, |process_data| {
        process_data.block_size = if prng.gen_bool(0.8) {
            prng.gen_range(2..=MAX_BUFFER_SIZE)
        } else {
            1
        };

        process_data.buffers.randomize(&mut prng);

        if let Some(note_event_rng) = note_event_rng.as_mut() {
            note_event_rng.fill_event_queue(
                &mut prng,
                &process_data.input_events,
                process_data.block_size,
            )?;
        }

        Ok(())
    })?;

    host.callback_error_check()
        .context("An error occured during a host callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::ProcessVaryingBlockSizes`.
pub fn test_process_audio_constant_mask(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    let mut prng = new_prng();

    let host = Host::new();
    let plugin = library
        .create_plugin(plugin_id, host.clone())
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports_config = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(format!(
                    "The plugin does not implement the '{}' extension.",
                    AudioPorts::EXTENSION_ID.to_str().unwrap(),
                )),
            });
        }
    };

    let mut audio_buffers = AudioBuffers::new_out_of_place_f32(&audio_ports_config, 512);
    let mut original_buffers = audio_buffers.clone();
    let mut curr_iter = 0;

    let mut has_received_constant_output = false;
    let mut has_received_constant_flag = false;

    ProcessingTest::new(&plugin, &mut audio_buffers).run(|plugin, process| {
        process.buffers.randomize(&mut prng);

        if curr_iter != 1 {
            process.buffers.silence_all_inputs();
        }

        original_buffers.clone_from(&process.buffers);
        curr_iter += 1;

        plugin
            .process(process)
            .with_context(|| format!("Failed to process cycle {} out of 20", curr_iter))?;

        check_process_call_consistency(process, &original_buffers, true)
            .with_context(|| format!("Failed to validate cycle {} out of 20", curr_iter))?;

        for buffer in process.buffers.buffers() {
            let Some(output) = buffer.output() else {
                continue;
            };

            for channel in 0..buffer.channels() {
                let is_constant = (0..buffer.len())
                    .all(|sample| buffer.get(channel, sample) == buffer.get(channel, 0));

                let marked_constant = process.buffers.output_constant_mask(output)
                    & (1u64.unbounded_shl(channel as u32))
                    != 0;

                if marked_constant && !is_constant {
                    anyhow::bail!(
                        "Failed to validate cycle {curr_iter} out of 20: The plugin has marked \
                         output port {output}, channel {channel} as constant, but it contains \
                         non-constant data."
                    );
                }

                has_received_constant_flag |= marked_constant;
                has_received_constant_output |= is_constant;
            }
        }

        Ok(curr_iter < 20)
    })?;

    host.callback_error_check()
        .context("An error occured during a host callback")?;

    if !has_received_constant_flag && has_received_constant_output {
        return Ok(TestStatus::Warning {
            details: Some(format!(
                "The plugin does not seem to set the constant mask during processing.",
            )),
        });
    }

    Ok(TestStatus::Success { details: None })
}

/// The process for consistency. This verifies that the output buffer has been written to, doesn't contain any NaN,
/// infinite, or denormal values, that the input buffers have not been modified by the plugin, and
/// that the output event queue is monotonically ordered.
fn check_process_call_consistency(
    process_data: &ProcessData,
    original_buffers: &AudioBuffers,
    check_denormals: bool,
) -> Result<()> {
    let block_size = process_data.block_size as usize;

    for (buffer, before) in process_data
        .buffers
        .buffers()
        .iter()
        .zip(original_buffers.buffers())
    {
        // Input-only buffers must not be overwritten during out of place processing
        if let (Some(index), None) = (buffer.input(), buffer.output()) {
            if !buffer.is_same(before) {
                anyhow::bail!(
                    "The plugin has overwritten an input buffer (index {index}) during \
                     out-of-place processing."
                );
            }
        }

        // Output-only buffers must not be left "untouched" during out of place processing
        if let (Some(index), None) = (buffer.output(), buffer.input()) {
            let is_all_nans = (0..buffer.channels()).all(|channel| {
                (0..block_size).all(|sample| {
                    buffer
                        .get(channel, sample)
                        .either(|x| x.is_nan(), |x| x.is_nan())
                })
            });

            if is_all_nans && buffer.is_same(before) {
                anyhow::bail!(
                    "The plugin has left an output buffer (index {index}) untouched during \
                     out-of-place processing."
                );
            }
        }

        // Output buffers must not contain any non-finite or denormal values
        if let Some(port_idx) = buffer.output() {
            let maybe_non_finite = (0..buffer.channels())
                .flat_map(|channel| (0..block_size).map(move |sample| (channel, sample)))
                .find_map(|(channel, sample)| {
                    let x = buffer.get(channel, sample);
                    if x.either(|x| !x.is_finite(), |x| !x.is_finite()) {
                        Some((x, channel, sample))
                    } else {
                        None
                    }
                });

            if let Some((sample, channel_idx, sample_idx)) = maybe_non_finite {
                anyhow::bail!(
                    "The sample written to output port {port_idx}, channel {channel_idx}, and \
                     sample index {sample_idx} is {sample}."
                );
            }

            if check_denormals {
                let maybe_denormal = (0..buffer.channels())
                    .flat_map(|channel| (0..block_size).map(move |sample| (channel, sample)))
                    .find_map(|(channel, sample)| {
                        let x = buffer.get(channel, sample);
                        if x.either(|x| x.is_subnormal(), |x| x.is_subnormal()) {
                            Some((x, channel, sample))
                        } else {
                            None
                        }
                    });

                if let Some((sample, channel_idx, sample_idx)) = maybe_denormal {
                    anyhow::bail!(
                        "The sample written to output port {port_idx}, channel {channel_idx}, and \
                         sample index {sample_idx} is subnormal ({sample})."
                    );
                }
            }
        }
    }

    // If the plugin output any events, then they should be in a monotonically increasing order
    let mut last_event_time = 0;
    for event in process_data.output_events.events.lock().iter() {
        let event_time = event.header().time;
        if event_time < last_event_time {
            anyhow::bail!(
                "The plugin output an event for sample {event_time} after it had previously \
                 output an event for sample {last_event_time}."
            )
        }

        if event_time >= block_size as u32 {
            anyhow::bail!(
                "The plugin output an event for sample {event_time} but the audio buffer only \
                 contains {block_size} samples."
            )
        }

        last_event_time = event_time;
    }

    Ok(())
}
