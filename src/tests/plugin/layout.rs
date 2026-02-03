use crate::plugin::ext::audio_ports::AudioPorts;
use crate::plugin::ext::audio_ports_activation::AudioPortsActivation;
use crate::plugin::ext::audio_ports_config::{AudioPortsConfig, AudioPortsConfigInfo};
use crate::plugin::ext::configurable_audio_ports::ConfigurableAudioPorts;
use crate::plugin::ext::note_ports::{NotePortConfig, NotePorts};
use crate::plugin::library::PluginLibrary;
use crate::plugin::process::{AudioBuffers, ProcessScope};
use crate::tests::TestStatus;
use crate::tests::rng::{NoteGenerator, new_prng, random_layout_requests};
use anyhow::{Context, Result};

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
        audio_ports_config
            .select(config_audio_ports_config.id)
            .with_context(|| {
                format!(
                    "Could not select audio port configuration '{}' ({})",
                    config_audio_ports_config.name, config_audio_ports_config.id,
                )
            })?;

        let config_audio_ports = audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?;

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
    const MAX_PASSED_CHECKS: u32 = 20;

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

        let can_apply = configurable_audio_ports.can_apply_configuration(requests.iter().copied());
        let has_applied = configurable_audio_ports.apply_configuration(requests.iter().copied());

        if can_apply != has_applied {
            anyhow::bail!(
                "The plugin returned conflicting results from 'can_apply_configuration' ({}) and \
                 'apply_configuration' ({}) for the following layout: {}",
                can_apply,
                has_applied,
                requests.iter().map(|r| format!("{}", r)).collect::<Vec<_>>().join(", ")
            );
        }

        if has_applied {
            checks_total += 1;
            checks_passed += 1;
        } else {
            checks_total += 1;
            continue;
        }

        let config_audio_ports = audio_ports
            .config()
            .context("Error while querying 'audio-ports' IO configuration")?;

        plugin
            .on_audio_thread(|plugin| -> Result<()> {
                let mut audio_buffers = AudioBuffers::new_in_place_f32(&config_audio_ports, BUFFER_SIZE)?;
                let mut note_rng = NoteGenerator::new(&note_ports_config).with_sample_offset_range(-1..=128);
                let mut process = ProcessScope::new(&plugin, &mut audio_buffers)?;

                for _ in 0..5 {
                    process.audio_buffers().fill_white_noise(&mut prng);
                    process.add_events(note_rng.generate_events(&mut prng, BUFFER_SIZE));
                    process.run()?;
                }

                Ok(())
            })
            .with_context(|| {
                format!(
                    "Error while processing audio with the following configuration: {}",
                    requests.iter().map(|r| format!("{}", r)).collect::<Vec<_>>().join(", ")
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

    let note_ports_config = match plugin.get_extension::<NotePorts>() {
        Some(note_ports) => note_ports
            .config()
            .context("Error while querying 'note-ports' IO configuration")?,
        None => NotePortConfig::default(),
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

    Ok(TestStatus::Success { details: None })
}
