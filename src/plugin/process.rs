//! Data structures and functions surrounding audio processing.
use crate::plugin::instance::{PluginAudioThread, PluginStatus};
use anyhow::Result;
use clap_sys::process::*;
use std::pin::Pin;

mod buffer;
mod events;
mod transport;

pub use buffer::*;
pub use events::*;
pub use transport::*;

pub struct ProcessScope<'a> {
    plugin: &'a PluginAudioThread<'a>,
    buffer: &'a mut AudioBuffers,

    events_input: Pin<Box<EventQueue>>,
    events_output: Pin<Box<EventQueue>>,

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

            events_input: EventQueue::new(),
            events_output: EventQueue::new(),
            transport: TransportState::default(),
            sample_rate,
        })
    }

    pub fn max_block_size(&self) -> u32 {
        self.buffer.len()
    }

    pub fn input_queue(&self) -> &EventQueue {
        &self.events_input
    }

    pub fn output_queue(&self) -> &EventQueue {
        &self.events_output
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

    pub fn run(&mut self) -> Result<()> {
        self.run_with_block_size(self.buffer.len())
    }

    pub fn run_with_block_size(&mut self, samples: u32) -> Result<()> {
        assert!(samples > 0 && samples <= self.buffer.len());

        // check for requested restart
        if self.plugin.shared().requested_restart.load() {
            self.restart();
        }

        // check state, activate if needed
        if self.plugin.status() == PluginStatus::Deactivated {
            self.plugin.shared().requested_restart.store(false);
            self.plugin
                .send_main_thread(|plugin| plugin.activate(self.sample_rate, 1, self.buffer.len()))?;
        }

        // start processing if needed
        if self.plugin.status() == PluginStatus::Activated {
            self.plugin.start_processing()?;
        }

        // prepare output event queue for processing
        self.events_output.clear();

        // prepare output audio buffers for processing
        // this is used to detect uninitialized output buffers
        for buffer in self.buffer.buffers_mut() {
            if buffer.is_output_only() {
                buffer.fill(CHECK_NAN_F32, CHECK_NAN_F64);
            }
        }

        // save original buffers for consistency check
        let original_buffers = self.buffer.buffers().to_owned();

        // run processing
        let transport = self.transport.as_clap_transport(0);
        let (inputs, outputs) = self.buffer.clap_buffers();
        self.plugin.process(&clap_process {
            steady_time: self.transport.sample_pos,
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
            in_events: self.events_input.vtable_input(),
            out_events: self.events_output.vtable_output(),
        })?;

        // clear input event queue and advance transport
        self.events_input.clear();
        self.transport.advance(samples, self.sample_rate);

        // check output audio buffers for NaNs or infinities
        check_process_call_consistency(self.buffer.buffers(), &original_buffers, self.output_queue(), samples)
    }

    pub fn restart(&mut self) {
        if self.plugin.status() == PluginStatus::Processing {
            self.plugin.stop_processing();
        }

        if self.plugin.status() == PluginStatus::Activated {
            self.plugin.send_main_thread(|plugin| {
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
    output_events: &EventQueue,
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
                        |x| x.to_bits() == CHECK_NAN_F64.to_bits(),
                        |x| x.to_bits() == CHECK_NAN_F32.to_bits(),
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
    for event in output_events.read() {
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
