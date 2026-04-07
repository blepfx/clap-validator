use crate::cli::tracing::{Span, record};
use crate::fuzz::FuzzStatus;
use crate::fuzz::rng::{AudioFuzzer, random_buffer_size_range, random_sample_rate};
use crate::plugin::ext::audio_ports::AudioPorts;
use crate::plugin::ext::audio_ports_config::AudioPortsConfig;
use crate::plugin::ext::configurable_audio_ports::ConfigurableAudioPorts;
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::ext::params::Params;
use crate::plugin::ext::state::State;
use crate::plugin::instance::{CallbackEvent, Plugin};
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, Event, InputEventQueue, OutputEventQueue, ProcessRun, ProcessScope};
use crate::tests::rng::{NoteGenerator, ParamFuzzer, TransportFuzzer, random_layout_requests};
use anyhow::Result;
use rand::rngs::Xoshiro128PlusPlus;
use rand::seq::{IndexedRandom, IteratorRandom};
use rand::{Rng, RngExt, SeedableRng};
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

    for _ in 0..5 {
        if choose_random_layout(&plugin, &mut prng)? {
            // If we successfully chose a layout, the port configuration might have changed, so we re-query it here to be sure.
            audio_config = plugin
                .get_extension::<AudioPorts>()
                .map(|ports| ports.config())
                .transpose()?
                .unwrap_or_default();
        }

        // use in-place processing if possible
        let is_in_place = prng.random_bool(0.5);
        // use 64-bit processing if possible
        let is_64bit = prng.random_bool(0.5);

        // new random sample rate (use preferined for 90% of the cases, fully random for 10% of the cases)
        let sample_rate = random_sample_rate(&mut prng);
        let (min_buffer_size, max_buffer_size) = random_buffer_size_range(&mut prng);

        // a quiet section is where we send no events and just process silent audio (used for tail and silence checks)
        let mut is_quiet = false;
        let mut blocks_to_process = prng.random_range(20000..200000u32).div_ceil(max_buffer_size);

        let _span = Span::begin(
            "FuzzerConfig",
            record! {
                sample_rate: sample_rate,
                blocks_to_process: blocks_to_process,
                min_buffer_size: min_buffer_size,
                max_buffer_size: max_buffer_size,
                is_in_place: is_in_place,
                is_64bit: is_64bit
            },
        );

        while blocks_to_process > 0 {
            let mut audio_config_changed = false;
            let mut note_config_changed = false;
            let mut params_changed = false;

            plugin.poll_callback(|event| {
                match event {
                    CallbackEvent::AudioPortsRescanAll | CallbackEvent::AudioPortsConfigRescan => {
                        audio_config_changed = true;
                    }
                    CallbackEvent::ParamsRescanAll | CallbackEvent::ParamsRescanInfo => {
                        params_changed = true;
                    }
                    CallbackEvent::NotePortsRescanAll => {
                        note_config_changed = true;
                    }

                    _ => {}
                }

                Ok(())
            })?;

            if audio_config_changed {
                audio_config = plugin
                    .get_extension::<AudioPorts>()
                    .map(|ports| ports.config())
                    .transpose()?
                    .unwrap_or_default();
            }

            if note_config_changed {
                note_config = plugin
                    .get_extension::<NotePorts>()
                    .map(|ports| ports.config())
                    .transpose()?
                    .unwrap_or_default();
            }

            if params_changed {
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
                let mut audio_fuzzer = AudioFuzzer::new();
                let mut param_fuzzer = ParamFuzzer::new(&param_info).with_sample_offset_range(-10..=200);
                let mut event_fuzzer = NoteGenerator::new(&note_config)
                    .with_params(&param_info)
                    .with_sample_offset_range(-10..=200);

                while blocks_to_process > 0 {
                    if process.wants_restart() {
                        // exit the audio thread and do a full reinit before processing next blocks
                        return Ok(());
                    }

                    // random number of samples per block within the requested buffer size range
                    let num_samples = if prng.random_bool(0.5) {
                        prng.random_range(min_buffer_size..=max_buffer_size)
                    } else if prng.random_bool(0.5) {
                        max_buffer_size
                    } else {
                        min_buffer_size
                    };

                    // toggle between quiet and non-quiet sections
                    if prng.random_bool(0.01) {
                        is_quiet = !is_quiet;
                    }

                    let _span = Span::begin("FuzzerBlock", record! { num_samples: num_samples, is_quiet: is_quiet });

                    // sometimes we do a state reset
                    if prng.random_bool(0.05) {
                        process.reset();
                    }

                    // sometimes we do a full restart (deactivate + activate)
                    if prng.random_bool(0.05) {
                        process.restart();
                    }

                    // try saving the current state in parallel
                    if prng.random_bool(0.01) {
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
                    if prng.random_bool(0.01) {
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

                    // try a random value to text to value to text roundtrip conversion
                    if prng.random_bool(0.05) {
                        // choose a random parameter and a random value and do a roundtrip conversion (value -> text -> value -> text) on the main thread _in parallel_.
                        if let Some((&id, param)) = param_info.iter().choose(&mut prng) {
                            let value = ParamFuzzer::random_value(param, &mut prng);
                            plugin.send_main_thread(move |plugin| test_value_conversion(plugin, id, value));
                        }
                    }

                    if is_quiet {
                        // if quiet, do not send any events and fill the audio inputs with silence (and set constant flags)
                        process.audio_buffers().fill_silence();
                    } else {
                        // sometimes generate events with null cookies to test plugins handling of that
                        param_fuzzer.no_cookies = prng.random_bool(0.1);

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

                        // randomize audio inputs
                        audio_fuzzer.fill(&mut prng, sample_rate, process.audio_buffers());
                    }

                    // unsynchronized poll, runs parallel to the audio thread (non-blocking)
                    if prng.random_bool(0.8) {
                        plugin.poll_callback();
                    }

                    // do the process!!
                    process.run_with(ProcessRun {
                        block_size: num_samples,
                        output_ignore_denormals: false,
                        output_ignore_mask: 0,
                    })?;

                    blocks_to_process -= 1;

                    //TODO: post process validation
                }

                Ok(())
            })?;
        }
    }

    Ok(FuzzStatus::Success)
}

fn choose_random_layout(plugin: &Plugin, rng: &mut impl Rng) -> Result<bool> {
    if rng.random_bool(0.25)
        && let Some(ext) = plugin.get_extension::<AudioPortsConfig>()
    {
        let list = ext.enumerate()?;
        if list.is_empty() {
            return Ok(false);
        }

        let config = list.choose(rng).unwrap();
        ext.select(config.id)?;
        return Ok(true);
    }

    if rng.random_bool(0.25)
        && let Some(ext) = plugin.get_extension::<ConfigurableAudioPorts>()
    {
        let config = match plugin.get_extension::<AudioPorts>() {
            Some(ports) => ports.config()?,
            None => return Ok(false),
        };

        // 100 attempts
        for _ in 0..100 {
            let layout = random_layout_requests(&config, rng);
            if ext.can_apply_configuration(&layout) {
                if ext.apply_configuration(&layout) {
                    return Ok(true);
                } else {
                    anyhow::bail!(
                        "'clap_plugin_configurable_audio_ports::apply_configuration' returned false but \
                         'can_apply_configuration' returned true."
                    )
                }
            }
        }
    }

    Ok(false)
}

fn test_value_conversion(plugin: &Plugin, param_id: u32, value: f64) -> Result<()> {
    let params = match plugin.get_extension::<Params>() {
        Some(params) => params,
        None => anyhow::bail!("Plugin does not support 'Params' extension"),
    };

    let text_first = match params.value_to_text(param_id, value)? {
        Some(text) => text,
        None => return Ok(()), // this parameter does not support v2t, skip
    };

    let value_second = match params.text_to_value(param_id, &text_first)? {
        Some(value) => value,
        None => {
            log::warn!(
                "Text conversion error for parameter {}: {} -> '{}' -> ?",
                param_id,
                value,
                text_first
            );

            return Ok(());
        }
    };

    let text_second = match params.value_to_text(param_id, value_second)? {
        Some(text) => text,
        None => {
            log::warn!(
                "Text conversion error for parameter {}: {} -> '{}' -> {} -> ?",
                param_id,
                value,
                text_first,
                value_second
            );

            return Ok(());
        }
    };

    if text_first != text_second {
        log::warn!(
            "Text conversion error for parameter {}: {} -> {:?} -> {} -> {:?}",
            param_id,
            value,
            text_first,
            value_second,
            text_second
        );
    }

    Ok(())
}
