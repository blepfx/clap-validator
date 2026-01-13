use crate::{
    plugin::{ext::Extension, instance::Plugin},
    util::{c_char_slice_to_string, clap_call, unsafe_clap_call},
};
use anyhow::Result;
use clap_sys::{
    ext::audio_ports_config::{
        clap_audio_ports_config, clap_plugin_audio_ports_config,
        clap_plugin_audio_ports_config_info, CLAP_EXT_AUDIO_PORTS_CONFIG,
        CLAP_EXT_AUDIO_PORTS_CONFIG_INFO,
    },
    id::clap_id,
};
use std::{ffi::CStr, mem::zeroed, ptr::NonNull};

#[derive(Debug)]
pub struct AudioPortsConfig<'a> {
    plugin: &'a Plugin<'a>,
    audio_ports_config: NonNull<clap_plugin_audio_ports_config>,
}

#[derive(Debug)]
pub struct AudioPortsConfigInfo<'a> {
    plugin: &'a Plugin<'a>,
    audio_ports_config_info: NonNull<clap_plugin_audio_ports_config_info>,
}

/// A configuration
#[derive(Debug, Clone)]
pub struct AudioPortsConfigConfig {
    pub id: clap_id,
    pub name: String,

    pub input_port_count: u32,
    pub output_port_count: u32,

    pub main_input_channel_count: Option<u32>,
    pub main_output_channel_count: Option<u32>,
}

impl<'a> Extension<&'a Plugin<'a>> for AudioPortsConfig<'a> {
    const EXTENSION_ID: &'static CStr = CLAP_EXT_AUDIO_PORTS_CONFIG;

    type Struct = clap_plugin_audio_ports_config;

    fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            audio_ports_config: extension_struct,
        }
    }
}

impl<'a> Extension<&'a Plugin<'a>> for AudioPortsConfigInfo<'a> {
    const EXTENSION_ID: &'static CStr = CLAP_EXT_AUDIO_PORTS_CONFIG_INFO;

    type Struct = clap_plugin_audio_ports_config_info;

    fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            audio_ports_config_info: extension_struct,
        }
    }
}

impl AudioPortsConfig<'_> {
    pub fn enumerate(&self) -> Result<Vec<AudioPortsConfigConfig>> {
        let audio_ports_config = self.audio_ports_config.as_ptr();
        let plugin = self.plugin.as_ptr();
        let count = unsafe_clap_call! { audio_ports_config=>count(plugin) };

        (0..count)
            .map(|i| unsafe {
                let mut dst = clap_audio_ports_config { ..zeroed() };
                let result = clap_call! { audio_ports_config=>get(plugin, i, &mut dst) };
                if !result {
                    anyhow::bail!("audio_ports_config::get({}) returned false", i);
                }

                Ok(AudioPortsConfigConfig {
                    id: dst.id,
                    name: c_char_slice_to_string(&dst.name)?,
                    input_port_count: dst.input_port_count,
                    output_port_count: dst.output_port_count,
                    main_input_channel_count: dst
                        .has_main_input
                        .then_some(dst.main_input_channel_count),
                    main_output_channel_count: dst
                        .has_main_output
                        .then_some(dst.main_output_channel_count),
                })
            })
            .collect()
    }

    pub fn select(&self, config_id: clap_id) -> Result<()> {
        let audio_ports_config = self.audio_ports_config.as_ptr();
        let plugin = self.plugin.as_ptr();
        let result = unsafe_clap_call! { audio_ports_config=>select(plugin, config_id) };
        if !result {
            anyhow::bail!("audio_ports_config::select() returned false");
        }

        Ok(())
    }
}

impl AudioPortsConfigInfo<'_> {
    pub fn current(&self) -> clap_id {
        let audio_ports_config_info = self.audio_ports_config_info.as_ptr();
        let plugin = self.plugin.as_ptr();
        unsafe_clap_call! { audio_ports_config_info=>current_config(plugin) }
    }

    // TODO:
}
