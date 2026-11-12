pub const WHISPER_RATE: u32 = 16_000;
pub const MAX_RATE: u32 = 192_000;
pub const MAX_CHANNELS: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub width: u16,
    pub channels: u16,
}

impl Format {
    pub fn valid(self) -> bool {
        (1_000..=MAX_RATE).contains(&self.rate)
            && (1..=4).contains(&self.width)
            && (1..=MAX_CHANNELS).contains(&self.channels)
    }
    pub fn frame_bytes(self) -> usize {
        usize::from(self.width) * usize::from(self.channels)
    }
}

/// Wyoming and WAV PCM are little endian; 8-bit samples are unsigned as in WAV.
pub fn mono_i16(bytes: &[u8], format: Format) -> Vec<i16> {
    let frame = format.frame_bytes();
    if frame == 0 {
        return Vec::new();
    }
    let width = usize::from(format.width);
    let channels = i64::from(format.channels);
    bytes
        .chunks_exact(frame)
        .map(|frame| {
            let sum: i64 = frame
                .chunks_exact(width)
                .map(|sample| i64::from(sample_i16(sample)))
                .sum();
            (sum / channels) as i16
        })
        .collect()
}

fn sample_i16(sample: &[u8]) -> i16 {
    match sample.len() {
        1 => (i16::from(sample[0]) - 128) << 8,
        2 => i16::from_le_bytes([sample[0], sample[1]]),
        3 => i16::from_le_bytes([sample[1], sample[2]]),
        _ => i16::from_le_bytes([sample[2], sample[3]]),
    }
}

/// Downsampling averages each output window so energy above the new Nyquist rate does not
/// fold back as audible aliasing; upsampling interpolates linearly.
pub fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }
    let output_len = ((samples.len() as u64 * u64::from(to)) / u64::from(from)) as usize;
    let ratio = f64::from(from) / f64::from(to);
    let last = samples.len() - 1;
    (0..output_len)
        .map(|index| {
            let center = index as f64 * ratio;
            if ratio > 1.0 {
                let start = center.floor() as usize;
                let end = ((center + ratio).ceil() as usize)
                    .min(samples.len())
                    .max(start + 1);
                let window = &samples[start.min(last)..end];
                (window.iter().map(|value| i64::from(*value)).sum::<i64>() / window.len() as i64)
                    as i16
            } else {
                let base = (center.floor() as usize).min(last);
                let next = (base + 1).min(last);
                let fraction = center - base as f64;
                let value = f64::from(samples[base]) * (1.0 - fraction)
                    + f64::from(samples[next]) * fraction;
                value.round() as i16
            }
        })
        .collect()
}

pub fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

pub fn to_le_bytes(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

pub fn from_le_bytes(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    pub rate: u32,
    pub samples: Vec<i16>,
}

/// Accepts integer PCM and 32-bit float WAV, including WAVE_FORMAT_EXTENSIBLE; streamed
/// writers may leave the data length unset, so a short or oversized data chunk is clipped.
pub fn decode_wav(bytes: &[u8]) -> Option<Decoded> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut offset = 12;
    let mut format: Option<(u16, Format)> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let declared = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        let body = offset + 8;
        let end = body.saturating_add(declared).min(bytes.len());
        match id {
            b"fmt " if end - body >= 16 => {
                let field =
                    |at: usize| u16::from_le_bytes([bytes[body + at], bytes[body + at + 1]]);
                let mut tag = field(0);
                if tag == 0xfffe && end - body >= 26 {
                    tag = field(24);
                }
                let rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().ok()?);
                let bits = field(14);
                format = Some((
                    tag,
                    Format {
                        rate,
                        width: bits.div_ceil(8),
                        channels: field(2),
                    },
                ));
            }
            b"data" => {
                let (tag, format) = format?;
                if !format.valid() {
                    return None;
                }
                let data = &bytes[body..end];
                let samples = match (tag, format.width) {
                    (1, _) => mono_i16(data, format),
                    (3, 4) => {
                        let channels = usize::from(format.channels);
                        data.chunks_exact(4 * channels)
                            .map(|frame| {
                                let sum: f32 = frame
                                    .chunks_exact(4)
                                    .map(|value| {
                                        let value = f32::from_le_bytes(value.try_into().unwrap());
                                        if value.is_finite() { value } else { 0.0 }
                                    })
                                    .sum();
                                ((sum / channels as f32).clamp(-1.0, 1.0) * 32767.0) as i16
                            })
                            .collect()
                    }
                    _ => return None,
                };
                return Some(Decoded {
                    rate: format.rate,
                    samples,
                });
            }
            _ => {}
        }
        offset = body.saturating_add(declared).saturating_add(declared & 1);
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VadSettings {
    pub threshold: f64,
    pub silence_ms: u32,
    pub prefix_padding_ms: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum VadEvent {
    Started { at_ms: u64 },
    Stopped { at_ms: u64 },
}

/// An energy gate over 20 ms frames. `threshold` keeps the OpenAI 0..1 scale: 0.5 maps to
/// an RMS level of about -34 dBFS, and higher values need louder speech.
pub struct Vad {
    settings: VadSettings,
    frame: usize,
    pending: Vec<i16>,
    processed_ms: u64,
    speaking: bool,
    silent_ms: u32,
    voiced_ms: u32,
}

const VAD_FRAME_MS: u32 = 20;
const VAD_ONSET_MS: u32 = 60;

impl Vad {
    pub fn new(settings: VadSettings, rate: u32) -> Self {
        Self {
            settings,
            frame: (rate / (1000 / VAD_FRAME_MS)).max(1) as usize,
            pending: Vec::new(),
            processed_ms: 0,
            speaking: false,
            silent_ms: 0,
            voiced_ms: 0,
        }
    }
    pub fn speaking(&self) -> bool {
        self.speaking
    }
    fn gate(&self) -> f64 {
        0.02 * (self.settings.threshold / 0.5).powi(2)
    }
    /// Keeps the partial frame and the clock so later event times stay on the session timeline.
    pub fn reset(&mut self) {
        self.speaking = false;
        self.silent_ms = 0;
        self.voiced_ms = 0;
    }
    pub fn push(&mut self, samples: &[i16]) -> Vec<VadEvent> {
        self.pending.extend_from_slice(samples);
        let mut events = Vec::new();
        let gate = self.gate();
        let mut consumed = 0;
        while self.pending.len() - consumed >= self.frame {
            let frame = &self.pending[consumed..consumed + self.frame];
            consumed += self.frame;
            let energy = frame
                .iter()
                .map(|sample| (f64::from(*sample) / 32768.0).powi(2))
                .sum::<f64>()
                / frame.len() as f64;
            let voiced = energy.sqrt() >= gate;
            self.processed_ms += u64::from(VAD_FRAME_MS);
            if self.speaking {
                if voiced {
                    self.silent_ms = 0;
                } else {
                    self.silent_ms += VAD_FRAME_MS;
                    if self.silent_ms >= self.settings.silence_ms.max(VAD_FRAME_MS) {
                        self.speaking = false;
                        self.voiced_ms = 0;
                        events.push(VadEvent::Stopped {
                            at_ms: self.processed_ms,
                        });
                    }
                }
            } else if voiced {
                self.voiced_ms += VAD_FRAME_MS;
                if self.voiced_ms >= VAD_ONSET_MS {
                    self.speaking = true;
                    self.silent_ms = 0;
                    events.push(VadEvent::Started {
                        at_ms: self
                            .processed_ms
                            .saturating_sub(u64::from(self.voiced_ms))
                            .saturating_sub(u64::from(self.settings.prefix_padding_ms)),
                    });
                }
            } else {
                self.voiced_ms = 0;
            }
        }
        self.pending.drain(..consumed);
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, ms: u32, amplitude: f64) -> Vec<i16> {
        (0..rate * ms / 1000)
            .map(|index| {
                (amplitude
                    * 32767.0
                    * (2.0 * std::f64::consts::PI * 440.0 * f64::from(index) / f64::from(rate))
                        .sin()) as i16
            })
            .collect()
    }

    #[test]
    fn mixed_widths_and_channels_fold_to_signed_mono() {
        let stereo = [0x00, 0x40, 0x00, 0xc0, 0xff, 0x7f, 0xff, 0x7f];
        assert_eq!(
            mono_i16(
                &stereo,
                Format {
                    rate: 8000,
                    width: 2,
                    channels: 2
                }
            ),
            [0, 32767]
        );
        assert_eq!(
            mono_i16(
                &[0, 128, 255],
                Format {
                    rate: 8000,
                    width: 1,
                    channels: 1
                }
            ),
            [-32768, 0, 32512]
        );
        assert_eq!(
            mono_i16(
                &[0x11, 0x22, 0x33, 0x00, 0x00, 0x40, 0x7f],
                Format {
                    rate: 8000,
                    width: 3,
                    channels: 1
                }
            ),
            [0x3322, 0x4000]
        );
        assert_eq!(
            mono_i16(
                &[0, 0, 0, 0x80],
                Format {
                    rate: 8000,
                    width: 4,
                    channels: 1
                }
            ),
            [i16::MIN]
        );
    }

    #[test]
    fn resampling_preserves_duration_and_tone_level() {
        let input = tone(24_000, 1000, 0.5);
        let output = resample(&input, 24_000, WHISPER_RATE);
        assert_eq!(output.len(), 16_000);
        let peak = output
            .iter()
            .map(|value| value.unsigned_abs())
            .max()
            .unwrap();
        assert!((14_000..=16_500).contains(&peak), "{peak}");
        let up = resample(&tone(8_000, 500, 0.5), 8_000, WHISPER_RATE);
        assert_eq!(up.len(), 8_000);
        assert_eq!(resample(&[1, 2, 3], 16_000, 16_000), [1, 2, 3]);
        let alias = resample(&tone(48_000, 200, 0.0), 48_000, WHISPER_RATE);
        assert!(alias.iter().all(|value| *value == 0));
    }

    #[test]
    fn wav_round_trips_and_float_extensible_wav_decodes() {
        let samples = [0i16, 1000, -1000, i16::MAX];
        let bytes = wav(&samples, WHISPER_RATE);
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(
            decode_wav(&bytes),
            Some(Decoded {
                rate: WHISPER_RATE,
                samples: samples.to_vec()
            })
        );
        let mut float = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        float.extend(40u32.to_le_bytes());
        float.extend(0xfffeu16.to_le_bytes());
        float.extend(1u16.to_le_bytes());
        float.extend(24_000u32.to_le_bytes());
        float.extend(96_000u32.to_le_bytes());
        float.extend(4u16.to_le_bytes());
        float.extend(32u16.to_le_bytes());
        float.extend(22u16.to_le_bytes());
        float.extend([0u8; 6]);
        float.extend(3u16.to_le_bytes());
        float.extend([0u8; 14]);
        float.extend(b"LIST\x03\0\0\0abc\0");
        float.extend(b"data\xff\xff\xff\xff");
        for value in [0.5f32, -2.0, f32::NAN] {
            float.extend(value.to_le_bytes());
        }
        float.push(1);
        let decoded = decode_wav(&float).unwrap();
        assert_eq!(decoded.rate, 24_000);
        assert_eq!(decoded.samples, [16383, -32767, 0]);
        assert!(decode_wav(b"RIFF\0\0\0\0WAVEdata\0\0\0\0").is_none());
        assert!(decode_wav(b"not a wav").is_none());
    }

    #[test]
    fn energy_vad_reports_onset_and_trailing_silence() {
        let settings = VadSettings {
            threshold: 0.5,
            silence_ms: 200,
            prefix_padding_ms: 0,
        };
        let mut vad = Vad::new(settings, WHISPER_RATE);
        assert!(vad.push(&vec![0; 16_000]).is_empty());
        let started = vad.push(&tone(WHISPER_RATE, 500, 0.3));
        assert_eq!(started, [VadEvent::Started { at_ms: 1000 }]);
        assert!(vad.speaking());
        assert!(vad.push(&vec![0; 1_600]).is_empty());
        let stopped = vad.push(&vec![0; 3_200]);
        assert_eq!(stopped, [VadEvent::Stopped { at_ms: 1700 }]);
        let mut strict = Vad::new(
            VadSettings {
                threshold: 0.99,
                ..settings
            },
            WHISPER_RATE,
        );
        assert!(strict.push(&tone(WHISPER_RATE, 500, 0.02)).is_empty());
    }
}
