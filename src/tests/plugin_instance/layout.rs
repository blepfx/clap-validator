use crate::cli::tracing::{Span, from_fn, record};
use crate::plugin::ext::audio_ports::AudioPorts;
use crate::plugin::ext::audio_ports_activation::AudioPortsActivation;
use crate::plugin::ext::audio_ports_config::{AudioPortsConfig, AudioPortsConfigInfo};
use crate::plugin::ext::configurable_audio_ports::{AudioPortsRequest, AudioPortsRequestInfo, ConfigurableAudioPorts};
use crate::plugin::ext::note_ports::{NotePortConfig, NotePorts};
use crate::plugin::instance::{CallbackEvent, HostCapabilities};
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, ProcessRun, ProcessScope};
use crate::tests::TestStatus;
use crate::tests::rng::{NoteGenerator, new_prng, random_layout_requests};
use anyhow::{Context, Result};
use clap_sys::ext::ambisonic::CLAP_PORT_AMBISONIC;
use clap_sys::ext::surround::CLAP_PORT_SURROUND;
use rand::RngExt;

const BUFFER_SIZE: u32 = 512;

/// The test for `PluginTestCase::LayoutAudioPortsConfig`.
pub fn test_layout_audio_ports_config(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    let mut prng = new_prng();

    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports' extension.",
                )),
            });
        }
    };

    let audio_ports_config_info = plugin.get_extension::<AudioPortsConfigInfo>();
    let audio_ports_config = match plugin.get_extension::<AudioPortsConfig>() {
        Some(audio_ports_config) => audio_ports_config,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports-config' extension.",
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

    for config_audio_ports_config in audio_ports_config
        .enumerate()
        .context("Could not enumerate audio port configurations")?
    {
        let _span = Span::begin(
            "Config",
            record! { id: config_audio_ports_config.id, name: &config_audio_ports_config.name },
        );

        audio_ports_config
            .select(config_audio_ports_config.id)
            .with_context(|| {
                format!(
                    "Could not select audio port configuration '{}' ({})",
                    config_audio_ports_config.name, config_audio_ports_config.id,
                )
            })?;

        plugin.poll_callback(|_| Ok(()))?;

        let config_audio_ports = audio_ports.config().with_context(|| {
            format!(
                "Error while querying 'audio-ports' IO configuration with layout '{}' ({})",
                config_audio_ports_config.name, config_audio_ports_config.id,
            )
        })?;

        // Check that the audio-ports-config info matches the actual audio-ports config
        {
            let main_input_channels = config_audio_ports
                .inputs
                .first()
                .filter(|x| x.is_main)
                .map(|x| x.channel_count);

            let main_output_channels = config_audio_ports
                .outputs
                .first()
                .filter(|x| x.is_main)
                .map(|x| x.channel_count);

            anyhow::ensure!(
                config_audio_ports.inputs.len() as u32 == config_audio_ports_config.input_port_count,
                "The number of input audio ports for configuration '{}' ({}) does not match the number reported by \
                 'audio-ports' ({})",
                config_audio_ports_config.name,
                config_audio_ports_config.input_port_count,
                config_audio_ports.inputs.len() as u32,
            );

            anyhow::ensure!(
                config_audio_ports.outputs.len() as u32 == config_audio_ports_config.output_port_count,
                "The number of output audio ports for configuration '{}' ({}) does not match the number reported by \
                 'audio-ports' ({})",
                config_audio_ports_config.name,
                config_audio_ports_config.output_port_count,
                config_audio_ports.outputs.len() as u32,
            );

            match (main_input_channels, config_audio_ports_config.main_input_channel_count) {
                (None, None) => {}
                (Some(a), Some(b)) => anyhow::ensure!(
                    a == b,
                    "The number of channels in the main input port for the '{}' configuration info ({}) does not \
                     match the number reported by 'audio-ports' ({})",
                    config_audio_ports_config.name,
                    b,
                    a,
                ),
                (None, Some(_)) => {
                    anyhow::bail!(
                        "The configuration '{}' reports that a main input port exists, but 'audio-ports' does not.",
                        config_audio_ports_config.name,
                    )
                }
                (Some(_), None) => anyhow::bail!(
                    "The configuration '{}' reports that main input port does not exist, but according to \
                     'audio-ports' it does.",
                    config_audio_ports_config.name,
                ),
            }

            match (
                main_output_channels,
                config_audio_ports_config.main_output_channel_count,
            ) {
                (None, None) => {}
                (Some(a), Some(b)) => anyhow::ensure!(
                    a == b,
                    "The number of channels in the main output port for the '{}' configuration info ({}) does not \
                     match the number reported by 'audio-ports' ({})",
                    config_audio_ports_config.name,
                    b,
                    a,
                ),
                (None, Some(_)) => {
                    anyhow::bail!(
                        "The configuration '{}' reports that a main output port exists, but 'audio-ports' does not.",
                        config_audio_ports_config.name,
                    )
                }
                (Some(_), None) => anyhow::bail!(
                    "The configuration '{}' reports that main output port does not exist, but according to \
                     'audio-ports' it does.",
                    config_audio_ports_config.name,
                ),
            }
        }

        // Check that the audio-ports-config-info matches the current config
        if let Some(audio_ports_config_info) = &audio_ports_config_info {
            anyhow::ensure!(
                audio_ports_config_info.current() == config_audio_ports_config.id,
                "The current configuration ID reported by 'audio-ports-config-info' ({}) does not match the last \
                 selected configuration ID ({})",
                audio_ports_config_info.current(),
                config_audio_ports_config.id,
            );

            for index in 0..config_audio_ports_config.input_port_count {
                let extra_info = audio_ports_config_info
                    .get(config_audio_ports_config.id, true, index)
                    .with_context(|| {
                        format!(
                            "Could not get info for input port {} of configuration '{}' ({}) from \
                             'audio-ports-config-info'",
                            index, config_audio_ports_config.name, config_audio_ports_config.id,
                        )
                    })?;

                anyhow::ensure!(
                    extra_info == config_audio_ports.inputs[index as usize],
                    "Mismatch between info queried via 'audio-ports-config-info' and 'audio-ports' for input port {} \
                     of configuration '{}' ({})",
                    index,
                    config_audio_ports_config.name,
                    config_audio_ports_config.id,
                )
            }

            for index in 0..config_audio_ports_config.output_port_count {
                let extra_info = audio_ports_config_info
                    .get(config_audio_ports_config.id, false, index)
                    .with_context(|| {
                        format!(
                            "Could not get info for output port {} of configuration '{}' ({}) from \
                             'audio-ports-config-info'",
                            index, config_audio_ports_config.name, config_audio_ports_config.id,
                        )
                    })?;

                anyhow::ensure!(
                    extra_info == config_audio_ports.outputs[index as usize],
                    "Mismatch between info queried via 'audio-ports-config-info' and 'audio-ports' for output port {} \
                     of configuration '{}' ({})",
                    index,
                    config_audio_ports_config.name,
                    config_audio_ports_config.id,
                )
            }
        }

        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut audio_buffers = AudioBuffers::new_in_place_f32(&config_audio_ports, BUFFER_SIZE)?;
                let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-1..=128);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

                for _ in 0..5 {
                    plugin.poll_callback();
                    process.audio_buffers().fill_white_noise(&mut prng);
                    process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| {
                format!(
                    "Error while processing audio with IO configuration '{}' ({})",
                    config_audio_ports_config.name, config_audio_ports_config.id,
                )
            })?;
    }

    plugin.poll_callback(|_| Ok(()))?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::LayoutConfigurableAudioPorts`.
pub fn test_layout_configurable_audio_ports(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    const MAX_TOTAL_CHECKS: u32 = 200;
    const MAX_PASSED_CHECKS: u32 = 50;

    let mut prng = new_prng();
    let plugin = library
        .create_plugin(plugin_id)
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports' extension.",
                )),
            });
        }
    };

    let configurable_audio_ports = match plugin.get_extension::<ConfigurableAudioPorts>() {
        Some(extension) => extension,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'configurable-audio-ports' extension.",
                )),
            });
        }
    };

    let ambisonic = plugin.get_extension::<crate::plugin::ext::ambisonic::Ambisonic>();
    let surround = plugin.get_extension::<crate::plugin::ext::surround::Surround>();

    let note_ports_config = plugin
        .get_extension::<NotePorts>()
        .map(|x| x.config())
        .transpose()
        .context("Error while querying 'note-ports' IO configuration")?
        .unwrap_or_default();

    let config_audio_ports = audio_ports
        .config()
        .context("Error while querying 'audio-ports' IO configuration")?;

    let mut checks_total = 0;
    let mut checks_passed = 0;

    while checks_total < MAX_TOTAL_CHECKS && checks_passed < MAX_PASSED_CHECKS {
        let requests = random_layout_requests(&config_audio_ports, &mut prng);

        let _span = Span::begin(
            "Config",
            from_fn(|record| {
                for (i, request) in requests.iter().enumerate() {
                    record.record(&format!("requests.{}", i), *request);
                }
            }),
        );

        let can_apply = configurable_audio_ports.can_apply_configuration(&requests);
        let has_applied = configurable_audio_ports.apply_configuration(&requests);

        if can_apply != has_applied {
            anyhow::bail!(
                "The plugin returned conflicting results from 'can_apply_configuration' ({}) and \
                 'apply_configuration' ({}) for the following layout: \n{}",
                can_apply,
                has_applied,
                print_layout(&requests)
            );
        }

        if has_applied {
            checks_total += 1;
            checks_passed += 1;
        } else {
            checks_total += 1;
            continue;
        }

        let config_audio_ports = audio_ports.config().with_context(|| {
            format!(
                "Error while querying 'audio-ports' IO configuration after applying the following layout: \n{}",
                print_layout(&requests)
            )
        })?;

        for request in &requests {
            let port = match request.is_input {
                true => config_audio_ports.inputs.get(request.port_index as usize),
                false => config_audio_ports.outputs.get(request.port_index as usize),
            };

            let port = match port {
                Some(port) => port,
                None => continue, // we assume that the plugin being overly defensive and accepts configurations with out-of-range port indices, but then ignores the invalid requests instead of rejecting the whole configuration
            };

            if port.channel_count != request.request_info.channel_count() {
                anyhow::bail!(
                    "Wrong number of channels set for {} port (index {}) in response to the layout request: \n{}\n \
                     Expected: {}, got: {}",
                    if request.is_input { "input" } else { "output" },
                    request.port_index,
                    print_layout(&requests),
                    request.request_info.channel_count(),
                    port.channel_count,
                );
            }

            match request.request_info {
                AudioPortsRequestInfo::Ambisonic { config, .. } => {
                    if port.port_type.as_deref() == Some(CLAP_PORT_AMBISONIC) {
                        let result = ambisonic
                            .as_ref()
                            .expect("already checked")
                            .get_config(request.is_input, request.port_index);

                        if result
                            .is_none_or(|x| x.normalization != config.normalization && x.ordering != config.ordering)
                        {
                            anyhow::bail!(
                                "Wrong ambisonic config set for {} port (index {}) in response to the layout request: \
                                 \n{}",
                                if request.is_input { "input" } else { "output" },
                                request.port_index,
                                print_layout(&requests),
                            );
                        }
                    }
                }

                AudioPortsRequestInfo::Surround { channel_map } => {
                    if port.port_type.as_deref() == Some(CLAP_PORT_SURROUND) {
                        let result_map = surround.as_ref().expect("already checked").get_channel_map(
                            request.is_input,
                            request.port_index,
                            channel_map.len() as u32,
                        );

                        if channel_map != result_map {
                            anyhow::bail!(
                                "Wrong surround map set for {} port (index {}) in response to the layout request: \n{}",
                                if request.is_input { "input" } else { "output" },
                                request.port_index,
                                print_layout(&requests),
                            );
                        }
                    }
                }

                _ => {}
            }
        }

        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut audio_buffers = AudioBuffers::new_in_place_f32(&config_audio_ports, BUFFER_SIZE)?;
                let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-1..=128);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

                for _ in 0..5 {
                    plugin.poll_callback();
                    process.audio_buffers().fill_white_noise(&mut prng);
                    process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| {
                format!(
                    "Error while processing audio with the following configuration: \n{}",
                    print_layout(&requests)
                )
            })?;
    }

    plugin.poll_callback(|_| Ok(()))?;

    if checks_passed == 0 {
        return Ok(TestStatus::Warning {
            details: Some(format!(
                "Tried {} random audio port layouts, but none were accepted.",
                checks_total
            )),
        });
    }

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::LayoutAudioPortsActivation`.
pub fn test_layout_audio_ports_activation(library: &PluginLibrary, plugin_id: &str) -> Result<TestStatus> {
    #[derive(Debug, Clone, Copy)]
    struct PortMask(u64);

    impl PortMask {
        fn enabled(port_count: usize) -> Self {
            Self((1u64.unbounded_shl(port_count as u32)).wrapping_sub(1))
        }

        fn random(rng: &mut impl RngExt, port_count: usize) -> Self {
            Self(rng.next_u64() & (1u64.unbounded_shl(port_count as u32).wrapping_sub(1)))
        }

        fn is_active(&self, port_index: usize) -> bool {
            (self.0 & 1u64.unbounded_shl(port_index as u32)) != 0
        }

        fn with_active(&self, port_index: usize, active: bool) -> Self {
            if active {
                Self(self.0 | 1u64.unbounded_shl(port_index as u32))
            } else {
                Self(self.0 & !1u64.unbounded_shl(port_index as u32))
            }
        }
    }

    let mut prng = new_prng();

    let plugin = library
        .create_plugin_with(
            plugin_id,
            HostCapabilities {
                can_rescan_audio_ports: true,
                ..Default::default()
            },
        )
        .context("Could not create the plugin instance")?;
    plugin.init().context("Error during initialization")?;

    let audio_ports = match plugin.get_extension::<AudioPorts>() {
        Some(audio_ports) => audio_ports,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports' extension.",
                )),
            });
        }
    };

    let audio_ports_activation = match plugin.get_extension::<AudioPortsActivation>() {
        Some(extension) => extension,
        None => {
            return Ok(TestStatus::Skipped {
                details: Some(String::from(
                    "The plugin does not implement the 'audio-ports-activation' extension.",
                )),
            });
        }
    };

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => NotePortConfig::default(),
    };

    let mut audio_ports_config = audio_ports
        .config()
        .context("Error while querying 'audio-ports' IO configuration")?;

    let mut next_input_mask = PortMask::enabled(audio_ports_config.inputs.len());
    let mut next_output_mask = PortMask::enabled(audio_ports_config.outputs.len());

    // 32 different attempts
    for _ in 0..32 {
        plugin.poll_callback(|event| match event {
            CallbackEvent::AudioPortsRescanList => {
                // rescan ports, reset masks
                audio_ports_config = audio_ports
                    .config()
                    .context("Error while querying 'audio-ports' IO configuration after rescan")?;

                // invalidate the activation masks
                next_input_mask = PortMask::enabled(audio_ports_config.inputs.len());
                next_output_mask = PortMask::enabled(audio_ports_config.outputs.len());

                Ok(())
            }
            _ => Ok(()),
        })?;

        let prev_input_mask = std::mem::replace(
            &mut next_input_mask,
            PortMask::random(&mut prng, audio_ports_config.inputs.len()),
        );

        let prev_output_mask = std::mem::replace(
            &mut next_output_mask,
            PortMask::random(&mut prng, audio_ports_config.outputs.len()),
        );

        let _span = Span::begin(
            "AudioPortActivationMask",
            record! {
                input_mask: format_args!("0b{:b}", next_input_mask.0),
                output_mask: format_args!("0b{:b}", next_output_mask.0)
            },
        );

        for input in 0..audio_ports_config.inputs.len() {
            let prev = prev_input_mask.is_active(input);
            let next = next_input_mask.is_active(input);

            if prev != next && !audio_ports_activation.set_active(true, input as u32, next, 32) {
                next_input_mask = next_input_mask.with_active(input, prev);
            }
        }

        for output in 0..audio_ports_config.outputs.len() {
            let prev = prev_output_mask.is_active(output);
            let next = next_output_mask.is_active(output);

            if prev != next && !audio_ports_activation.set_active(false, output as u32, next, 32) {
                next_output_mask = next_output_mask.with_active(output, prev);
            }
        }

        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut audio_buffers = AudioBuffers::new_in_place_f32(&audio_ports_config, BUFFER_SIZE)?;
                let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-1..=128);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

                for _ in 0..5 {
                    for buffer in process.audio_buffers().iter_mut() {
                        if let Some(input) = buffer.port().input() {
                            if next_input_mask.is_active(input) {
                                buffer.fill_white_noise(&mut prng);
                            } else {
                                buffer.fill_silence();
                            }
                        }
                    }

                    plugin.poll_callback();
                    process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run_with(ProcessRun {
                        block_size: BUFFER_SIZE,
                        output_ignore_mask: !next_output_mask.0, // ignore deactivated output ports for NaN checks
                        output_ignore_denormals: false,
                    })?;
                }

                Ok(())
            })
            .with_context(|| {
                format!(
                    "Error while processing audio with input mask 0b{:b} and output mask 0b{:b}",
                    next_input_mask.0, next_output_mask.0
                )
            })?;
    }

    Ok(TestStatus::Success { details: None })
}

fn print_layout(requests: &[AudioPortsRequest<'_>]) -> String {
    requests
        .iter()
        .map(|r| format!(" - {}", r))
        .collect::<Vec<_>>()
        .join("\n")
}
