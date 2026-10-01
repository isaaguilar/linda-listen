use crate::{
    capture::CapturedAudio,
    error::{AppError, AppResult},
};
use parakeet_rs::{ParakeetTDT, TimestampMode, Transcriber};
use std::path::PathBuf;

impl CapturedAudio {
    pub fn to_parakeet_samples(&self) -> AppResult<Vec<f32>> {
        if self.samples.is_empty() {
            return Err(AppError::Transcription(
                "no audio samples were captured".to_owned(),
            ));
        }

        let mono = downmix_to_mono(&self.samples, self.channels as usize)?;
        let resampled = if self.sample_rate == 16_000 {
            mono
        } else {
            resample_linear(&mono, self.sample_rate, 16_000)
        };

        Ok(normalize_audio(&resampled))
    }
}

pub struct ParakeetTranscriber {
    model: ParakeetTDT,
}

impl ParakeetTranscriber {
    pub fn new(model_dir: impl Into<PathBuf>) -> AppResult<Self> {
        let model_dir = model_dir.into();
        if !model_dir.is_dir() {
            return Err(AppError::MissingModel(model_dir));
        }

        let model = ParakeetTDT::from_pretrained(&model_dir, None)
            .map_err(|err| AppError::Transcription(err.to_string()))?;

        Ok(Self { model })
    }

    pub fn transcribe(&mut self, audio: &CapturedAudio) -> AppResult<String> {
        let samples = audio.to_parakeet_samples()?;
        let result = self
            .model
            .transcribe_samples(samples, 16_000, 1, Some(TimestampMode::Sentences))
            .map_err(|err| AppError::Transcription(err.to_string()))?;

        let transcript = result.text.trim().to_owned();
        if transcript.is_empty() {
            return Err(AppError::Transcription(
                "parakeet returned an empty transcript".to_owned(),
            ));
        }

        Ok(transcript)
    }
}

fn normalize_audio(samples: &[f32]) -> Vec<f32> {
    let peak = samples
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()));
    if peak <= 1e-6 {
        return samples.to_vec();
    }

    let gain = 1.0 / peak;
    samples.iter().map(|sample| sample * gain).collect()
}

fn downmix_to_mono(samples: &[f32], channels: usize) -> AppResult<Vec<f32>> {
    if channels == 0 {
        return Err(AppError::Transcription(
            "captured audio did not report any channels".to_owned(),
        ));
    }

    if channels == 1 {
        return Ok(samples.to_vec());
    }

    let frames = samples.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for frame in 0..frames {
        let mut sum = 0.0f32;
        for channel in 0..channels {
            sum += samples[frame * channels + channel];
        }
        mono.push(sum / channels as f32);
    }

    Ok(mono)
}

fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == target_rate {
        return samples.to_vec();
    }

    if samples.len() == 1 {
        return vec![samples[0]];
    }

    let target_len = ((samples.len() as f64) * (target_rate as f64) / (source_rate as f64))
        .round()
        .max(1.0) as usize;
    let last_index = samples.len() - 1;
    let mut output = Vec::with_capacity(target_len);

    for index in 0..target_len {
        let position =
            index as f64 * last_index as f64 / (target_len.saturating_sub(1).max(1) as f64);
        let left = position.floor() as usize;
        let right = left.min(last_index);
        let next = (right + 1).min(last_index);
        let fraction = (position - left as f64) as f32;
        let sample = samples[right] * (1.0 - fraction) + samples[next] * fraction;
        output.push(sample);
    }

    output
}

#[cfg(test)]
mod tests {
    use crate::capture::CapturedAudio;

    #[test]
    fn resamples_48khz_audio_to_16khz_mono() {
        let audio = CapturedAudio {
            samples: vec![0.25; 48_000],
            sample_rate: 48_000,
            channels: 1,
        };

        let samples = audio.to_parakeet_samples().unwrap();

        assert_eq!(samples.len(), 16_000);
        assert!((samples[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn downmixes_stereo_audio_before_resampling() {
        let audio = CapturedAudio {
            samples: vec![1.0, -1.0, 0.5, 0.5],
            sample_rate: 16_000,
            channels: 2,
        };

        let samples = audio.to_parakeet_samples().unwrap();

        assert_eq!(samples.len(), 2);
        assert!((samples[0] - 0.0).abs() < 1e-6);
        assert!((samples[1] - 1.0).abs() < 1e-6);
    }
}
