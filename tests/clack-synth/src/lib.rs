use crate::params::{PolySynthParamModulations, PolySynthParams};
use crate::poly_oscillator::PolyOscillator;
use clack_extensions::state::PluginState;
use clack_extensions::{audio_ports::*, note_ports::*, params::*};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::process::ConstantMask;

mod oscillator;
mod params;
mod poly_oscillator;

pub struct PolySynthPlugin;

impl Plugin for PolySynthPlugin {
    type AudioProcessor<'a> = PolySynthAudioProcessor<'a>;
    type Shared<'a> = PolySynthPluginShared;
    type MainThread<'a> = PolySynthPluginMainThread<'a>;

    fn declare_extensions(
        builder: &mut PluginExtensions<Self>,
        _shared: Option<&PolySynthPluginShared>,
    ) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>();
    }
}

impl DefaultPluginFactory for PolySynthPlugin {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;

        PluginDescriptor::new("org.rust-audio.clack.polysynth", "Clack PolySynth Example")
            .with_features([SYNTHESIZER, MONO, INSTRUMENT])
    }

    fn new_shared(_host: HostSharedHandle) -> Result<PolySynthPluginShared, PluginError> {
        Ok(PolySynthPluginShared {
            params: PolySynthParams::new(),
        })
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PolySynthPluginShared,
    ) -> Result<PolySynthPluginMainThread<'a>, PluginError> {
        Ok(PolySynthPluginMainThread { shared })
    }
}

pub struct PolySynthAudioProcessor<'a> {
    poly_osc: PolyOscillator,
    modulation_values: PolySynthParamModulations,
    shared: &'a PolySynthPluginShared,
}

impl<'a> PluginAudioProcessor<'a, PolySynthPluginShared, PolySynthPluginMainThread<'a>>
    for PolySynthAudioProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PolySynthPluginMainThread,
        shared: &'a PolySynthPluginShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            poly_osc: PolyOscillator::new(16, audio_config.sample_rate as f32),
            modulation_values: PolySynthParamModulations::new(),
            shared,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let mut output_port = audio
            .output_port(0)
            .ok_or(PluginError::Message("No output port found"))?;

        let mut output_channels = output_port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("Expected f32 output"))?;

        let output_buffer = output_channels
            .channel_mut(0)
            .ok_or(PluginError::Message("Expected at least one channel"))?;

        output_buffer.fill(0.0);

        for event_batch in events.input.batch() {
            for event in event_batch.events() {
                self.handle_event(event);
            }

            let output_buffer = &mut output_buffer[event_batch.sample_bounds()];
            self.poly_osc.generate_next_samples(
                output_buffer,
                self.shared.params.get_volume(),
                self.modulation_values.volume(),
            );
        }

        // Copy the first channel to all other channels for mono output
        if output_channels.channel_count() > 1 {
            let (first_channel, other_channels) = output_channels.split_at_mut(1);
            let first_channel = first_channel.channel(0).unwrap();

            for other_channel in other_channels {
                other_channel.copy_from_slice(first_channel)
            }
        }

        if self.poly_osc.has_active_voices() {
            Ok(ProcessStatus::Continue)
        } else {
            audio
                .output_port(0)
                .unwrap()
                .set_constant_mask(ConstantMask::FULLY_CONSTANT);
            Ok(ProcessStatus::Sleep)
        }
    }

    fn stop_processing(&mut self) {
        self.poly_osc.stop_all();
    }
}

impl PolySynthAudioProcessor<'_> {
    fn handle_event(&mut self, event: &UnknownEvent) {
        match event.as_core_event() {
            Some(CoreEventSpace::NoteOn(event)) => self.poly_osc.handle_note_on(event),
            Some(CoreEventSpace::NoteOff(event)) => self.poly_osc.handle_note_off(event),
            Some(CoreEventSpace::ParamValue(event)) => {
                if event.pckn().matches_all() {
                    self.shared.params.handle_event(event)
                } else {
                    self.poly_osc.handle_param_value(event)
                }
            }
            Some(CoreEventSpace::ParamMod(event)) => {
                if event.pckn().matches_all() {
                    self.modulation_values.handle_event(event)
                } else {
                    self.poly_osc.handle_param_mod(event)
                }
            }
            _ => {}
        }
    }
}

impl PluginAudioPortsImpl for PolySynthPluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 0 } else { 1 }
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if !is_input && index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"main",
                channel_count: 1,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::MONO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginNotePortsImpl for PolySynthPluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 1 } else { 0 }
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 {
            writer.set(&NotePortInfo {
                id: ClapId::new(1),
                name: b"main",
                preferred_dialect: Some(NoteDialect::Clap),
                supported_dialects: NoteDialects::CLAP,
            })
        }
    }
}

pub struct PolySynthPluginShared {
    params: PolySynthParams,
}

impl PluginShared<'_> for PolySynthPluginShared {}

pub struct PolySynthPluginMainThread<'a> {
    shared: &'a PolySynthPluginShared,
}

impl<'a> PluginMainThread<'a, PolySynthPluginShared> for PolySynthPluginMainThread<'a> {}

clack_export_entry!(SinglePluginEntry<PolySynthPlugin>);
