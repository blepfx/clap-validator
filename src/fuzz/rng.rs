use rand::seq::IndexedRandom;
use rand::{Rng, RngExt, SeedableRng};

#[derive(Debug, Clone)]
pub enum TestAudioSignal {
    /// A block consisting of denormal numbers, not marked as constant.
    NoiseDenormal { length: u32 },
    /// A block consisting of white noise, not marked as constant.
    NoiseWhite { length: u32, amplitude: f64 },
    /// A block marked as constant, might actually contain small non-zero noise specified by the amplitude parameter.
    Constant { length: u32, amplitude: f64, noise: f64 },
    /// A single sample impulse at the beginning of the block, with the given amplitude, followed by silence. Not marked as constant.
    Impulse { length: u32, amplitude: f64 },
    /// A sine wave that sweeps from start_freq to end_freq linearly, and from start_amplitude to end_amplitude over length samples. Not marked as constant.
    LinearSweep {
        length: u32,
        start_freq: f64,
        end_freq: f64,
        start_amplitude: f64,
        end_amplitude: f64,
    },
}

impl TestAudioSignal {
    pub fn random(rng: &mut impl Rng) -> Self {
        let length = rng.random_range(1..10000);
        let amplitude = if rng.random_bool(0.05) {
            rng.random_range(0.0..1.0)
        } else {
            128.0
        };

        match rng.random_range(0..5) {
            0 => Self::NoiseDenormal { length },
            1 => Self::NoiseWhite { length, amplitude },
            2 => Self::Constant {
                length,
                amplitude,
                noise: if rng.random_bool(0.1) {
                    rng.random_range(0.0..1e-5)
                } else {
                    0.0
                },
            },
            3 => Self::Impulse { length, amplitude },
            4 => Self::LinearSweep {
                length,
                start_freq: rng.random_range(0.0..0.5),
                end_freq: rng.random_range(0.0..0.5),
                start_amplitude: amplitude,
                end_amplitude: amplitude * rng.random_range(0.0..2.0),
            },
            _ => unreachable!(),
        }
    }

    pub fn length(&self) -> u32 {
        match self {
            Self::NoiseDenormal { length }
            | Self::NoiseWhite { length, .. }
            | Self::Constant { length, .. }
            | Self::Impulse { length, .. }
            | Self::LinearSweep { length, .. } => *length,
        }
    }

    pub fn fill_f64(&mut self, rng: &mut impl Rng, buffer: &mut [f64]) {
        assert!(buffer.len() >= self.length() as usize);

        match self {
            Self::NoiseDenormal { length } => {
                for sample in buffer.iter_mut().take(*length as usize) {
                    *sample = rng.random_range(-f64::MIN_POSITIVE..f64::MIN_POSITIVE);
                }
            }

            Self::NoiseWhite { length, amplitude } => {
                for sample in buffer.iter_mut().take(*length as usize) {
                    *sample = rng.random_range(-*amplitude..*amplitude);
                }
            }

            Self::Constant {
                length,
                amplitude,
                noise,
            } => {
                for sample in buffer.iter_mut().take(*length as usize) {
                    *sample = *amplitude + rng.random_range(-*noise..*noise);
                }
            }

            Self::Impulse { length, amplitude } => {
                if !buffer.is_empty() && *length > 0 {
                    buffer[0] = *amplitude;
                }
            }

            Self::LinearSweep {
                length,
                start_freq,
                end_freq,
                start_amplitude,
                end_amplitude,
            } => {
                for (i, sample) in buffer.iter_mut().take(*length as usize).enumerate() {
                    let t = i as f64 / (*length as f64);
                    let freq = start_freq + t * (end_freq - start_freq);
                    let amplitude = start_amplitude + t * (end_amplitude - start_amplitude);
                    *sample = amplitude * (2.0 * std::f64::consts::PI * freq * t).sin();
                }
            }
        }
    }
}
