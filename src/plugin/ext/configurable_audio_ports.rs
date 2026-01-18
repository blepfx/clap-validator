use crate::plugin::ext::Extension;
use crate::plugin::instance::Plugin;
use crate::util::unsafe_clap_call;
use clap_sys::ext::audio_ports::{CLAP_PORT_MONO, CLAP_PORT_STEREO};
use clap_sys::ext::configurable_audio_ports::{
    CLAP_EXT_CONFIGURABLE_AUDIO_PORTS, clap_audio_port_configuration_request,
    clap_plugin_configurable_audio_ports,
};
use std::ffi::CStr;
use std::ptr::{NonNull, null};

/// TODO: surround/ambisonic extensions?
#[derive(Debug, Clone, Copy)]
pub struct AudioPortsRequest {
    pub is_input: bool,
    pub port_index: u32,
    pub channel_count: u32,
}

pub struct ConfigurableAudioPorts<'a> {
    plugin: &'a Plugin<'a>,
    configurable_audio_ports: NonNull<clap_plugin_configurable_audio_ports>,
}

impl<'a> Extension<&'a Plugin<'a>> for ConfigurableAudioPorts<'a> {
    const EXTENSION_ID: &'static CStr = CLAP_EXT_CONFIGURABLE_AUDIO_PORTS;

    type Struct = clap_plugin_configurable_audio_ports;

    fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            configurable_audio_ports: extension_struct,
        }
    }
}

impl<'a> ConfigurableAudioPorts<'a> {
    pub fn can_apply_configuration(
        &self,
        requests: impl IntoIterator<Item = AudioPortsRequest>,
    ) -> bool {
        self.plugin.status().assert_inactive();

        let requests = requests
            .into_iter()
            .map(|r| clap_audio_port_configuration_request {
                is_input: r.is_input,
                port_index: r.port_index,
                channel_count: r.channel_count,
                port_details: null(),
                port_type: match r.channel_count {
                    1 => CLAP_PORT_MONO.as_ptr(),
                    2 => CLAP_PORT_STEREO.as_ptr(),
                    _ => null(),
                },
            })
            .collect::<Vec<_>>();

        let plugin = self.plugin.as_ptr();
        let ext = self.configurable_audio_ports.as_ptr();

        unsafe_clap_call! { ext=>can_apply_configuration(
            plugin,
            requests.as_ptr(),
            requests.len() as u32
        )}
    }

    pub fn apply_configuration(
        &self,
        requests: impl IntoIterator<Item = AudioPortsRequest>,
    ) -> bool {
        self.plugin.status().assert_inactive();

        let requests = requests
            .into_iter()
            .map(|r| clap_audio_port_configuration_request {
                is_input: r.is_input,
                port_index: r.port_index,
                channel_count: r.channel_count,
                port_details: null(),
                port_type: match r.channel_count {
                    1 => CLAP_PORT_MONO.as_ptr(),
                    2 => CLAP_PORT_STEREO.as_ptr(),
                    _ => null(),
                },
            })
            .collect::<Vec<_>>();

        let plugin = self.plugin.as_ptr();
        let ext = self.configurable_audio_ports.as_ptr();

        unsafe_clap_call! { ext=>can_apply_configuration(
            plugin,
            requests.as_ptr(),
            requests.len() as u32
        )}
    }
}
