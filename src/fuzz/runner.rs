use crate::cli::tracing::{Span, record};
use crate::fuzz::FuzzStatus;
use crate::plugin::ext::audio_ports::AudioPorts;
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::ext::params::Params;
use crate::plugin::ext::state::State;
use crate::plugin::instance::CallbackEvent;
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, Event, InputEventQueue, OutputEventQueue, ProcessRun, ProcessScope};
use crate::tests::rng::{NoteGenerator, ParamFuzzer, TransportFuzzer};
use anyhow::Result;
use rand::rngs::Xoshiro128PlusPlus;
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Runs a single fuzzer chunk for a given plugin.
///
/// Fully deterministic w.r.t. the seed.
///
/// The fuzzer:
/// - Randomly generates some processing configurations (buffer size, sample rate, etc.)
pub fn run_fuzzer(library: &Path, plugin_id: &str, seed: u64) -> Result<FuzzStatus> {
    let _span = Span::begin(
        "Fuzzer",
        record! {
            library: library.display().to_string(),
            plugin_id: plugin_id.to_string(),
            seed: seed
        },
    );

    let mut prng = Xoshiro128PlusPlus::seed_from_u64(seed);
    let library = PluginLibrary::load(library)?;
    let plugin = library.create_plugin(plugin_id)?;

    plugin.init()?;

    let mut audio_config = plugin
        .get_extension::<AudioPorts>()
        .map(|audio_ports| audio_ports.config())
        .transpose()?
        .unwrap_or_default();

    let mut note_config = plugin
        .get_extension::<NotePorts>()
        .map(|note_ports| note_ports.config())
        .transpose()?
        .unwrap_or_default();

    let mut param_info = plugin
        .get_extension::<Params>()
        .map(|params| params.info())
        .transpose()?
        .unwrap_or_default();

    // randomize parameters and set via flush
    if !param_info.is_empty() {
        let param_fuzzer = ParamFuzzer::new(&param_info);
        let input_queue = InputEventQueue::new();
        let output_queue = OutputEventQueue::new();

        input_queue.add_events(param_fuzzer.randomize_params_at(&mut prng, 0));

        plugin
            .get_extension::<Params>()
            .ok_or(anyhow::anyhow!(
                "No 'params' extension when querying it for a second time"
            ))?
            .flush(&input_queue, &output_queue);
    }

    // we use this to check state saving/loading (in parallel)
    let last_saved_state = Arc::new(Mutex::new(None));

    for _ in 0..prng.random_range(5..10) {
        // use in-place processing if possible
        let is_in_place = prng.random_bool(0.5);
        // use 64-bit processing if possible
        let is_64bit = prng.random_bool(0.5);

        // new random sample rate (use preferined for 90% of the cases, fully random for 10% of the cases)
        let sample_rate = {
            const PRESET: &[f64] = &[22050.0, 44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0];

            if prng.random_bool(0.1) {
                prng.random_range(1000.0..1_000_000.0)
            } else {
                *PRESET.choose(&mut prng).unwrap()
            }
        };

        // same for max buffer size
        let max_buffer_size = {
            const PRESET: &[u32] = &[64, 128, 256, 512, 1024, 2048, 4096];

            if prng.random_bool(0.1) {
                prng.random_range(1..10000)
            } else {
                *PRESET.choose(&mut prng).unwrap()
            }
        };

        let min_buffer_size = if prng.random_bool(0.25) {
            max_buffer_size
        } else if prng.random_bool(0.25) {
            1
        } else {
            prng.random_range(1..max_buffer_size)
        };

        let mut blocks_to_process = prng.random_range(20000..100000u32).div_ceil(max_buffer_size);
        let mut audio_config_changed = false;
        let mut note_config_changed = false;
        let mut params_changed = false;
        let mut is_sleeping = false;

        while blocks_to_process > 0 {
            if audio_config_changed {
                audio_config_changed = false;
                audio_config = plugin
                    .get_extension::<AudioPorts>()
                    .map(|ports| ports.config())
                    .transpose()?
                    .unwrap_or_default();
            }

            if note_config_changed {
                note_config_changed = false;
                note_config = plugin
                    .get_extension::<NotePorts>()
                    .map(|ports| ports.config())
                    .transpose()?
                    .unwrap_or_default();
            }

            if params_changed {
                params_changed = false;
                param_info = plugin
                    .get_extension::<Params>()
                    .map(|params| params.info())
                    .transpose()?
                    .unwrap_or_default();
            }

            plugin.on_audio_thread(|plugin| -> Result<()> {
                let mut buffers = match (is_in_place, is_64bit) {
                    (true, true) => AudioBuffers::new_in_place_f64(&audio_config, max_buffer_size)?,
                    (true, false) => AudioBuffers::new_in_place_f32(&audio_config, max_buffer_size)?,
                    (false, true) => AudioBuffers::new_out_of_place_f64(&audio_config, max_buffer_size),
                    (false, false) => AudioBuffers::new_out_of_place_f32(&audio_config, max_buffer_size),
                };

                let mut process = ProcessScope::with_config(&plugin, &mut buffers, sample_rate, min_buffer_size)?;
                let mut transport_fuzzer = TransportFuzzer::new();
                let mut event_fuzzer = NoteGenerator::new(&note_config)
                    .with_params(&param_info)
                    .with_sample_offset_range(-10..=200);
                let param_fuzzer = ParamFuzzer::new(&param_info).with_sample_offset_range(-10..=200);

                while blocks_to_process > 0 {
                    if !process.wants_restart() && prng.random_bool(0.8) {
                        plugin.poll_callback(); // unsynchronized poll, runs parallel to the audio thread (non-blocking)
                    } else {
                        plugin.poll_callback_with(|_, event| {
                            match event {
                                CallbackEvent::RequestProcess => {
                                    is_sleeping = false;
                                }
                                CallbackEvent::AudioPortsRescanAll => {
                                    audio_config_changed = true;
                                }
                                CallbackEvent::NotePortsRescanAll => {
                                    note_config_changed = true;
                                }
                                CallbackEvent::ParamsRescanAll | CallbackEvent::ParamsRescanInfo => {
                                    params_changed = true;
                                }

                                _ => {}
                            }

                            Ok(())
                        })?;

                        if process.wants_restart() {
                            // exit the audio thread and do a full reinit before processing next blocks
                            return Ok(());
                        }
                    }

                    // pre-process setup

                    // sometimes we do a state reset
                    if prng.random_bool(0.05) {
                        process.reset();
                    }

                    // sometimes we do a full restart (deactivate + activate)
                    if prng.random_bool(0.05) {
                        process.restart();
                    }

                    // try saving the current state in parallel
                    if prng.random_bool(0.05) {
                        let last_saved_state = last_saved_state.clone();
                        let buffer_size = match prng.random_bool(0.5) {
                            true => Some(prng.random_range(1..=64)),
                            false => None,
                        };

                        plugin.send_main_thread(move |plugin| {
                            let state = match plugin.get_extension::<State>() {
                                Some(state) => state,
                                None => return Ok(()), // plugin does not support state, skip
                            };

                            let saved_state = match buffer_size {
                                Some(size) => state.save_buffered(size)?,
                                None => state.save()?,
                            };

                            *last_saved_state.lock().unwrap() = Some(saved_state);
                            Ok(())
                        });
                    }

                    // try loading the last saved state in parallel
                    if prng.random_bool(0.05) {
                        let last_saved_state = last_saved_state.clone();
                        let buffer_size = match prng.random_bool(0.5) {
                            true => Some(prng.random_range(1..=64)),
                            false => None,
                        };

                        plugin.send_main_thread(move |plugin| {
                            let state = match plugin.get_extension::<State>() {
                                Some(state) => state,
                                None => return Ok(()), // plugin does not support state, skip
                            };

                            let Some(last_saved_state) = last_saved_state.lock().unwrap().clone() else {
                                return Ok(()); // no state saved yet, skip
                            };

                            match buffer_size {
                                Some(size) => state.load_buffered(&last_saved_state, size)?,
                                None => state.load(&last_saved_state)?,
                            };

                            Ok(())
                        });
                    }

                    // random number of samples per block within the requested buffer size range
                    let num_samples = if prng.random_bool(0.5) {
                        prng.random_range(min_buffer_size..=max_buffer_size)
                    } else if prng.random_bool(0.5) {
                        max_buffer_size
                    } else {
                        min_buffer_size
                    };

                    // add random note and modulation events if we have the input ports
                    process.add_events(event_fuzzer.generate_events(&mut prng, num_samples));

                    // add random parameter change events if we have parameters
                    process.add_events(param_fuzzer.generate_events(&mut prng, num_samples));

                    // randomize transport
                    process.transport().is_freerun = prng.random_bool(0.1); // null-transport
                    transport_fuzzer.mutate(&mut prng, process.transport()); // mutate block transport

                    // sometimes add a random transport event in the middle of the block
                    if prng.random_bool(0.2) {
                        let time_offset = prng.random_range(0..num_samples);
                        let mut transport = process.transport().clone();
                        transport.advance(time_offset as _, sample_rate);
                        process.add_events([Event::Transport(transport.as_clap_transport(time_offset))]);
                    }

                    // randomize audio inputs TODO: more audio variants, not just white noise
                    process.audio_buffers().fill_white_noise(&mut prng);

                    // do the process!!
                    let status = process.run_with(ProcessRun {
                        block_size: num_samples,
                        output_ignore_denormals: false,
                        output_ignore_mask: 0,
                    })?;

                    blocks_to_process -= 1;

                    // post process validation
                }

                Ok(())
            })?;
        }
    }

    Ok(FuzzStatus::Success)
}
