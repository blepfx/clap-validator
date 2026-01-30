//! Abstractions for interacting with the `audio-ports` extension.

use super::Extension;
use crate::plugin::ext::ambisonic::Ambisonic;
use crate::plugin::ext::surround::Surround;
use crate::plugin::instance::Plugin;
use crate::util::clap_call;
use anyhow::{Context, Result};
use clap_sys::ext::ambisonic::CLAP_PORT_AMBISONIC;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::surround::CLAP_PORT_SURROUND;
use clap_sys::id::CLAP_INVALID_ID;
use std::collections::HashMap;
use std::ffi::CStr;
use std::ptr::NonNull;

/// Abstraction for the `audio-ports` extension covering the main thread functionality.
pub struct AudioPorts<'a> {
    plugin: &'a Plugin<'a>,
    audio_ports: NonNull<clap_plugin_audio_ports>,
}

/// The audio port configuration for a plugin.
#[derive(Debug, Default)]
pub struct AudioPortConfig {
    /// Configuration for the plugin's input audio ports.
    pub inputs: Vec<AudioPort>,
    /// Configuration for the plugin's output audio ports.
    pub outputs: Vec<AudioPort>,
}

/// The configuration for a single audio port.
#[derive(Debug)]
pub struct AudioPort {
    /// Whether this is the main audio port.
    pub is_main: bool,

    /// The number of channels for an audio port.
    pub num_channels: u32,

    /// The index if the output/input port this input/output port should be connected to. This is
    /// the index in the other **port list**, not a stable ID (which have already been translated).
    pub in_place_pair_idx: Option<usize>,

    /// Supports 64 bit processing
    pub supports_double_sample_size: bool,

    /// All ports with this flag require common sample size
    #[allow(unused)] // TODO: use for future mixed precision processing tests
    pub requires_common_sample_size: bool,
}

impl<'a> Extension<&'a Plugin<'a>> for AudioPorts<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_AUDIO_PORTS];

    type Struct = clap_plugin_audio_ports;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            audio_ports: extension_struct,
        }
    }
}

impl AudioPorts<'_> {
    /// Get the audio port configuration for this plugin. This automatically performs a number of
    /// consistency checks on the plugin's audio port configuration.
    pub fn config(&self) -> Result<AudioPortConfig> {
        let mut config = AudioPortConfig::default();

        let audio_ports = self.audio_ports.as_ptr();
        let plugin = self.plugin.as_ptr();
        let (num_inputs, num_outputs) = unsafe {
            (
                clap_call! { audio_ports=>count(plugin, true) },
                clap_call! { audio_ports=>count(plugin, false) },
            )
        };

        // Audio ports have a stable ID attribute that can be used to connect input and output ports
        // so the host can do in-place processing. This uses stable IDs rather than the indices in
        // the list. To make it easier for us, we'll translate those stable IDs to vector indices.
        // These two hashmaps are keyed by the port's stable ID, and the value is a pair containing
        // the port's index in the input/output port vector, and the stable ID of its in-place pair
        // port.
        let mut input_stable_index_pairs: HashMap<u32, (usize, u32)> = HashMap::new();
        let mut output_stable_index_pairs: HashMap<u32, (usize, u32)> = HashMap::new();

        for index in 0..num_inputs {
            let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
            let success = unsafe {
                clap_call! { audio_ports=>get(plugin, index, true, &mut info) }
            };

            if !success {
                anyhow::bail!(
                    "Plugin returned an error when querying input audio port {index} ({num_inputs} total input ports)."
                );
            }

            // We'll convert these stable IDs to vector indices later
            if input_stable_index_pairs
                .insert(info.id, (index as usize, info.in_place_pair))
                .is_some()
            {
                anyhow::bail!(
                    "The stable ID of input audio port {index} (id={}) is a duplicate.",
                    info.id
                );
            }

            config
                .inputs
                .push(check_audio_port_info_valid(self.plugin, true, index, &info)?);
        }

        for index in 0..num_outputs {
            let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
            let success = unsafe {
                clap_call! { audio_ports=>get(plugin, index, false, &mut info) }
            };

            if !success {
                anyhow::bail!(
                    "Plugin returned an error when querying output audio port {index} ({num_outputs} total output \
                     ports)."
                );
            }

            if output_stable_index_pairs
                .insert(info.id, (index as usize, info.in_place_pair))
                .is_some()
            {
                anyhow::bail!(
                    "The stable ID of output audio port {index} (id={}) is a duplicate.",
                    info.id
                );
            }

            config
                .outputs
                .push(check_audio_port_info_valid(self.plugin, false, index, &info)?);
        }

        let has_single_precision_requires_common_port = config
            .inputs
            .iter()
            .chain(config.outputs.iter())
            .any(|port| port.requires_common_sample_size && !port.supports_double_sample_size);

        let has_double_precision_requires_common_port = config
            .inputs
            .iter()
            .chain(config.outputs.iter())
            .any(|port| port.requires_common_sample_size && port.supports_double_sample_size);

        // this implies that the common sample size requirement is useless (i.e. every port can only support
        // 32bit sample size) and nullifies the 64 bit support of the other ports
        if has_single_precision_requires_common_port && has_double_precision_requires_common_port {
            anyhow::bail!(
                "The plugin has audio ports that require common sample size, but some of these ports only support \
                 32-bit sample size while others support 64-bit sample size."
            );
        }

        // Now we need to convert the stable in-place pair indices to vector indices
        for (input_stable_id, (input_port_idx, pair_stable_id)) in input_stable_index_pairs
            .iter()
            .filter(|(_, (_, pair_stable_id))| *pair_stable_id != CLAP_INVALID_ID)
        {
            match output_stable_index_pairs
                .iter()
                .find(|(output_stable_id, (_, _))| *output_stable_id == pair_stable_id)
            {
                // This relation should be symmetrical
                Some((_, (pair_output_port_idx, output_pair_stable_id)))
                    if output_pair_stable_id == input_stable_id =>
                {
                    config.inputs[*input_port_idx].in_place_pair_idx = Some(*pair_output_port_idx);
                    config.outputs[*pair_output_port_idx].in_place_pair_idx = Some(*input_port_idx);
                }
                Some((output_stable_id, (pair_output_port_idx, output_pair_stable_id))) => {
                    anyhow::bail!(
                        "Input port {input_port_idx} with stable ID {input_stable_id} is connected to output port \
                         {pair_output_port_idx} with stable ID {output_stable_id} through an in-place pair, but the \
                         relation is not symmetrical. The output port reports to have an in-place pair with stable ID \
                         {output_pair_stable_id}."
                    )
                }
                None => anyhow::bail!(
                    "Input port {input_port_idx} with stable ID {input_stable_id} claims to be connected to an output \
                     port with stable ID {pair_stable_id} through an in-place pair, but this port does not exist."
                ),
            }
        }

        // This needs to be repeated for output ports that are connected to input ports in case an
        // output port has a stable ID pair but the corresponding input port does not
        for (output_stable_id, (output_port_idx, pair_stable_id)) in output_stable_index_pairs
            .iter()
            .filter(|(_, (_, pair_stable_id))| *pair_stable_id != CLAP_INVALID_ID)
        {
            match input_stable_index_pairs
                .iter()
                .find(|(input_stable_id, (_, _))| *input_stable_id == pair_stable_id)
            {
                Some((_, (pair_input_port_idx, input_pair_stable_id))) if input_pair_stable_id == output_stable_id => {
                    // We should have already done this. If this is not the case, then this is an
                    // error in the validator
                    assert_eq!(
                        config.inputs[*output_port_idx].in_place_pair_idx,
                        Some(*pair_input_port_idx)
                    );
                    assert_eq!(
                        config.inputs[*pair_input_port_idx].in_place_pair_idx,
                        Some(*output_port_idx)
                    );
                }
                Some((input_stable_id, (pair_input_port_idx, input_pair_stable_id))) => {
                    anyhow::bail!(
                        "Output port {output_port_idx} with stable ID {output_stable_id} is connected to input port \
                         {pair_input_port_idx} with stable ID {input_stable_id} through an in-place pair, but the \
                         relation is not symmetrical. The input port reports to have an in-place pair with stable ID \
                         {input_pair_stable_id}."
                    )
                }
                None => anyhow::bail!(
                    "Output port {output_port_idx} with stable ID {output_stable_id} claims to be connected to an \
                     input port with stable ID {pair_stable_id} through an in-place pair, but this port does not \
                     exist."
                ),
            }
        }

        Ok(config)
    }
}

pub fn check_audio_port_info_valid(
    plugin: &Plugin,
    is_input: bool,
    port_index: u32,
    info: &clap_audio_port_info,
) -> Result<AudioPort> {
    let ext_ambisonic = plugin.get_extension::<Ambisonic>();
    let ext_surround = plugin.get_extension::<Surround>();

    let port_type = if info.port_type.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(info.port_type) })
    };

    // check consistency between port type and channel count / extensions
    check_audio_port_type_consistent(
        is_input,
        port_index,
        port_type,
        info.channel_count,
        ext_ambisonic.as_ref(),
        ext_surround.as_ref(),
    )
    .with_context(|| {
        format!(
            "Inconsistent port info for {} port {port_index}",
            if is_input { "input" } else { "output" }
        )
    })?;

    // if the main port flag is set, the port index must be 0
    let is_main = (info.flags & CLAP_AUDIO_PORT_IS_MAIN) != 0;
    if is_main && port_index != 0 {
        anyhow::bail!(
            "{} audio port {port_index} is marked as main, but it is not the first port in the list.",
            if is_input { "Input" } else { "Output" }
        );
    }

    let supports_double_sample_size = (info.flags & CLAP_AUDIO_PORT_SUPPORTS_64BITS) != 0;
    let requires_common_sample_size = (info.flags & CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE) != 0;
    let prefers_double_sample_size = (info.flags & CLAP_AUDIO_PORT_PREFERS_64BITS) != 0;

    if !supports_double_sample_size && prefers_double_sample_size {
        anyhow::bail!(
            "{} audio port {port_index} prefers 64-bit sample size, but does not support it.",
            if is_input { "Input" } else { "Output" }
        );
    }

    Ok(AudioPort {
        is_main: (info.flags & CLAP_AUDIO_PORT_IS_MAIN) != 0,
        num_channels: info.channel_count,
        in_place_pair_idx: None,

        supports_double_sample_size,
        requires_common_sample_size,
    })
}

/// Check if the returned port information consistent with the audio port type, ambisonic extension, surround extension, etc.
/// Returns an error if the port information is not consistent.
pub fn check_audio_port_type_consistent(
    is_input: bool,
    port_index: u32,
    port_type: Option<&CStr>,
    channel_count: u32,
    ext_ambisonic: Option<&Ambisonic>,
    ext_surround: Option<&Surround>,
) -> Result<()> {
    if port_type.is_none() {
        return Ok(());
    }

    if port_type == Some(CLAP_PORT_MONO) {
        if channel_count == 1 {
            Ok(())
        } else {
            anyhow::bail!(
                "Audio port type is 'mono', but the audio port has {} channels.",
                channel_count
            );
        }
    } else if port_type == Some(CLAP_PORT_STEREO) {
        if channel_count == 2 {
            Ok(())
        } else {
            anyhow::bail!(
                "Audio port type is 'stereo', but the audio port has {} channel(s).",
                channel_count
            );
        }
    } else if port_type == Some(CLAP_PORT_SURROUND) {
        let Some(ext_surround) = ext_surround else {
            anyhow::bail!("Audio port type is 'surround', but the plugin does not implement the 'surround' extension.");
        };

        let channel_map = ext_surround.get_channel_map(is_input, port_index, channel_count);
        if channel_map.len() as u32 != channel_count {
            anyhow::bail!(
                "The surround channel map returned by 'clap_plugin_surround::get_channel_map' has length {}, but the \
                 audio port has {} channels.",
                channel_map.len(),
                channel_count
            );
        }

        let mask = channel_map.iter().fold(0u64, |acc, &ch| acc | (1u64 << ch));
        if !ext_surround.is_channel_mask_supported(mask) {
            anyhow::bail!(
                "The surround channel mask {mask:#b} returned by 'clap_plugin_surround::get_channel_map' is not \
                 supported by the plugin ('clap_plugin_surround::is_channel_mask_supported' returned false)."
            );
        }

        Ok(())
    } else if port_type == Some(CLAP_PORT_AMBISONIC) {
        let Some(ext_ambisonic) = ext_ambisonic else {
            anyhow::bail!(
                "Audio port type is 'ambisonic', but the plugin does not implement the 'ambisonic' extension."
            );
        };

        // ambisonic audio requires (N^2) channels where N is the ambisonics order
        if channel_count.isqrt().pow(2) != channel_count {
            anyhow::bail!(
                "Expected a perfect square (N^2 where N is the ambisonics order) number of channels for ambisonic \
                 audio port, but the audio port has {} channels.",
                channel_count
            );
        }

        let config = ext_ambisonic
            .get_config(is_input, port_index)
            .context("Failed to get ambisonic configuration for the port.")?;

        if !ext_ambisonic.is_config_supported(&config) {
            anyhow::bail!(
                "The ambisonic configuration returned by 'clap_plugin_ambisonic::get_config' is not supported by the \
                 plugin ('clap_plugin_ambisonic::is_config_supported' returned false).",
            );
        }

        Ok(())
    } else {
        log::warn!("Unknown audio port type '{port_type:?}'");
        Ok(())
    }
}
