//! Data structures and functions surrounding audio processing.

use anyhow::Result;
use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_CHOKE, CLAP_EVENT_NOTE_END,
    CLAP_EVENT_NOTE_EXPRESSION, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_MOD,
    CLAP_EVENT_PARAM_VALUE, CLAP_EVENT_TRANSPORT, CLAP_TRANSPORT_HAS_BEATS_TIMELINE,
    CLAP_TRANSPORT_HAS_SECONDS_TIMELINE, CLAP_TRANSPORT_HAS_TEMPO,
    CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_PLAYING, clap_event_header,
    clap_event_midi, clap_event_note, clap_event_note_expression, clap_event_param_mod,
    clap_event_param_value, clap_event_transport, clap_input_events, clap_output_events,
};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::process::clap_process;
use either::Either;
use parking_lot::Mutex;
use rand::Rng;
use rand_pcg::Pcg32;
use std::ffi::c_void;
use std::fmt::Debug;
use std::pin::Pin;
use std::ptr::null_mut;
use std::sync::atomic::Ordering;

use crate::plugin::ext::audio_ports::AudioPortConfig;
use crate::plugin::instance::Plugin;
use crate::plugin::instance::audio_thread::PluginAudioThread;
use crate::util::check_null_ptr;

/// The input and output data for a call to `clap_plugin::process()`.
pub struct ProcessData<'a> {
    /// The input and output audio buffers.
    pub buffers: &'a mut AudioBuffers,
    /// The input events.
    pub input_events: Pin<Box<EventQueue>>,
    /// The output events.
    pub output_events: Pin<Box<EventQueue>>,
    /// The length of the current block in samples.
    pub block_size: u32,

    config: ProcessConfig,
    /// The current transport information. This is populated when constructing this object, and the
    /// transport can be advanced `N` samples using the
    /// [`advance_transport()`][Self::advance_transport()] method.
    transport_info: clap_event_transport,
    /// The current sample position. This is used to recompute values in `transport_info`.
    sample_pos: u32,
    // TODO: Maybe do something with `steady_time`
}

/// Control flow for the processing loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessControlFlow {
    Continue,
    Reset,
    Exit,
}

/// The general context information for a process call.
#[derive(Debug, Clone, Copy)]
pub struct ProcessConfig {
    /// The current sample rate.
    pub sample_rate: f64,
    // The current tempo in beats per minute.
    pub tempo: f64,
    // The time signature's numerator.
    pub time_sig_numerator: u16,
    // The time signature's denominator.
    pub time_sig_denominator: u16,
}

/// Audio buffers for audio processing. These contain both input and output buffers, that can be either in-place
/// or out-of-place, single or double precision.
#[derive(Clone, Debug)]
pub struct AudioBuffers {
    // These are all indexed by `[port_idx][channel_idx][sample_idx]`. The inputs also need to be
    // mutable because reborrwing them from here is the only way to modify them without
    // reinitializing the pointers.
    buffers: Vec<AudioBuffer>,

    // These are point to `inputs` and `outputs` because `clap_audio_buffer` needs to contain a
    // `*const *const f32`
    _pointers: Vec<Vec<*const ()>>,

    clap_inputs: Vec<clap_audio_buffer>,
    clap_outputs: Vec<clap_audio_buffer>,

    /// The number of samples for this buffer. This is consistent across all inner vectors.
    num_samples: usize,
}

#[derive(Clone)]
pub enum AudioBuffer {
    Float32 {
        input: Option<usize>,
        output: Option<usize>,
        data: Vec<Vec<f32>>,
    },

    #[allow(unused)] //TODO: use for future 64 bit processing tests
    Float64 {
        input: Option<usize>,
        output: Option<usize>,
        data: Vec<Vec<f64>>,
    },
}

pub trait AudioBufferFill {
    fn fill_input_f32(&mut self, bus: usize, channel: usize, slice: &mut [f32]);
    fn fill_input_f64(&mut self, bus: usize, channel: usize, slice: &mut [f64]);
    fn fill_output_f32(&mut self, bus: usize, channel: usize, slice: &mut [f32]) {
        self.fill_input_f32(bus, channel, slice);
    }
    fn fill_output_f64(&mut self, bus: usize, channel: usize, slice: &mut [f64]) {
        self.fill_input_f64(bus, channel, slice);
    }
    fn fill_inplace_f32(&mut self, input: usize, output: usize, channel: usize, slice: &mut [f32]) {
        let _ = output;
        self.fill_input_f32(input, channel, slice);
    }
    fn fill_inplace_f64(&mut self, input: usize, output: usize, channel: usize, slice: &mut [f64]) {
        let _ = output;
        self.fill_input_f64(input, channel, slice);
    }
}

// SAFETY: Sharing these pointers with other threads is safe as they refer to the borrowed input and
//         output slices. The pointers thus cannot be invalidated.
unsafe impl Send for AudioBuffers {}
unsafe impl Sync for AudioBuffers {}

/// An event queue that can be used as either an input queue or an output queue. This is always
/// allocated through a `Pin<Box<EventQueue>>` so the pointers are stable. The `VTable` type
/// argument should be either `clap_input_events` or `clap_output_events`.
#[derive(Debug)]
pub struct EventQueue {
    vtable_input: clap_input_events,
    vtable_output: clap_output_events,
    /// The actual event queue. Since we're going for correctness over performance, this uses a very
    /// suboptimal memory layout by just using an `enum` instead of doing fancy bit packing.
    events: Mutex<Vec<Event>>,
}

/// An event sent to or from the plugin. This uses an enum to make the implementation simple and
/// correct at the cost of more wasteful memory usage.
#[derive(Debug, Clone)]
#[repr(C, align(8))]
pub enum Event {
    /// `CLAP_EVENT_NOTE_ON`, `CLAP_EVENT_NOTE_OFF`, `CLAP_EVENT_NOTE_CHOKE`, or `CLAP_EVENT_NOTE_END`.
    Note(clap_event_note),
    /// `CLAP_EVENT_NOTE_EXPRESSION`.
    NoteExpression(clap_event_note_expression),
    /// `CLAP_EVENT_MIDI`.
    Midi(clap_event_midi),
    /// `CLAP_EVENT_PARAM_VALUE`.
    ParamValue(clap_event_param_value),
    /// `CLAP_EVENT_PARAM_MOD`.
    ParamMod(clap_event_param_mod),
    /// An unhandled event type. This is only used when the plugin outputs an event we don't handle
    /// or recognize.
    Unknown(clap_event_header),
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            sample_rate: 44_100.0,
            tempo: 110.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
        }
    }
}

impl<'a> ProcessData<'a> {
    /// Initialize the process data using the given audio buffers. The transport information will be
    /// initialized at the start of the project, and it can be moved using the
    /// [`advance_transport()`][Self::advance_transport()] method.
    //
    // TODO: More transport info options. Missing fields, loop regions, flags, etc.
    pub fn new(buffers: &'a mut AudioBuffers, config: ProcessConfig) -> Self {
        ProcessData {
            input_events: EventQueue::new(),
            output_events: EventQueue::new(),
            block_size: buffers.len() as u32,
            buffers,

            config,
            transport_info: clap_event_transport {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_transport>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_TRANSPORT,
                    flags: 0,
                },
                flags: CLAP_TRANSPORT_HAS_TEMPO
                    | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                    | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                    | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                    | CLAP_TRANSPORT_IS_PLAYING,
                song_pos_beats: 0,
                song_pos_seconds: 0,
                tempo: config.tempo,
                tempo_inc: 0.0,
                // These four currently aren't used
                loop_start_beats: 0,
                loop_end_beats: 0,
                loop_start_seconds: 0,
                loop_end_seconds: 0,
                bar_start: 0,
                bar_number: 0,
                tsig_num: config.time_sig_numerator,
                tsig_denom: config.time_sig_denominator,
            },
            sample_pos: 0,
        }
    }

    /// Construct the CLAP process data, and evaluate a closure with it. The `clap_process_data`
    /// contains raw pointers to this struct's data, so the closure is there to prevent dangling
    /// pointers.
    pub fn with_clap_process_data<T, F: FnOnce(clap_process) -> T>(&mut self, f: F) -> T {
        assert!(
            self.block_size as usize <= self.buffers.len(),
            "Process block size is larger than the maximum allowed buffer size. This is a \
             clap-validator bug."
        );

        let (inputs, outputs) = self.buffers.clap_buffers();

        let process_data = clap_process {
            steady_time: self.sample_pos as i64,
            frames_count: self.block_size,
            transport: &self.transport_info,
            audio_inputs: if inputs.is_empty() {
                std::ptr::null()
            } else {
                inputs.as_ptr()
            },
            audio_outputs: if outputs.is_empty() {
                std::ptr::null_mut()
            } else {
                outputs.as_mut_ptr()
            },
            audio_inputs_count: inputs.len() as u32,
            audio_outputs_count: outputs.len() as u32,
            in_events: self.input_events.vtable_input(),
            out_events: self.output_events.vtable_output(),
        };

        f(process_data)
    }

    /// Get current the transport information.
    #[allow(unused)]
    pub fn transport_info(&self) -> clap_event_transport {
        self.transport_info
    }

    /// Advance the transport by a certain number of samples.
    pub fn advance_next(&mut self) {
        self.input_events.clear();
        self.output_events.clear();

        self.sample_pos += self.block_size;
        self.transport_info.song_pos_beats =
            ((self.sample_pos as f64 / self.config.sample_rate / 60.0 * self.transport_info.tempo)
                * CLAP_BEATTIME_FACTOR as f64)
                .round() as i64;
        self.transport_info.song_pos_seconds = ((self.sample_pos as f64 / self.config.sample_rate)
            * CLAP_SECTIME_FACTOR as f64)
            .round() as i64;
    }

    pub fn reset(&mut self) {
        self.sample_pos = 0;
        self.transport_info.song_pos_beats = 0;
        self.transport_info.song_pos_seconds = 0;
        self.input_events.clear();
        self.output_events.clear();
    }

    pub fn run<Process>(&mut self, plugin: &Plugin, mut process: Process) -> Result<()>
    where
        Process: FnMut(&PluginAudioThread, &mut Self) -> Result<ProcessControlFlow> + Send,
    {
        let mut running = true;
        while running {
            plugin.activate(self.config.sample_rate, 1, self.buffers.len())?;
            plugin.host().handle_callbacks_once();
            self.reset();

            plugin.on_audio_thread(|plugin| -> Result<()> {
                plugin.start_processing()?;

                // This test can be repeated a couple of times
                // NOTE: We intentionally do not disable denormals here
                'processing: while running {
                    let flow = process(&plugin, self)?;
                    running &= flow != ProcessControlFlow::Exit;
                    self.advance_next();

                    // Restart processing as necessary
                    if plugin
                        .state()
                        .requested_restart
                        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        log::trace!(
                            "Restarting the plugin during processing cycle after a call to \
                             'clap_host::request_restart()'",
                        );
                        break 'processing;
                    }

                    if flow == ProcessControlFlow::Reset {
                        break 'processing;
                    }
                }

                plugin.stop_processing();

                Ok(())
            })?;

            plugin.deactivate();
        }

        // Handle callbacks the plugin may have made during deactivate
        plugin.host().handle_callbacks_once();

        Ok(())
    }

    pub fn run_once<Process>(&mut self, plugin: &Plugin, process: Process) -> Result<()>
    where
        Process: FnOnce(&PluginAudioThread, &mut Self) -> Result<()> + Send,
    {
        let mut process = Some(process);
        self.run(plugin, |plugin, instance| {
            if let Some(process) = process.take() {
                process(plugin, instance)?;
            }

            Ok(ProcessControlFlow::Exit)
        })
    }
}

impl AudioBuffers {
    /// Construct the audio buffers from the given buffer configurations. The number of samples must
    /// be greater than zero and all channel vectors must have the same length.
    pub fn new(buffers: Vec<AudioBuffer>, num_samples: usize) -> Self {
        assert!(
            num_samples > 0,
            "Number of samples must be greater than zero."
        );

        let mut pointers = vec![];
        let mut clap_inputs = vec![];
        let mut clap_outputs = vec![];

        for buffer in buffers.iter() {
            let pointer_list = match buffer {
                AudioBuffer::Float32 { data, .. } => {
                    assert!(
                        data.iter().all(|x| x.len() == num_samples),
                        "Channel buffer length does not match"
                    );

                    data.iter()
                        .map(|x| x.as_ptr() as *const ())
                        .collect::<Vec<_>>()
                }
                AudioBuffer::Float64 { data, .. } => {
                    assert!(
                        data.iter().all(|x| x.len() == num_samples),
                        "Channel buffer length does not match"
                    );

                    data.iter()
                        .map(|x| x.as_ptr() as *const ())
                        .collect::<Vec<_>>()
                }
            };

            if let Some(input) = buffer.input() {
                if clap_inputs.len() <= input {
                    clap_inputs.resize(input + 1, None);
                }

                clap_inputs[input] = Some(clap_audio_buffer {
                    data32: if buffer.is_64bit() {
                        null_mut()
                    } else {
                        pointer_list.as_ptr() as *mut *mut f32
                    },

                    data64: if buffer.is_64bit() {
                        pointer_list.as_ptr() as *mut *mut f64
                    } else {
                        null_mut()
                    },

                    channel_count: pointer_list.len() as u32,
                    latency: 0, //TODO: do some interesting tests with these 2 fields
                    constant_mask: 0,
                });
            }

            if let Some(output) = buffer.output() {
                if clap_outputs.len() <= output {
                    clap_outputs.resize(output + 1, None);
                }

                clap_outputs[output] = Some(clap_audio_buffer {
                    data32: if buffer.is_64bit() {
                        null_mut()
                    } else {
                        pointer_list.as_ptr() as *mut *mut f32
                    },

                    data64: if buffer.is_64bit() {
                        pointer_list.as_ptr() as *mut *mut f64
                    } else {
                        null_mut()
                    },

                    channel_count: pointer_list.len() as u32,
                    latency: 0, //TODO: do some interesting tests with these 2 fields
                    constant_mask: 0,
                });
            }

            pointers.push(pointer_list);
        }

        Self {
            buffers,
            _pointers: pointers,
            clap_inputs: clap_inputs
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .expect("Missing an input bus"),
            clap_outputs: clap_outputs
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .expect("Missing an output bus"),
            num_samples,
        }
    }

    /// Construct the out of place audio buffers. This allocates the channel pointers that are
    /// handed to the plugin in the process function.
    pub fn new_out_of_place_f32(config: &AudioPortConfig, num_samples: usize) -> Self {
        Self::new(
            config
                .inputs
                .iter()
                .enumerate()
                .map(|(index, port)| AudioBuffer::Float32 {
                    input: Some(index),
                    output: None,
                    data: vec![vec![0.0f32; num_samples]; port.num_channels as usize],
                })
                .chain(config.outputs.iter().enumerate().map(|(index, port)| {
                    AudioBuffer::Float32 {
                        input: None,
                        output: Some(index),
                        data: vec![vec![0.0f32; num_samples]; port.num_channels as usize],
                    }
                }))
                .collect(),
            num_samples,
        )
    }

    /// Construct the in place audio buffers. This allocates the channel pointers that are handed to
    /// the plugin in the process function.
    pub fn new_in_place_f32(config: &AudioPortConfig, num_samples: usize) -> Self {
        let mut buffers = vec![];

        for (index, port) in config.inputs.iter().enumerate() {
            let in_place = port
                .in_place_pair_idx
                .filter(|output| config.outputs[*output].num_channels == port.num_channels);

            if in_place.is_none() {
                buffers.push(AudioBuffer::Float32 {
                    input: Some(index),
                    output: None,
                    data: vec![vec![0.0f32; num_samples]; port.num_channels as usize],
                });
            }
        }

        for (index, port) in config.outputs.iter().enumerate() {
            let in_place = port
                .in_place_pair_idx
                .filter(|input| config.inputs[*input].num_channels == port.num_channels);

            buffers.push(AudioBuffer::Float32 {
                input: in_place,
                output: Some(index),
                data: vec![vec![0.0f32; num_samples]; port.num_channels as usize],
            });
        }

        Self::new(buffers, num_samples)
    }

    /// The number of samples in the buffer.
    pub fn len(&self) -> usize {
        self.num_samples
    }

    /// Pointers for the inputs and the outputs. These can be used to construct the `clap_process`
    /// data.
    pub fn clap_buffers(&mut self) -> (&[clap_audio_buffer], &mut [clap_audio_buffer]) {
        (&self.clap_inputs, &mut self.clap_outputs)
    }

    /// Pointers to the internal audio buffers
    pub fn buffers(&self) -> &[AudioBuffer] {
        &self.buffers
    }

    /// Check whether the audio buffers are identical to another set of audio buffers.
    pub fn is_same(&self, other: &Self) -> bool {
        if self.buffers.len() != other.buffers.len() {
            return false;
        }

        for (this, other) in self.buffers.iter().zip(other.buffers.iter()) {
            if !this.is_same(other) {
                return false;
            }
        }

        true
    }

    /// Fill the input and output buffers with arbitrary values.
    pub fn fill(&mut self, mut fill: impl AudioBufferFill) {
        for bus in &mut self.buffers {
            match bus {
                AudioBuffer::Float32 {
                    input,
                    output,
                    data,
                } => {
                    for (channel_idx, channel) in data.iter_mut().enumerate() {
                        match (*input, *output) {
                            (Some(input), Some(output)) => {
                                fill.fill_inplace_f32(input, output, channel_idx, channel);
                            }
                            (Some(input), None) => {
                                fill.fill_input_f32(input, channel_idx, channel);
                            }
                            (None, Some(output)) => {
                                fill.fill_output_f32(output, channel_idx, channel);
                            }
                            (None, None) => {}
                        }
                    }
                }
                AudioBuffer::Float64 {
                    input,
                    output,
                    data,
                } => {
                    for (channel_idx, channel) in data.iter_mut().enumerate() {
                        match (*input, *output) {
                            (Some(input), Some(output)) => {
                                fill.fill_inplace_f64(input, output, channel_idx, channel);
                            }
                            (Some(input), None) => {
                                fill.fill_input_f64(input, channel_idx, channel);
                            }
                            (None, Some(output)) => {
                                fill.fill_output_f64(output, channel_idx, channel);
                            }
                            (None, None) => {}
                        }
                    }
                }
            }
        }

        for input in &mut self.clap_inputs {
            input.constant_mask = 0;
        }

        for output in &mut self.clap_outputs {
            output.constant_mask = 0;
        }
    }

    /// Fill the input buffers with white noise ([-1, 1], denormals are snapped to zero).
    /// Output buffers are filled with random NaN values to detect if they have been written to.
    pub fn randomize(&mut self, prng: &mut Pcg32) {
        struct Randomize<'a>(&'a mut Pcg32);

        impl AudioBufferFill for Randomize<'_> {
            fn fill_input_f32(&mut self, _bus: usize, _channel: usize, slice: &mut [f32]) {
                for sample in slice.iter_mut() {
                    let y = self.0.random_range(-1.0..=1.0f32);
                    *sample = if y.is_subnormal() { 0.0 } else { y };
                }
            }

            fn fill_input_f64(&mut self, _bus: usize, _channel: usize, slice: &mut [f64]) {
                for sample in slice.iter_mut() {
                    let y = self.0.random_range(-1.0..=1.0f64);
                    *sample = if y.is_subnormal() { 0.0 } else { y };
                }
            }

            // fill with random NaN values so we can detect if a plugin left the output uninitialized
            fn fill_output_f32(&mut self, _bus: usize, _channel: usize, slice: &mut [f32]) {
                for sample in slice.iter_mut() {
                    let y: u32 = self.0.random();
                    let y = f32::from_bits(y | 0x7F800001);
                    assert!(y.is_nan());
                    *sample = y;
                }
            }

            fn fill_output_f64(&mut self, _bus: usize, _channel: usize, slice: &mut [f64]) {
                for sample in slice.iter_mut() {
                    let y: u64 = self.0.random();
                    let y = f64::from_bits(y | 0x7FF0000000000001);
                    assert!(y.is_nan());
                    *sample = y;
                }
            }
        }

        self.fill(Randomize(prng));
    }

    pub fn silence_all_inputs(&mut self) {
        struct Silence;

        impl AudioBufferFill for Silence {
            fn fill_input_f32(&mut self, _bus: usize, _channel: usize, slice: &mut [f32]) {
                slice.fill(0.0);
            }

            fn fill_input_f64(&mut self, _bus: usize, _channel: usize, slice: &mut [f64]) {
                slice.fill(0.0);
            }

            fn fill_output_f32(&mut self, _bus: usize, _channel: usize, _slice: &mut [f32]) {}
            fn fill_output_f64(&mut self, _bus: usize, _channel: usize, _slice: &mut [f64]) {}
        }

        self.fill(Silence);

        for input in &mut self.clap_inputs {
            input.constant_mask = 1u64.unbounded_shl(input.channel_count).wrapping_sub(1);
        }
    }

    pub fn output_constant_mask(&self, bus: usize) -> u64 {
        self.clap_outputs[bus].constant_mask
    }
}

impl AudioBuffer {
    /// Get the index of the input bus for this buffer.
    pub fn input(&self) -> Option<usize> {
        match self {
            AudioBuffer::Float32 { input, .. } => *input,
            AudioBuffer::Float64 { input, .. } => *input,
        }
    }

    /// Get the index of the output bus for this buffer.
    pub fn output(&self) -> Option<usize> {
        match self {
            AudioBuffer::Float32 { output, .. } => *output,
            AudioBuffer::Float64 { output, .. } => *output,
        }
    }

    /// Check whether this is a double precision buffer.
    pub fn is_64bit(&self) -> bool {
        match self {
            AudioBuffer::Float32 { .. } => false,
            AudioBuffer::Float64 { .. } => true,
        }
    }

    pub fn is_same(&self, other: &Self) -> bool {
        match (self, other) {
            (AudioBuffer::Float32 { data: this, .. }, AudioBuffer::Float32 { data: other, .. }) => {
                for (this, other) in this.iter().zip(other.iter()) {
                    for (this, other) in this.iter().zip(other.iter()) {
                        if this.to_bits() != other.to_bits() {
                            return false;
                        }
                    }
                }

                true
            }

            (AudioBuffer::Float64 { data: this, .. }, AudioBuffer::Float64 { data: other, .. }) => {
                for (this, other) in this.iter().zip(other.iter()) {
                    for (this, other) in this.iter().zip(other.iter()) {
                        if this.to_bits() != other.to_bits() {
                            return false;
                        }
                    }
                }

                true
            }

            _ => false,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            AudioBuffer::Float32 { data, .. } => data.first().map_or(0, |x| x.len()),
            AudioBuffer::Float64 { data, .. } => data.first().map_or(0, |x| x.len()),
        }
    }

    pub fn channels(&self) -> usize {
        match self {
            AudioBuffer::Float32 { data, .. } => data.len(),
            AudioBuffer::Float64 { data, .. } => data.len(),
        }
    }

    pub fn get(&self, channel: usize, sample: usize) -> Either<f64, f32> {
        match self {
            AudioBuffer::Float32 { data, .. } => Either::Right(data[channel][sample]),
            AudioBuffer::Float64 { data, .. } => Either::Left(data[channel][sample]),
        }
    }
}

impl Debug for AudioBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Float32 { input, output, .. } => f
                .debug_struct("Float32")
                .field("input", input)
                .field("output", output)
                .finish_non_exhaustive(),
            Self::Float64 { input, output, .. } => f
                .debug_struct("Float64")
                .field("input", input)
                .field("output", output)
                .finish_non_exhaustive(),
        }
    }
}

impl EventQueue {
    /// Construct a new event queue. This can be used as both an input and an output queue.
    pub fn new() -> Pin<Box<Self>> {
        let mut queue = Box::pin(Self {
            vtable_input: clap_input_events {
                // This is set to point to this object below
                ctx: std::ptr::null_mut(),
                size: Some(Self::size),
                get: Some(Self::get),
            },

            vtable_output: clap_output_events {
                // This is set to point to this object below
                ctx: std::ptr::null_mut(),
                try_push: Some(Self::try_push),
            },

            // Using a mutex here is obviously a terrible idea in a real host, but we're not a real
            // host
            events: Mutex::new(Vec::new()),
        });

        queue.vtable_input.ctx = &*queue as *const Self as *mut c_void;
        queue.vtable_output.ctx = &*queue as *const Self as *mut c_void;
        queue
    }

    pub fn clear(&self) {
        self.events.lock().clear();
    }

    pub fn add_events(&self, extend: impl IntoIterator<Item = Event>) {
        let mut events = self.events.lock();
        let should_sort = !events.is_empty();
        events.extend(extend);
        if should_sort {
            events.sort_by_key(|event| event.header().time);
        }
    }

    pub fn read(&self) -> Vec<Event> {
        self.events.lock().clone()
    }

    /// Get the vtable pointer for input events.
    pub fn vtable_input(self: &Pin<Box<Self>>) -> *const clap_input_events {
        &self.vtable_input
    }

    /// Get the vtable pointer for output events.
    pub fn vtable_output(self: &Pin<Box<Self>>) -> *const clap_output_events {
        &self.vtable_output
    }

    unsafe extern "C" fn size(list: *const clap_input_events) -> u32 {
        unsafe {
            check_null_ptr!(list, (*list).ctx);
            let this = &*((*list).ctx as *const Self);
            this.events.lock().len() as u32
        }
    }

    unsafe extern "C" fn get(
        list: *const clap_input_events,
        index: u32,
    ) -> *const clap_event_header {
        unsafe {
            check_null_ptr!(list, (*list).ctx);
            let this = &*((*list).ctx as *const Self);

            let events = this.events.lock();
            match events.get(index as usize) {
                Some(event) => event.header(),
                None => {
                    log::warn!(
                        "The plugin tried to get an event with index {index} ({} total events)",
                        events.len()
                    );
                    std::ptr::null()
                }
            }
        }
    }

    unsafe extern "C" fn try_push(
        list: *const clap_output_events,
        event: *const clap_event_header,
    ) -> bool {
        unsafe {
            check_null_ptr!(list, (*list).ctx, event);
            let this = &*((*list).ctx as *const Self);

            // The monotonicity of the plugin's event insertion order is checked as part of the output
            // consistency checks
            this.events
                .lock()
                .push(Event::from_header_ptr(event).unwrap());

            true
        }
    }
}

impl Event {
    /// Parse an event from a plugin-provided pointer. Returns an error if the pointer as a null pointer
    pub unsafe fn from_header_ptr(ptr: *const clap_event_header) -> Result<Self> {
        if ptr.is_null() {
            anyhow::bail!("Null pointer provided for 'clap_event_header'.");
        }

        unsafe {
            match ((*ptr).space_id, ((*ptr).type_)) {
                (
                    CLAP_CORE_EVENT_SPACE_ID,
                    CLAP_EVENT_NOTE_ON
                    | CLAP_EVENT_NOTE_OFF
                    | CLAP_EVENT_NOTE_CHOKE
                    | CLAP_EVENT_NOTE_END,
                ) => Ok(Event::Note(*(ptr as *const clap_event_note))),
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_EXPRESSION) => Ok(
                    Event::NoteExpression(*(ptr as *const clap_event_note_expression)),
                ),
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE) => {
                    Ok(Event::ParamValue(*(ptr as *const clap_event_param_value)))
                }
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_MOD) => {
                    Ok(Event::ParamMod(*(ptr as *const clap_event_param_mod)))
                }
                (CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI) => {
                    Ok(Event::Midi(*(ptr as *const clap_event_midi)))
                }
                (_, _) => Ok(Event::Unknown(*ptr)),
            }
        }
    }

    /// Get a a reference to the event's header.
    pub fn header(&self) -> &clap_event_header {
        match self {
            Event::Note(event) => &event.header,
            Event::NoteExpression(event) => &event.header,
            Event::ParamValue(event) => &event.header,
            Event::ParamMod(event) => &event.header,
            Event::Midi(event) => &event.header,
            Event::Unknown(header) => header,
        }
    }
}
