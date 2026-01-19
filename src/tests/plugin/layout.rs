use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::audio_ports_config::{AudioPortsConfig, AudioPortsConfigInfo};
use crate::plugin::ext::configurable_audio_ports::{AudioPortsRequest, ConfigurableAudioPorts};
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::instance::process::{AudioBuffers, ProcessConfig, ProcessData};
use crate::plugin::library::PluginLibrary;
use crate::tests::TestStatus;
use crate::tests::plugin::processing::run_simple;
use crate::tests::rng::{NoteGenerator, new_prng};
use anyhow::{Context, Result};
use rand::Rng;
use rand::seq::SliceRandom;
use rand_pcg::Pcg32;

const BUFFER_SIZE: usize = 512;

/// The test for `PluginTestCase::LayoutAudioPortsConfig`.
pub fn test_layout_audio_ports_config(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
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
                .map(|x| x.num_channels);

            let main_output_channels = config_audio_ports
                .outputs
                .first()
                .filter(|x| x.is_main)
                .map(|x| x.num_channels);

            anyhow::ensure!(
                config_audio_ports.inputs.len() as u32
                    == config_audio_ports_config.input_port_count,
                "The number of input audio ports for configuration '{}' ({}) does not match the \
                 number reported by 'audio-ports' ({})",
                config_audio_ports_config.name,
                config_audio_ports_config.input_port_count,
                config_audio_ports.inputs.len() as u32,
            );

            anyhow::ensure!(
                config_audio_ports.outputs.len() as u32
                    == config_audio_ports_config.output_port_count,
                "The number of output audio ports for configuration '{}' ({}) does not match the \
                 number reported by 'audio-ports' ({})",
                config_audio_ports_config.name,
                config_audio_ports_config.output_port_count,
                config_audio_ports.outputs.len() as u32,
            );

            match (
                main_input_channels,
                config_audio_ports_config.main_input_channel_count,
            ) {
                (None, None) => {}
                (Some(a), Some(b)) => anyhow::ensure!(
                    a == b,
                    "The number of channels in the main input port for the '{}' configuration \
                     info ({}) does not match the number reported by 'audio-ports' ({})",
                    config_audio_ports_config.name,
                    b,
                    a,
                ),
                (None, Some(_)) => {
                    anyhow::bail!(
                        "The configuration '{}' reports that a main input port exists, but \
                         'audio-ports' does not.",
                        config_audio_ports_config.name,
                    )
                }
                (Some(_), None) => anyhow::bail!(
                    "The configuration '{}' reports that main input port does not exist, but \
                     according to 'audio-ports' it does.",
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
                    "The number of channels in the main output port for the '{}' configuration \
                     info ({}) does not match the number reported by 'audio-ports' ({})",
                    config_audio_ports_config.name,
                    b,
                    a,
                ),
                (None, Some(_)) => {
                    anyhow::bail!(
                        "The configuration '{}' reports that a main output port exists, but \
                         'audio-ports' does not.",
                        config_audio_ports_config.name,
                    )
                }
                (Some(_), None) => anyhow::bail!(
                    "The configuration '{}' reports that main output port does not exist, but \
                     according to 'audio-ports' it does.",
                    config_audio_ports_config.name,
                ),
            }
        }

        // Check that the audio-ports-config-info matches the current config
        if let Some(audio_ports_config_info) = &audio_ports_config_info {
            anyhow::ensure!(
                audio_ports_config_info.current() == config_audio_ports_config.id,
                "The current configuration ID reported by 'audio-ports-config-info' ({}) does not \
                 match the last selected configuration ID ({})",
                audio_ports_config_info.current(),
                config_audio_ports_config.id,
            );

            // TODO: check info
        }

        let mut note_event_rng = NoteGenerator::new(&note_ports_config);
        let mut audio_buffers = AudioBuffers::new_in_place_f32(&config_audio_ports, BUFFER_SIZE);
        let mut process_data = ProcessData::new(&mut audio_buffers, ProcessConfig::default());

        run_simple(&plugin, &mut process_data, 5, |process_data| {
            process_data.buffers.randomize(&mut prng);
            note_event_rng.fill_event_queue(
                &mut prng,
                &process_data.input_events,
                process_data.block_size,
            );

            Ok(())
        })
        .with_context(|| {
            format!(
                "Error while processing audio with IO configuration '{}' ({})",
                config_audio_ports_config.name, config_audio_ports_config.id,
            )
        })?;
    }

    plugin
        .handle_callback()
        .context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}

/// The test for `PluginTestCase::LayoutConfigurableAudioPorts`.
pub fn test_layout_configurable_audio_ports(
    library: &PluginLibrary,
    plugin_id: &str,
) -> Result<TestStatus> {
    fn random_layout_requests(
        prng: &mut Pcg32,
        config: &AudioPortConfig,
    ) -> Vec<AudioPortsRequest> {
        let mut requests = Vec::new();

        for (i, _) in config.inputs.iter().enumerate() {
            requests.push(AudioPortsRequest {
                is_input: true,
                port_index: i as u32,
                channel_count: prng.random_range(0..=8),
            });
        }

        for (i, _) in config.outputs.iter().enumerate() {
            requests.push(AudioPortsRequest {
                is_input: false,
                port_index: i as u32,
                channel_count: prng.random_range(0..=8),
            });
        }

        requests.shuffle(prng);
        requests
    }

    fn print_layout_requests(requests: &[AudioPortsRequest]) -> String {
        let mut result = Vec::new();

        for request in requests {
            result.push(format!(
                "{}{}-{}ch",
                if request.is_input { "in" } else { "out" },
                request.port_index,
                request.channel_count,
            ));
        }

        result.join(" ")
    }

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

    while checks_total < 100 && checks_passed < 10 {
        let requests = random_layout_requests(&mut prng, &config_audio_ports);
        let can_apply = configurable_audio_ports.can_apply_configuration(requests.iter().cloned());
        let has_applied = configurable_audio_ports.apply_configuration(requests.iter().cloned());

        if can_apply != has_applied {
            anyhow::bail!(
                "The plugin returned conflicting results from 'can_apply_configuration' ({}) and \
                 'apply_configuration' ({}) for the following layout: {}",
                can_apply,
                has_applied,
                print_layout_requests(&requests),
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

        let mut note_event_rng = NoteGenerator::new(&note_ports_config);
        let mut audio_buffers = AudioBuffers::new_in_place_f32(&config_audio_ports, BUFFER_SIZE);
        let mut process_data = ProcessData::new(&mut audio_buffers, ProcessConfig::default());

        run_simple(&plugin, &mut process_data, 5, |process_data| {
            process_data.buffers.randomize(&mut prng);
            note_event_rng.fill_event_queue(
                &mut prng,
                &process_data.input_events,
                process_data.block_size,
            );

            Ok(())
        })
        .with_context(|| {
            format!(
                "Error while processing audio with the following configuration: {}",
                print_layout_requests(&requests)
            )
        })?;
    }

    plugin
        .handle_callback()
        .context("An error occured during a callback")?;

    Ok(TestStatus::Success { details: None })
}
