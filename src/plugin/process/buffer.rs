use crate::plugin::ext::audio_ports::AudioPortConfig;
use crate::plugin::process::ConstantMask;
use anyhow::Result;
use clap_sys::audio_buffer::*;
use either::Either;
use rand::RngExt;
use rand_pcg::Pcg32;
use std::collections::HashMap;
use std::fmt::Debug;
use std::ops::{Deref, DerefMut};
use std::ptr::null_mut;

/// Audio buffers for audio processing. These contain both input and output buffers, that can be either in-place
/// or out-of-place, single or double precision.
#[derive(Clone)]
pub struct AudioBuffers {
    /// These are all indexed by `[port_idx][channel_idx][sample_idx]`. The inputs also need to be
    /// mutable because reborrwing them from here is the only way to modify them without
    /// reinitializing the pointers.
    buffers: Box<[AudioBuffer]>,

    /// The CLAP audio buffer representations for inputs
    clap_inputs: Box<[clap_audio_buffer]>,
    /// The CLAP audio buffer representations for outputs
    clap_outputs: Box<[clap_audio_buffer]>,

    /// The number of samples for this buffer. This is consistent across all inner vectors.
    samples: u32,
}

#[derive(Debug, Clone)]
pub struct AudioBuffer {
    port: AudioBufferPort,
    data: AudioBufferData,

    input_constant_mask: ConstantMask,
    output_constant_mask: ConstantMask,

    input_latency: u32,
    output_latency: u32,
}

pub struct AudioBufferData {
    #[allow(clippy::type_complexity)]
    data: Either<Box<[Box<[f32]>]>, Box<[Box<[f64]>]>>,
    pointers: Box<[*const ()]>,
    samples: u32,
}

/// A port to which an audio buffer belongs.
#[derive(Clone, Copy, Debug)]
pub enum AudioBufferPort {
    Input(usize),
    Output(usize),
    Inplace(usize, usize),
}

impl AudioBuffers {
    /// Construct the audio buffers from the given buffer configurations. The number of samples must
    /// be greater than zero and all channel vectors must have the same length.
    pub fn new(buffers: Vec<AudioBuffer>, samples: u32) -> Self {
        let mut clap_inputs = vec![];
        let mut clap_outputs = vec![];

        for buffer in buffers.iter() {
            assert!(
                buffer.samples() == samples,
                "All audio buffers must have the same number of samples."
            );

            if let Some(input) = buffer.port().input() {
                if clap_inputs.len() <= input {
                    clap_inputs.resize(input + 1, None);
                }

                clap_inputs[input] = Some(clap_audio_buffer {
                    data32: buffer.as_ptr().either(|x| x, |_| null_mut()),
                    data64: buffer.as_ptr().either(|_| null_mut(), |x| x),
                    channel_count: buffer.channels(),
                    latency: 0,
                    constant_mask: 0,
                });
            }

            if let Some(output) = buffer.port().output() {
                if clap_outputs.len() <= output {
                    clap_outputs.resize(output + 1, None);
                }

                clap_outputs[output] = Some(clap_audio_buffer {
                    data32: buffer.as_ptr().either(|x| x, |_| null_mut()),
                    data64: buffer.as_ptr().either(|_| null_mut(), |x| x),
                    channel_count: buffer.channels(),
                    latency: 0,
                    constant_mask: 0,
                });
            }
        }

        Self {
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
            buffers: buffers.into_boxed_slice(),
            samples,
        }
    }

    pub fn new_out_of_place_f32(config: &AudioPortConfig, samples: u32) -> Self {
        Self::new(
            (0..config.inputs.len())
                .map(AudioBufferPort::Input)
                .chain((0..config.outputs.len()).map(AudioBufferPort::Output))
                .map(|port| port.create_buffer(config, samples, false))
                .collect(),
            samples,
        )
    }

    pub fn new_out_of_place_f64(config: &AudioPortConfig, samples: u32) -> Self {
        Self::new(
            (0..config.inputs.len())
                .map(AudioBufferPort::Input)
                .chain((0..config.outputs.len()).map(AudioBufferPort::Output))
                .map(|port| port.create_buffer(config, samples, true))
                .collect(),
            samples,
        )
    }

    pub fn new_in_place_f32(config: &AudioPortConfig, samples: u32) -> Result<Self> {
        Ok(Self::new(
            resolve_in_place_pairs(config)?
                .iter()
                .map(|port| port.create_buffer(config, samples, false))
                .collect(),
            samples,
        ))
    }

    pub fn new_in_place_f64(config: &AudioPortConfig, samples: u32) -> Result<Self> {
        Ok(Self::new(
            resolve_in_place_pairs(config)?
                .iter()
                .map(|port| port.create_buffer(config, samples, true))
                .collect(),
            samples,
        ))
    }

    pub fn process<R>(&mut self, f: impl FnOnce(&[clap_audio_buffer], &mut [clap_audio_buffer]) -> R) -> R {
        for buffer in self.buffers.iter() {
            if let Some(input) = buffer.port().input() {
                self.clap_inputs[input].constant_mask = buffer.input_constant_mask.0;
                self.clap_inputs[input].latency = buffer.input_latency;
            }

            if let Some(output) = buffer.port().output() {
                self.clap_outputs[output].constant_mask = 0;
                self.clap_outputs[output].latency = 0;
            }
        }

        let result = f(&self.clap_inputs, &mut self.clap_outputs);

        for buffer in self.buffers.iter_mut() {
            if let Some(output) = buffer.port().output() {
                buffer.output_constant_mask = ConstantMask(self.clap_outputs[output].constant_mask);
                buffer.output_latency = self.clap_outputs[output].latency;
            }
        }

        result
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn fill_white_noise(&mut self, prng: &mut Pcg32) {
        for buffer in self.buffers.iter_mut() {
            if buffer.port().input().is_some() {
                buffer.fill_white_noise(prng);
            }
        }
    }

    pub fn fill_silence(&mut self) {
        for buffer in self.buffers.iter_mut() {
            if buffer.port().input().is_some() {
                buffer.fill_silence();
            }
        }
    }
}

impl AudioBuffer {
    pub fn new(port: AudioBufferPort, data: AudioBufferData) -> Self {
        Self {
            port,
            data,
            input_constant_mask: ConstantMask::DYNAMIC,
            output_constant_mask: ConstantMask::DYNAMIC,
            input_latency: 0,
            output_latency: 0,
        }
    }

    pub fn port(&self) -> AudioBufferPort {
        self.port
    }

    pub fn set_input_constant_mask(&mut self, mask: ConstantMask) {
        self.input_constant_mask = mask;
    }

    pub fn get_output_constant_mask(&self) -> ConstantMask {
        self.output_constant_mask
    }

    #[allow(unused)]
    pub fn set_input_latency(&mut self, latency: u32) {
        self.input_latency = latency;
    }

    #[allow(unused)]
    pub fn get_output_latency(&self) -> u32 {
        self.output_latency
    }

    pub fn fill_white_noise(&mut self, prng: &mut Pcg32) {
        self.data.fill_white_noise(prng);
        self.set_input_constant_mask(ConstantMask::DYNAMIC);
    }

    pub fn fill_silence(&mut self) {
        self.data.fill(0.0, 0.0);
        self.set_input_constant_mask(ConstantMask::CONSTANT);
    }
}

impl AudioBufferPort {
    pub fn input(&self) -> Option<usize> {
        match self {
            AudioBufferPort::Input(index) => Some(*index),
            AudioBufferPort::Inplace(index, _) => Some(*index),
            AudioBufferPort::Output(_) => None,
        }
    }

    pub fn output(&self) -> Option<usize> {
        match self {
            AudioBufferPort::Output(index) => Some(*index),
            AudioBufferPort::Inplace(_, index) => Some(*index),
            AudioBufferPort::Input(_) => None,
        }
    }

    pub fn create_buffer(self, config: &AudioPortConfig, samples: u32, is_double: bool) -> AudioBuffer {
        match self {
            AudioBufferPort::Input(index) => AudioBuffer::new(
                self,
                AudioBufferData::new(
                    config.inputs[index].channel_count,
                    samples,
                    is_double && config.inputs[index].supports_double_sample_size,
                ),
            ),
            AudioBufferPort::Output(index) => AudioBuffer::new(
                self,
                AudioBufferData::new(
                    config.outputs[index].channel_count,
                    samples,
                    is_double && config.outputs[index].supports_double_sample_size,
                ),
            ),
            AudioBufferPort::Inplace(input_index, output_index) => AudioBuffer::new(
                self,
                AudioBufferData::new(
                    config.inputs[input_index].channel_count,
                    samples,
                    is_double
                        && config.inputs[input_index].supports_double_sample_size
                        && config.outputs[output_index].supports_double_sample_size,
                ),
            ),
        }
    }
}

impl AudioBufferData {
    pub fn new(channels: u32, samples: u32, is_double: bool) -> Self {
        let data = if is_double {
            Either::Right(vec![vec![0.0f64; samples as usize].into_boxed_slice(); channels as usize].into_boxed_slice())
        } else {
            Either::Left(vec![vec![0.0f32; samples as usize].into_boxed_slice(); channels as usize].into_boxed_slice())
        };

        let pointers = match &data {
            Either::Left(data) => data.iter().map(|channel| channel.as_ptr() as *const ()).collect(),
            Either::Right(data) => data.iter().map(|channel| channel.as_ptr() as *const ()).collect(),
        };

        Self {
            samples,
            pointers,
            data,
        }
    }

    pub fn is_64bit(&self) -> bool {
        self.data.is_right()
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn channels(&self) -> u32 {
        match &self.data {
            Either::Left(data) => data.len() as u32,
            Either::Right(data) => data.len() as u32,
        }
    }

    pub fn channel(&self, channel: u32) -> Either<&[f32], &[f64]> {
        match &self.data {
            Either::Left(data) => Either::Left(&data[channel as usize]),
            Either::Right(data) => Either::Right(&data[channel as usize]),
        }
    }

    pub fn channel_mut(&mut self, channel: u32) -> Either<&mut [f32], &mut [f64]> {
        match &mut self.data {
            Either::Left(data) => Either::Left(&mut data[channel as usize]),
            Either::Right(data) => Either::Right(&mut data[channel as usize]),
        }
    }

    pub fn get(&self, channel: u32, sample: u32) -> Either<f32, f64> {
        match &self.data {
            Either::Left(data) => Either::Left(data[channel as usize][sample as usize]),
            Either::Right(data) => Either::Right(data[channel as usize][sample as usize]),
        }
    }

    pub fn is_same(&self, other: &Self) -> bool {
        if self.channels() != other.channels() {
            return false;
        }

        for channel in 0..self.channels() {
            let left = self.channel(channel);
            let right = other.channel(channel);

            match (left, right) {
                (Either::Left(left), Either::Left(right)) => {
                    if left != right {
                        return false;
                    }
                }
                (Either::Right(left), Either::Right(right)) => {
                    if left != right {
                        return false;
                    }
                }
                _ => return false,
            }
        }

        true
    }

    /// Fill the buffer with silence (zeros).
    pub fn fill(&mut self, value_f32: f32, value_f64: f64) {
        for channel in 0..self.channels() {
            match self.channel_mut(channel) {
                Either::Left(data) => data.fill(value_f32),
                Either::Right(data) => data.fill(value_f64),
            }
        }
    }

    /// Fill the buffer with white noise (random values in the range [-1, 1]).
    pub fn fill_white_noise(&mut self, prng: &mut Pcg32) {
        for channel in 0..self.channels() {
            match self.channel_mut(channel) {
                Either::Left(data) => data.fill_with(|| prng.random_range(-1.0..1.0)),
                Either::Right(data) => data.fill_with(|| prng.random_range(-1.0..1.0)),
            }
        }
    }

    pub fn as_ptr(&self) -> Either<*mut *mut f32, *mut *mut f64> {
        match &self.data {
            Either::Left(_) => Either::Left(self.pointers.as_ptr() as *mut *mut f32),
            Either::Right(_) => Either::Right(self.pointers.as_ptr() as *mut *mut f64),
        }
    }
}

unsafe impl Send for AudioBufferData {}
unsafe impl Sync for AudioBufferData {}

impl Clone for AudioBufferData {
    fn clone(&self) -> Self {
        let data = self.data.clone();

        Self {
            samples: self.samples,
            pointers: data.as_ref().either(
                |x| x.iter().map(|channel| channel.as_ptr() as *const ()).collect(),
                |x| x.iter().map(|channel| channel.as_ptr() as *const ()).collect(),
            ),
            data,
        }
    }
}

impl Debug for AudioBufferData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioBufferData")
            .field("channels", &self.channels())
            .field("type", if self.is_64bit() { &"f64" } else { &"f32" })
            .finish()
    }
}

impl Deref for AudioBuffer {
    type Target = AudioBufferData;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl DerefMut for AudioBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl Deref for AudioBuffers {
    type Target = [AudioBuffer];
    fn deref(&self) -> &Self::Target {
        &self.buffers
    }
}

impl DerefMut for AudioBuffers {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffers
    }
}

/// Resolve the in-place pairs from the given audio port configuration.
///
/// Returns an error if there are any inconsistencies, such as an input or output port
/// referencing a non-existent in-place pair.
fn resolve_in_place_pairs(config: &AudioPortConfig) -> Result<Vec<AudioBufferPort>> {
    let mut ports = vec![];
    let mut in_place: HashMap<(u32, u32), (Option<usize>, Option<usize>)> = HashMap::new();

    for (index, port) in config.inputs.iter().enumerate() {
        if let Some(inplace_id) = port.in_place_pair {
            in_place.entry((port.id, inplace_id)).or_default().0 = Some(index);
        } else {
            ports.push(AudioBufferPort::Input(index));
        }
    }

    for (index, port) in config.outputs.iter().enumerate() {
        if let Some(inplace_id) = port.in_place_pair {
            in_place.entry((inplace_id, port.id)).or_default().1 = Some(index);
        } else {
            ports.push(AudioBufferPort::Output(index));
        }
    }

    for ((input_id, output_id), (input, output)) in in_place {
        match (input, output) {
            (None, Some(output)) => anyhow::bail!(
                "Output port {output} has an in-place pair ({input_id}), but the corresponding input port does not \
                 exist."
            ),
            (Some(input), None) => anyhow::bail!(
                "Input port {input} has an in-place pair ({output_id}), but the corresponding output port does not \
                 exist."
            ),
            (Some(input), Some(output)) => {
                if config.inputs[input].channel_count != config.outputs[output].channel_count {
                    // TODO: is this allowed?
                    // anyhow::bail!(
                    //     "Input port {input} and output port {output} are configured as an in-place pair, but they \
                    //      have different channel counts ({} vs {}).",
                    //     config.inputs[input].channel_count,
                    //     config.outputs[output].channel_count
                    // );

                    ports.push(AudioBufferPort::Input(input));
                    ports.push(AudioBufferPort::Output(output));
                    continue;
                }

                ports.push(AudioBufferPort::Inplace(input, output));
            }
            _ => {}
        }
    }

    Ok(ports)
}
