use crate::plugin::ext::audio_ports::{AudioPortConfig, AudioPorts};
use crate::plugin::ext::note_ports::NotePorts;
use crate::plugin::ext::params::Params;
use crate::plugin::ext::state::State;
use crate::plugin::instance::CallbackEvent;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub struct FuzzContext {
    pub path: PathBuf,
    pub plugin_id: String,
    pub duration: Duration,
}

pub fn run(context: FuzzContext) -> Result<()> {
    let start = std::time::Instant::now();
    let library = crate::plugin::library::PluginLibrary::load(&context.path)?;
    let plugin = library.create_plugin(&context.plugin_id)?;

    let ext_audio_ports = plugin.get_extension::<AudioPorts>();
    let ext_note_ports = plugin.get_extension::<NotePorts>();
    let ext_params = plugin.get_extension::<Params>();
    let ext_state = plugin.get_extension::<State>();

    let mut audio_ports_config = match ext_audio_ports.as_ref() {
        Some(ext) => ext.config()?,
        None => AudioPortConfig::default(),
    };

    let mut note_ports_config = match ext_note_ports.as_ref() {
        Some(ext) => ext.config()?,
        None => Default::default(),
    };

    let mut params = match ext_params.as_ref() {
        Some(ext) => ext.info()?,
        None => Default::default(),
    };

    while start.elapsed() < context.duration {
        plugin.on_parallel(
            |plugin| {
                plugin.poll_callback(|e| match e {
                    CallbackEvent::ParamsRescanAll | CallbackEvent::ParamsRescanInfo => {
                        if let Some(ext) = ext_params.as_ref() {
                            params = ext.info().unwrap_or_default();
                        }

                        Ok(())
                    }

                    CallbackEvent::AudioPortsRescanAll => {
                        if let Some(ext) = ext_audio_ports.as_ref() {
                            audio_ports_config = ext.config().unwrap_or_default();
                        }

                        Ok(())
                    }

                    CallbackEvent::NotePortsRescanAll => {
                        if let Some(ext) = ext_note_ports.as_ref() {
                            note_ports_config = ext.config().unwrap_or_default();
                        }

                        Ok(())
                    }

                    _ => Ok(()),
                })?;

                Ok(Some(Duration::from_millis(16)))
            },
            |plugin| {
                
                


                 Ok(())
            },
        )?;
    }

    Ok(())
}
