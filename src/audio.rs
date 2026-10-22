//! Microphone capture: native-rate input from the default device, downmixed to
//! mono and resampled to the 16 kHz whisper.cpp expects when recording stops.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const WHISPER_RATE: u32 = 16_000;

/// A live recording. Dropping it stops the microphone.
pub struct Recording {
    _stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

/// Starts recording from the default input device (whatever macOS has
/// selected). `level` receives the RMS of each audio callback (as f32 bits) to
/// drive the overlay meter.
pub fn start(level: Arc<AtomicU32>) -> Result<Recording, String> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("no microphone found")?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    // shortcut: f32 only, which is what CoreAudio hands out; add i16 if another backend needs it.
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("unsupported sample format {:?}", supported.sample_format()));
    }
    let config = supported.config();
    let channels = config.channels as usize;
    let rate = config.sample_rate;

    let buf = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 60)));
    let sink = buf.clone();
    let stream = device
        .build_input_stream::<f32, _, _>(
            config,
            move |data, _| {
                let (mono, rms) = mix(data, channels);
                level.store(rms.to_bits(), Ordering::Relaxed);
                sink.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("audio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Recording {
        _stream: stream,
        buf,
        rate,
    })
}

impl Recording {
    /// Everything recorded so far as 16 kHz mono, while recording continues.
    pub fn snapshot(&self) -> Vec<f32> {
        resample(&self.buf.lock().unwrap(), self.rate, WHISPER_RATE)
    }

    /// Stops the microphone and returns 16 kHz mono samples.
    pub fn finish(self) -> Vec<f32> {
        self.snapshot()
    }
}

/// Downmixes interleaved frames to mono, and returns their RMS level too.
fn mix(data: &[f32], channels: usize) -> (Vec<f32>, f32) {
    let mono: Vec<f32> = data
        .chunks(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();
    let rms = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len().max(1) as f32).sqrt();
    (mono, rms)
}

/// Cuts leading and trailing silence, keeping a short margin around the speech.
/// Whisper invents text ("Thank you.") for silence, typically the gap between
/// the last word and the stop tap. Returns an empty slice if nothing is loud.
pub fn trim_silence(samples: &[f32]) -> &[f32] {
    const MARGIN: usize = WHISPER_RATE as usize * 3 / 10; // 300 ms
    let loud = loud_frames(samples);
    let (Some(first), Some(last)) = (loud.iter().position(|l| *l), loud.iter().rposition(|l| *l)) else {
        return &[];
    };
    let start = (first * FRAME).saturating_sub(MARGIN);
    let end = ((last + 1) * FRAME + MARGIN).min(samples.len());
    &samples[start..end]
}

/// How many samples of `samples` are speech (loud frames), wherever they are.
pub fn speech_len(samples: &[f32]) -> usize {
    loud_frames(samples).iter().filter(|l| **l).count() * FRAME
}

const FRAME: usize = WHISPER_RATE as usize / 50; // 20 ms

/// Which 20 ms frames are speech rather than silence.
fn loud_frames(samples: &[f32]) -> Vec<bool> {
    let rms: Vec<f32> = samples
        .chunks(FRAME)
        .map(|f| (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().copied().fold(0.0, f32::max);
    // calibration: speech peaks well above 0.02 on a laptop mic; breath and room noise stay under 5% of the peak.
    rms.iter().map(|r| *r > (peak * 0.05).max(0.002)).collect()
}

// shortcut: linear interpolation without a low-pass filter; fine for speech into whisper, swap for a sinc resampler if accuracy suffers.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let len = (input.len() as f64 / ratio) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let j = pos as usize;
            let frac = (pos - j as f64) as f32;
            let next = input.get(j + 1).copied().unwrap_or(input[j]);
            input[j] + (next - input[j]) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{mix, resample, speech_len, trim_silence};

    #[test]
    fn speech_len_counts_only_speech_not_the_gap_between() {
        let second = 16_000;
        let mut clip = vec![0.3f32; second / 10]; // a click
        clip.extend(vec![0.0005; 2 * second]); // a long pause
        clip.extend(vec![0.3; second / 5]); // a syllable
        assert!(trim_silence(&clip).len() > 2 * second); // spans the pause
        assert_eq!(speech_len(&clip), 3 * second / 10); // but holds 0.3 s of sound
    }

    #[test]
    fn mix_downmixes_to_mono_and_measures_rms() {
        let (mono, rms) = mix(&[0.5, -0.5, 0.2, 0.6], 2);
        assert_eq!(mono, [0.0, 0.4]);
        assert!((rms - (0.08f32).sqrt()).abs() < 1e-6);
        assert_eq!(mix(&[0.3, 0.3], 1), (vec![0.3, 0.3], 0.3));
        assert_eq!(mix(&[], 2), (vec![], 0.0));
    }

    #[test]
    fn trim_silence_keeps_speech_plus_margin() {
        let second = 16_000;
        let mut clip = vec![0.0005f32; second]; // 1 s room noise
        clip.extend((0..second).map(|i| 0.2 * (i as f32 * 0.05).sin())); // 1 s "speech"
        clip.extend(vec![0.0005f32; 2 * second]); // 2 s before the stop tap
        let kept = trim_silence(&clip);
        assert!(
            kept.len() >= second && kept.len() <= second + second * 6 / 10 + 640,
            "{}",
            kept.len()
        );
        assert!(trim_silence(&vec![0.0005; second]).is_empty());
        assert!(trim_silence(&[]).is_empty());
    }

    #[test]
    fn resample_48k_to_16k_keeps_duration_and_shape() {
        let ramp: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        let out = resample(&ramp, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!((out[8_000] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn resample_44100_interpolates_between_samples() {
        let out = resample(&[0.0, 1.0, 0.0, 1.0], 44_100, 16_000);
        assert_eq!(out.len(), 1);
        assert_eq!(resample(&[], 44_100, 16_000), Vec::<f32>::new());
    }
}
