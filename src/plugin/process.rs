//! Data structures and functions surrounding audio processing.
use crate::plugin::instance::{PluginAudioThread, PluginStatus, ProcessStatus};
use crate::plugin::util::Proxy;
use anyhow::Result;
use clap_sys::process::*;

mod buffer;
mod events;
mod transport;

pub use buffer::*;
pub use events::*;
pub use transport::*;

pub struct ProcessScope<'a> {
    plugin: &'a PluginAudioThread<'a>,
    buffer: &'a mut AudioBuffers,

    events_input: Proxy<InputEventQueue>,
    events_output: Proxy<OutputEventQueue>,

    transport: TransportState,
    sample_rate: f64,
}

impl<'a> ProcessScope<'a> {
    pub fn new(plugin: &'a PluginAudioThread, buffer: &'a mut AudioBuffers) -> Result<Self> {
        Self::with_sample_rate(plugin, buffer, 44100.0)
    }

    pub fn with_sample_rate(
        plugin: &'a PluginAudioThread,
        buffer: &'a mut AudioBuffers,
        sample_rate: f64,
    ) -> Result<Self> {
        plugin.status().assert_is(PluginStatus::Deactivated);

        Ok(ProcessScope {
            plugin,
            buffer,
            events_input: InputEventQueue::new(),
            events_output: OutputEventQueue::new(),
            transport: TransportState::dummy(),
            sample_rate,
        })
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn max_block_size(&self) -> u32 {
        self.buffer.samples()
    }

    pub fn add_events(&mut self, events: impl IntoIterator<Item = Event>) {
        self.events_input.add_events(events);
    }

    #[allow(unused)]
    pub fn read_events(&self) -> Vec<Event> {
        self.events_output.read()
    }

    pub fn transport(&mut self) -> &mut TransportState {
        &mut self.transport
    }

    pub fn audio_buffers(&mut self) -> &mut AudioBuffers {
        self.buffer
    }

    pub fn reset(&mut self) {
        if self.plugin.status() >= PluginStatus::Activated {
            self.plugin.reset();
        }
    }

    pub fn run(&mut self) -> Result<ProcessStatus> {
        self.run_with_block_size(self.max_block_size())
    }

    pub fn run_with_block_size(&mut self, samples: u32) -> Result<ProcessStatus> {
        assert!(samples > 0 && samples <= self.buffer.samples());

        // check for requested restart
        if self.plugin.shared().requested_restart.load() {
            self.restart();
        }

        // check state, activate if needed
        if self.plugin.status() == PluginStatus::Deactivated {
            self.plugin.shared().requested_restart.store(false);

            let sample_rate = self.sample_rate;
            let buffer_size = self.buffer.samples();
            self.plugin
                .on_main_thread(move |plugin| plugin.activate(sample_rate, 1, buffer_size))?;
        }

        // start processing if needed
        if self.plugin.status() == PluginStatus::Activated {
            self.plugin.start_processing()?;
        }

        // check that we dont overfill the input event queue
        assert!(
            self.events_input.last_event_time().is_none_or(|t| t < samples),
            "The input event queue contains events beyond the current processing block size"
        );

        // prepare output event queue for processing
        self.events_output.clear();

        // prepare output audio buffers for processing
        // this is used to detect uninitialized output buffers
        for buffer in self.buffer.iter_mut() {
            if buffer.port().input().is_none() {
                buffer.fill(CHECK_NAN_F32, CHECK_NAN_F64);
            }
        }

        // save original buffers for consistency check
        let original_buffers = self.buffer[..].to_owned();

        // run processing
        let status = self.buffer.process(|inputs, outputs| {
            let transport = self.transport.as_clap_transport(0);
            self.plugin.process(&clap_process {
                steady_time: self.transport.sample_pos.map_or(-1, |f| f as i64),
                frames_count: samples,
                transport: if self.transport.is_freerun {
                    std::ptr::null()
                } else {
                    &transport as *const _
                },
                audio_inputs: inputs.as_ptr(),
                audio_outputs: outputs.as_mut_ptr(),
                audio_inputs_count: inputs.len() as u32,
                audio_outputs_count: outputs.len() as u32,
                in_events: Proxy::vtable(&self.events_input),
                out_events: Proxy::vtable(&self.events_output),
            })
        })?;

        // clear input event queue and advance transport
        self.events_input.clear();
        self.transport.advance(samples as i64, self.sample_rate());

        // check output audio buffers for NaNs or infinities
        check_process_call_consistency(&self.buffer[..], &original_buffers, &self.events_output.read(), samples)?;

        Ok(status)
    }

    pub fn restart(&mut self) {
        if self.plugin.status() == PluginStatus::Processing {
            self.plugin.stop_processing();
        }

        if self.plugin.status() == PluginStatus::Activated {
            self.plugin.on_main_thread(|plugin| {
                plugin.deactivate();
            });
        }
    }
}

impl Drop for ProcessScope<'_> {
    fn drop(&mut self) {
        self.restart();
    }
}

/// NaN values used for checking if output buffers have been written to.
/// These are quiet NaNs with a specific payload to avoid accidental matches with other NaN values.
/// The payload is chosen to be unlikely to appear in normal processing.
const CHECK_NAN_F32: f32 = f32::from_bits(0x7FC0_1234);
/// See [`CHECK_NAN_F32`].
const CHECK_NAN_F64: f64 = f64::from_bits(0x7FF8_1234_5678_1234);

/// The process for consistency. This verifies that the output buffer has been written to, doesn't contain any NaN,
/// infinite, or denormal values, that the input buffers have not been modified by the plugin, and
/// that the output event queue is monotonically ordered.
fn check_process_call_consistency(
    resulting_buffers: &[AudioBuffer],
    original_buffers: &[AudioBuffer],
    output_events: &[Event],
    block_size: u32,
) -> Result<()> {
    for (buffer, before) in resulting_buffers.iter().zip(original_buffers.iter()) {
        // Input-only buffers must not be overwritten during out of place processing
        match buffer.port() {
            AudioBufferPort::Input(index) => {
                if !buffer.is_same(before) {
                    anyhow::bail!(
                        "The plugin has overwritten an input buffer (index {index}) during out-of-place processing."
                    );
                }
            }

            // Output buffers must not contain any non-finite or denormal values
            AudioBufferPort::Output(port_idx) | AudioBufferPort::Inplace(_, port_idx) => {
                let maybe_non_finite = (0..buffer.channels())
                    .flat_map(|channel| (0..block_size).map(move |sample| (channel, sample)))
                    .find_map(|(channel, sample)| {
                        let x = buffer.get(channel, sample);
                        if x.either(
                            |x| !x.is_finite() || x.is_subnormal(),
                            |x| !x.is_finite() || x.is_subnormal(),
                        ) {
                            Some((x, channel, sample))
                        } else {
                            None
                        }
                    });

                if let Some((sample, channel_idx, sample_idx)) = maybe_non_finite {
                    let is_subnormal = sample.either(|x| x.is_subnormal(), |x| x.is_subnormal());
                    let is_unwritten = sample.either(
                        |x| x.to_bits() == CHECK_NAN_F32.to_bits(),
                        |x| x.to_bits() == CHECK_NAN_F64.to_bits(),
                    );

                    if is_subnormal {
                        anyhow::bail!(
                            "The sample written to output port {port_idx}, channel {channel_idx}, and sample index \
                             {sample_idx} is subnormal ({sample})."
                        );
                    } else if is_unwritten {
                        anyhow::bail!(
                            "The sample at output port {port_idx}, channel {channel_idx}, and sample index \
                             {sample_idx} was left unwritten."
                        );
                    } else {
                        anyhow::bail!(
                            "The sample written to output port {port_idx}, channel {channel_idx}, and sample index \
                             {sample_idx} is {sample}."
                        );
                    }
                }
            }
        }
    }

    // If the plugin output any events, then they should be in a monotonically increasing order
    let mut last_event_time = 0;
    for event in output_events {
        let event_time = event.header().time;
        if event_time < last_event_time {
            anyhow::bail!(
                "The plugin output an event for sample {event_time} after it had previously output an event for \
                 sample {last_event_time}."
            )
        }

        if event_time >= block_size {
            anyhow::bail!(
                "The plugin output an event for sample {event_time} but the audio buffer only contains {block_size} \
                 samples."
            )
        }

        last_event_time = event_time;
    }

    Ok(())
}
