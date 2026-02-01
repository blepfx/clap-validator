use crate::plugin::ext::Extension;
use crate::plugin::instance::Plugin;
use crate::plugin::util::clap_call;
use clap_sys::ext::ambisonic::{CLAP_PORT_AMBISONIC, clap_ambisonic_config};
use clap_sys::ext::audio_ports::{CLAP_PORT_MONO, CLAP_PORT_STEREO};
use clap_sys::ext::configurable_audio_ports::{
    CLAP_EXT_CONFIGURABLE_AUDIO_PORTS, CLAP_EXT_CONFIGURABLE_AUDIO_PORTS_COMPAT, clap_audio_port_configuration_request,
    clap_plugin_configurable_audio_ports,
};
use clap_sys::ext::surround::CLAP_PORT_SURROUND;
use std::ffi::CStr;
use std::fmt::{Debug, Display};
use std::ptr::{NonNull, null};

#[derive(Debug, Clone, Copy)]
pub struct AudioPortsRequest<'a> {
    pub is_input: bool,
    pub port_index: u32,
    pub request_info: AudioPortsRequestInfo<'a>,
}

/// Different types of port details that can be requested.
#[derive(Debug, Clone, Copy)]
pub enum AudioPortsRequestInfo<'a> {
    Mono,
    Stereo,
    Untyped {
        channel_count: u32,
    },

    Ambisonic {
        channel_count: u32,
        config: &'a clap_ambisonic_config,
    },

    Surround {
        channel_map: &'a [u8],
    },
}

pub struct ConfigurableAudioPorts<'a> {
    plugin: &'a Plugin<'a>,
    configurable_audio_ports: NonNull<clap_plugin_configurable_audio_ports>,
}

impl<'a> Extension<&'a Plugin<'a>> for ConfigurableAudioPorts<'a> {
    const IDS: &'static [&'static CStr] = &[
        CLAP_EXT_CONFIGURABLE_AUDIO_PORTS,
        CLAP_EXT_CONFIGURABLE_AUDIO_PORTS_COMPAT,
    ];

    type Struct = clap_plugin_configurable_audio_ports;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            configurable_audio_ports: extension_struct,
        }
    }
}

impl<'a> ConfigurableAudioPorts<'a> {
    pub fn can_apply_configuration<'b>(&self, requests: impl IntoIterator<Item = AudioPortsRequest<'b>>) -> bool {
        self.plugin.status().assert_inactive();

        let requests = convert_requests(requests);
        let plugin = self.plugin.as_ptr();
        let ext = self.configurable_audio_ports.as_ptr();

        unsafe {
            clap_call! { ext=>can_apply_configuration(
                plugin,
                requests.as_ptr(),
                requests.len() as u32
            )}
        }
    }

    pub fn apply_configuration<'b>(&self, requests: impl IntoIterator<Item = AudioPortsRequest<'b>>) -> bool {
        self.plugin.status().assert_inactive();

        let requests = convert_requests(requests);
        let plugin = self.plugin.as_ptr();
        let ext = self.configurable_audio_ports.as_ptr();

        unsafe {
            clap_call! { ext=>apply_configuration(
                plugin,
                requests.as_ptr(),
                requests.len() as u32
            )}
        }
    }
}

fn convert_requests<'a>(
    requests: impl IntoIterator<Item = AudioPortsRequest<'a>>,
) -> Vec<clap_audio_port_configuration_request> {
    requests
        .into_iter()
        .map(|r| clap_audio_port_configuration_request {
            is_input: r.is_input,
            port_index: r.port_index,
            channel_count: match r.request_info {
                AudioPortsRequestInfo::Mono => 1,
                AudioPortsRequestInfo::Stereo => 2,
                AudioPortsRequestInfo::Untyped { channel_count } => channel_count,
                AudioPortsRequestInfo::Ambisonic { channel_count, .. } => channel_count,
                AudioPortsRequestInfo::Surround { channel_map } => channel_map.len() as u32,
            },
            port_type: match r.request_info {
                AudioPortsRequestInfo::Mono => CLAP_PORT_MONO.as_ptr(),
                AudioPortsRequestInfo::Stereo => CLAP_PORT_STEREO.as_ptr(),
                AudioPortsRequestInfo::Ambisonic { .. } => CLAP_PORT_AMBISONIC.as_ptr(),
                AudioPortsRequestInfo::Surround { .. } => CLAP_PORT_SURROUND.as_ptr(),
                AudioPortsRequestInfo::Untyped { .. } => null(),
            },
            port_details: match r.request_info {
                AudioPortsRequestInfo::Surround { channel_map } => channel_map.as_ptr() as *const _,
                AudioPortsRequestInfo::Ambisonic { config, .. } => config as *const clap_ambisonic_config as *const _,
                _ => null(),
            },
        })
        .collect::<Vec<_>>()
}

impl Display for AudioPortsRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} #{}: {}",
            if self.is_input { "Input" } else { "Output" },
            self.port_index,
            self.request_info
        )
    }
}

impl Display for AudioPortsRequestInfo<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioPortsRequestInfo::Mono => write!(f, "Mono"),
            AudioPortsRequestInfo::Stereo => write!(f, "Stereo"),
            AudioPortsRequestInfo::Untyped { channel_count } => {
                write!(f, "Untyped ({}ch)", channel_count)
            }
            AudioPortsRequestInfo::Ambisonic { channel_count, .. } => {
                write!(f, "Ambisonic ({}ch)", channel_count)
            }
            AudioPortsRequestInfo::Surround { channel_map } => {
                write!(f, "Surround ({}ch)", channel_map.len())
            }
        }
    }
}
