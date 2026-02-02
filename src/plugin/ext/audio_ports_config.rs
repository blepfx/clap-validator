use crate::plugin::ext::Extension;
use crate::plugin::ext::ambisonic::Ambisonic;
use crate::plugin::ext::audio_ports::{AudioPort, check_audio_port_info_valid, check_audio_port_type_consistent};
use crate::plugin::ext::surround::Surround;
use crate::plugin::instance::Plugin;
use crate::plugin::util::{c_char_slice_to_string, clap_call};
use anyhow::{Context, Result};
use clap_sys::ext::audio_ports::clap_audio_port_info;
use clap_sys::ext::audio_ports_config::*;
use clap_sys::id::clap_id;
use std::ffi::CStr;
use std::mem::zeroed;
use std::ptr::NonNull;

pub struct AudioPortsConfig<'a> {
    plugin: &'a Plugin<'a>,
    audio_ports_config: NonNull<clap_plugin_audio_ports_config>,
}

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

impl<'a> Extension for AudioPortsConfig<'a> {
    const IDS: &'static [&'static CStr] = &[CLAP_EXT_AUDIO_PORTS_CONFIG];

    type Plugin = &'a Plugin<'a>;
    type Struct = clap_plugin_audio_ports_config;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            audio_ports_config: extension_struct,
        }
    }
}

impl<'a> Extension for AudioPortsConfigInfo<'a> {
    const IDS: &'static [&'static CStr] = &[
        CLAP_EXT_AUDIO_PORTS_CONFIG_INFO,
        CLAP_EXT_AUDIO_PORTS_CONFIG_INFO_COMPAT,
    ];

    type Plugin = &'a Plugin<'a>;
    type Struct = clap_plugin_audio_ports_config_info;

    unsafe fn new(plugin: &'a Plugin<'a>, extension_struct: NonNull<Self::Struct>) -> Self {
        Self {
            plugin,
            audio_ports_config_info: extension_struct,
        }
    }
}

impl AudioPortsConfig<'_> {
    pub fn enumerate(&self) -> Result<Vec<AudioPortsConfigConfig>> {
        let ext_ambisonic = self.plugin.get_extension::<Ambisonic>();
        let ext_surround = self.plugin.get_extension::<Surround>();

        let audio_ports_config = self.audio_ports_config.as_ptr();
        let plugin = self.plugin.as_ptr();
        let count = unsafe {
            clap_call! { audio_ports_config=>count(plugin) }
        };

        (0..count)
            .map(|i| unsafe {
                let mut info = clap_audio_ports_config { ..zeroed() };
                let result = clap_call! { audio_ports_config=>get(plugin, i, &mut info) };
                if !result {
                    anyhow::bail!("audio_ports_config::get({}) returned false", i);
                }

                if info.has_main_input {
                    let port_type = if info.main_input_port_type.is_null() {
                        None
                    } else {
                        Some(CStr::from_ptr(info.main_input_port_type))
                    };

                    check_audio_port_type_consistent(
                        true,
                        0,
                        port_type,
                        info.main_input_channel_count,
                        ext_ambisonic.as_ref(),
                        ext_surround.as_ref(),
                    )
                    .with_context(|| format!("Inconsistent main input port info for config {i}"))?;
                }

                if info.has_main_output {
                    let port_type = if info.main_output_port_type.is_null() {
                        None
                    } else {
                        Some(CStr::from_ptr(info.main_output_port_type))
                    };

                    check_audio_port_type_consistent(
                        false,
                        0,
                        port_type,
                        info.main_output_channel_count,
                        ext_ambisonic.as_ref(),
                        ext_surround.as_ref(),
                    )
                    .with_context(|| format!("Inconsistent main output port info for config {i}"))?;
                }

                Ok(AudioPortsConfigConfig {
                    id: info.id,
                    name: c_char_slice_to_string(&info.name)?,
                    input_port_count: info.input_port_count,
                    output_port_count: info.output_port_count,
                    main_input_channel_count: info.has_main_input.then_some(info.main_input_channel_count),
                    main_output_channel_count: info.has_main_output.then_some(info.main_output_channel_count),
                })
            })
            .collect()
    }

    pub fn select(&self, config_id: clap_id) -> Result<()> {
        let audio_ports_config = self.audio_ports_config.as_ptr();
        let plugin = self.plugin.as_ptr();
        let result = unsafe {
            clap_call! { audio_ports_config=>select(plugin, config_id) }
        };

        if !result {
            anyhow::bail!("audio_ports_config::select() returned false");
        }

        Ok(())
    }
}

impl AudioPortsConfigInfo<'_> {
    /// Get the current selected audio ports configuration ID.
    pub fn current(&self) -> clap_id {
        let audio_ports_config_info = self.audio_ports_config_info.as_ptr();
        let plugin = self.plugin.as_ptr();

        unsafe {
            clap_call! { audio_ports_config_info=>current_config(plugin) }
        }
    }

    /// Get information about an audio port for a configuration.
    pub fn get(&self, config_id: clap_id, is_input: bool, port_index: u32) -> Result<AudioPort> {
        let info = unsafe {
            let audio_ports_config_info = self.audio_ports_config_info.as_ptr();
            let plugin = self.plugin.as_ptr();

            let mut info = clap_audio_port_info { ..zeroed() };
            if !clap_call! { audio_ports_config_info=>get(plugin, config_id, port_index, is_input, &mut info) } {
                anyhow::bail!("audio_ports_config_info::get() returned false");
            }

            info
        };

        check_audio_port_info_valid(self.plugin, is_input, port_index, &info)
    }
}
