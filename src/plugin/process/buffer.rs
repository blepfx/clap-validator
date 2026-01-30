use crate::plugin::{ext::audio_ports::AudioPortConfig, process::ConstantMask};
use clap_sys::audio_buffer::*;
use either::Either;
use rand::Rng;
use rand_pcg::Pcg32;
use std::ptr::null_mut;

/// Audio buffers for audio processing. These contain both input and output buffers, that can be either in-place
/// or out-of-place, single or double precision.
#[derive(Clone)]
pub struct AudioBuffers {
    /// These are all indexed by `[port_idx][channel_idx][sample_idx]`. The inputs also need to be
    /// mutable because reborrwing them from here is the only way to modify them without
    /// reinitializing the pointers.
    buffers: Box<[AudioBuffer]>,

    /// These point to `inputs` and `outputs` because `clap_audio_buffer` needs to contain a
    /// `*const *const f32`
    _pointers: Box<[Box<[*const ()]>]>,

    /// The CLAP audio buffer representations for inputs and outputs.
    clap_inputs: Box<[clap_audio_buffer]>,
    clap_outputs: Box<[clap_audio_buffer]>,

    /// The number of samples for this buffer. This is consistent across all inner vectors.
    num_samples: u32,
}

/// A single audio buffer, either input, output or in-place. This can be either single or double precision.
#[derive(Clone)]
pub enum AudioBuffer {
    Float32 {
        port: AudioBufferPort,
        data: Box<[Box<[f32]>]>,
    },

    Float64 {
        port: AudioBufferPort,
        data: Box<[Box<[f64]>]>,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum AudioBufferPort {
    Input(usize),
    Output(usize),
    Inplace(usize, usize),
}

// SAFETY: Sharing these pointers with other threads is safe as they refer to the borrowed input and
//         output slices. The pointers thus cannot be invalidated.
unsafe impl Send for AudioBuffers {}
unsafe impl Sync for AudioBuffers {}

impl AudioBuffers {
    /// Construct the audio buffers from the given buffer configurations. The number of samples must
    /// be greater than zero and all channel vectors must have the same length.
    pub fn new(buffers: Vec<AudioBuffer>, num_samples: u32) -> Self {
        assert!(num_samples > 0, "Number of samples must be greater than zero.");

        let mut pointers = vec![];
        let mut clap_inputs = vec![];
        let mut clap_outputs = vec![];

        for buffer in buffers.iter() {
            let pointer_list = match buffer {
                AudioBuffer::Float32 { data, .. } => {
                    assert!(
                        data.iter().all(|x| x.len() as u32 == num_samples),
                        "Channel buffer length does not match"
                    );

                    data.iter().map(|x| x.as_ptr() as *const ()).collect::<Vec<_>>()
                }
                AudioBuffer::Float64 { data, .. } => {
                    assert!(
                        data.iter().all(|x| x.len() as u32 == num_samples),
                        "Channel buffer length does not match"
                    );

                    data.iter().map(|x| x.as_ptr() as *const ()).collect::<Vec<_>>()
                }
            };

            if let Some(input) = buffer.port().as_input() {
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

            if let Some(output) = buffer.port().as_output() {
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

            pointers.push(pointer_list.into_boxed_slice());
        }

        Self {
            buffers: buffers.into_boxed_slice(),
            _pointers: pointers.into_boxed_slice(),
            clap_inputs: clap_inputs
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .expect("Missing an input bus")
                .into_boxed_slice(),
            clap_outputs: clap_outputs
                .into_iter()
                .collect::<Option<Vec<_>>>()
                .expect("Missing an output bus")
                .into_boxed_slice(),
            num_samples,
        }
    }

    pub fn new_out_of_place_f32(config: &AudioPortConfig, num_samples: u32) -> Self {
        Self::new(
            config
                .inputs
                .iter()
                .enumerate()
                .map(|(index, port)| {
                    AudioBuffer::new(AudioBufferPort::Input(index), false, port.num_channels, num_samples)
                })
                .chain(config.outputs.iter().enumerate().map(|(index, port)| {
                    AudioBuffer::new(AudioBufferPort::Output(index), false, port.num_channels, num_samples)
                }))
                .collect(),
            num_samples,
        )
    }

    pub fn new_in_place_f32(config: &AudioPortConfig, num_samples: u32) -> Self {
        let mut buffers = vec![];

        for (index, port) in config.inputs.iter().enumerate() {
            let in_place = port
                .in_place_pair_idx
                .filter(|output| config.outputs[*output].num_channels == port.num_channels);

            if in_place.is_none() {
                buffers.push(AudioBuffer::new(
                    AudioBufferPort::Input(index),
                    false,
                    port.num_channels,
                    num_samples,
                ));
            }
        }

        for (index, port) in config.outputs.iter().enumerate() {
            let in_place = port
                .in_place_pair_idx
                .filter(|input| config.inputs[*input].num_channels == port.num_channels);

            buffers.push(AudioBuffer::new(
                match in_place {
                    Some(input) => AudioBufferPort::Inplace(input, index),
                    None => AudioBufferPort::Output(index),
                },
                false,
                port.num_channels,
                num_samples,
            ));
        }

        Self::new(buffers, num_samples)
    }

    pub fn new_out_of_place_f64(config: &AudioPortConfig, num_samples: u32) -> Self {
        Self::new(
            config
                .inputs
                .iter()
                .enumerate()
                .map(|(index, port)| {
                    AudioBuffer::new(
                        AudioBufferPort::Input(index),
                        port.supports_double_sample_size,
                        port.num_channels,
                        num_samples,
                    )
                })
                .chain(config.outputs.iter().enumerate().map(|(index, port)| {
                    AudioBuffer::new(
                        AudioBufferPort::Output(index),
                        port.supports_double_sample_size,
                        port.num_channels,
                        num_samples,
                    )
                }))
                .collect(),
            num_samples,
        )
    }

    pub fn new_in_place_f64(config: &AudioPortConfig, num_samples: u32) -> Self {
        let mut buffers = vec![];

        for (index, port) in config.inputs.iter().enumerate() {
            let in_place = port
                .in_place_pair_idx
                .filter(|output| config.outputs[*output].num_channels == port.num_channels);

            if in_place.is_none() {
                buffers.push(AudioBuffer::new(
                    AudioBufferPort::Input(index),
                    port.supports_double_sample_size,
                    port.num_channels,
                    num_samples,
                ));
            }
        }

        for (index, port) in config.outputs.iter().enumerate() {
            let in_place = port.in_place_pair_idx.filter(|input| {
                let input = &config.inputs[*input];
                port.num_channels == input.num_channels
                    && port.supports_double_sample_size == input.supports_double_sample_size
            });

            buffers.push(AudioBuffer::new(
                match in_place {
                    Some(input) => AudioBufferPort::Inplace(input, index),
                    None => AudioBufferPort::Output(index),
                },
                port.supports_double_sample_size,
                port.num_channels,
                num_samples,
            ));
        }

        Self::new(buffers, num_samples)
    }

    /// The number of samples in the buffer.
    pub fn len(&self) -> u32 {
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

    /// Pointers to the internal audio buffers
    pub fn buffers_mut(&mut self) -> &mut [AudioBuffer] {
        &mut self.buffers
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

    /// Fill the input buffers with white noise ([-1, 1], denormals are snapped to zero).
    pub fn randomize(&mut self, prng: &mut Pcg32) {
        for buffer in self.buffers_mut() {
            if buffer.is_input() {
                buffer.fill_white_noise(prng);
            }
        }

        for input in &mut self.clap_inputs {
            input.constant_mask = 0;
        }
    }

    /// Fill the input buffers with silence (zeros), and mark all input channels as constant.
    pub fn silence_inputs(&mut self) {
        for buffer in self.buffers_mut() {
            if buffer.is_input() {
                buffer.fill(0.0, 0.0);
            }
        }

        for input in &mut self.clap_inputs {
            input.constant_mask = u64::MAX;
        }
    }

    /// Get the constant mask for the given output bus.
    pub fn get_output_constant_mask(&self, bus: usize) -> ConstantMask {
        ConstantMask(self.clap_outputs[bus].constant_mask)
    }
}

impl AudioBufferPort {
    pub fn as_input(&self) -> Option<usize> {
        match self {
            AudioBufferPort::Input(index) => Some(*index),
            AudioBufferPort::Inplace(index, _) => Some(*index),
            AudioBufferPort::Output(_) => None,
        }
    }

    pub fn as_output(&self) -> Option<usize> {
        match self {
            AudioBufferPort::Output(index) => Some(*index),
            AudioBufferPort::Inplace(_, index) => Some(*index),
            AudioBufferPort::Input(_) => None,
        }
    }
}

impl AudioBuffer {
    pub fn new(port: AudioBufferPort, is_double_precision: bool, num_channels: u32, num_samples: u32) -> Self {
        if is_double_precision {
            AudioBuffer::Float64 {
                port,
                data: vec![vec![0.0f64; num_samples as usize].into_boxed_slice(); num_channels as usize]
                    .into_boxed_slice(),
            }
        } else {
            AudioBuffer::Float32 {
                port,
                data: vec![vec![0.0f32; num_samples as usize].into_boxed_slice(); num_channels as usize]
                    .into_boxed_slice(),
            }
        }
    }

    pub fn port(&self) -> AudioBufferPort {
        match self {
            AudioBuffer::Float32 { port, .. } => *port,
            AudioBuffer::Float64 { port, .. } => *port,
        }
    }

    /// Check whether this is a double precision buffer.
    pub fn is_64bit(&self) -> bool {
        match self {
            AudioBuffer::Float32 { .. } => false,
            AudioBuffer::Float64 { .. } => true,
        }
    }

    pub fn is_input(&self) -> bool {
        self.port().as_input().is_some()
    }

    pub fn is_output_only(&self) -> bool {
        self.port().as_output().is_some() && self.port().as_input().is_none()
    }

    /// Check whether this audio buffer's contents are identical to another audio buffer.
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

    /// The number of samples in this buffer.
    pub fn len(&self) -> u32 {
        match self {
            AudioBuffer::Float32 { data, .. } => data.first().map_or(0, |x| x.len() as u32),
            AudioBuffer::Float64 { data, .. } => data.first().map_or(0, |x| x.len() as u32),
        }
    }

    /// The number of channels in this buffer.
    pub fn channels(&self) -> u32 {
        match self {
            AudioBuffer::Float32 { data, .. } => data.len() as u32,
            AudioBuffer::Float64 { data, .. } => data.len() as u32,
        }
    }

    /// Get a sample from the buffer.
    pub fn get(&self, channel: u32, sample: u32) -> Either<f64, f32> {
        match self {
            AudioBuffer::Float32 { data, .. } => Either::Right(data[channel as usize][sample as usize]),
            AudioBuffer::Float64 { data, .. } => Either::Left(data[channel as usize][sample as usize]),
        }
    }

    /// Fill the buffer with silence (zeros).
    pub fn fill(&mut self, value_f32: f32, value_f64: f64) {
        match self {
            AudioBuffer::Float32 { data, .. } => {
                for channel in data {
                    for sample in channel {
                        *sample = value_f32;
                    }
                }
            }
            AudioBuffer::Float64 { data, .. } => {
                for channel in data {
                    for sample in channel {
                        *sample = value_f64;
                    }
                }
            }
        }
    }

    /// Fill the buffer with white noise (random values in the range [-1, 1]).
    pub fn fill_white_noise(&mut self, prng: &mut Pcg32) {
        match self {
            AudioBuffer::Float32 { data, .. } => {
                for channel in data {
                    for sample in channel {
                        *sample = prng.random_range(-1.0..=1.0f32);
                    }
                }
            }
            AudioBuffer::Float64 { data, .. } => {
                for channel in data {
                    for sample in channel {
                        *sample = prng.random_range(-1.0..=1.0f64);
                    }
                }
            }
        }
    }
}
