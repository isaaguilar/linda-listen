use crate::{
    capture::CapturedAudio,
    error::{AppError, AppResult},
};
use std::{
    fs::File,
    path::Path,
};
use symphonia::core::{
    audio::GenericAudioBufferRef,
    codecs::audio::{AudioDecoderOptions, CODEC_ID_NULL_AUDIO},
    codecs::registry::CodecRegistry,
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    formats::probe::{Hint, Probe},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

pub const MAX_AUDIO_FILE_BYTES: u64 = 250 * 1024 * 1024;
pub const MAX_AUDIO_DURATION_SECS: u64 = 30 * 60;
const TARGET_SAMPLE_RATE: u32 = 16_000;
const SUPPORTED_FORMATS: &str = "WAV, MP3, M4A/AAC, FLAC, and OGG/Opus";

pub fn decode_audio_file(path: &Path) -> AppResult<CapturedAudio> {
    validate_file_metadata(path)?;
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
    {
        return decode_wav_file(path);
    }

    let file = File::open(path)?;
    let media_source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    fn decode_wav_file(path: &Path) -> AppResult<CapturedAudio> {
        let mut reader = hound::WavReader::open(path)?;
        let spec = reader.spec();
        if spec.sample_rate == 0 || spec.channels == 0 {
            return Err(AppError::AudioDecoding(
                "WAV file reported an invalid audio specification".to_owned(),
            ));
        }
        validate_duration(u64::from(reader.duration()), spec.sample_rate)?;

        let mut resampler = MonoResampler::new(spec.sample_rate);
        let mut samples = Vec::with_capacity(4096);
        let sample_count = usize::from(spec.channels) * 4096;
        match spec.sample_format {
            hound::SampleFormat::Int if spec.bits_per_sample <= 16 => {
                for sample in reader.samples::<i16>() {
                    samples.push(sample? as f32 / i16::MAX as f32);
                    if samples.len() == sample_count {
                        resampler.push_interleaved(&samples, usize::from(spec.channels))?;
                        samples.clear();
                    }
                }
            }
            hound::SampleFormat::Int => {
                for sample in reader.samples::<i32>() {
                    samples.push(sample? as f32 / i32::MAX as f32);
                    if samples.len() == sample_count {
                        resampler.push_interleaved(&samples, usize::from(spec.channels))?;
                        samples.clear();
                    }
                }
            }
            hound::SampleFormat::Float => {
                for sample in reader.samples::<f32>() {
                    samples.push(sample?);
                    if samples.len() == sample_count {
                        resampler.push_interleaved(&samples, usize::from(spec.channels))?;
                        samples.clear();
                    }
                }
            }
        }
        if !samples.is_empty() {
            resampler.push_interleaved(&samples, usize::from(spec.channels))?;
        }

        resampler.finish();
        if resampler.samples.is_empty() {
            return Err(AppError::AudioDecoding(
                "audio file did not contain any decodable samples".to_owned(),
            ));
        }
        Ok(CapturedAudio {
            samples: resampler.samples,
            sample_rate: TARGET_SAMPLE_RATE,
            channels: 1,
        })
    }

    let mut probe = Probe::default();
    symphonia::default::register_enabled_formats(&mut probe);
    let mut format = probe
        .probe(
            &hint,
            media_source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| unsupported_format(error.to_string()))?;
    let track = format
        .default_track(symphonia::core::formats::TrackType::Audio)
        .ok_or_else(|| unsupported_format("audio file has no playable track".to_owned()))?;
    let track_id = track.id;
    let frame_count = track.num_frames;
    let codec_params = track
        .codec_params
        .as_ref()
        .ok_or_else(|| AppError::AudioDecoding("audio track has no codec parameters".to_owned()))?
        .audio()
        .ok_or_else(|| unsupported_format("selected track is not audio".to_owned()))?
        .clone();

    let source_rate = codec_params.sample_rate.ok_or_else(|| {
        AppError::AudioDecoding("audio file does not report a sample rate".to_owned())
    })?;
    if source_rate == 0 {
        return Err(AppError::AudioDecoding(
            "audio file reported an invalid sample rate".to_owned(),
        ));
    }
    if let Some(frame_count) = frame_count {
        validate_duration(frame_count, source_rate)?;
    }
    if codec_params.codec == CODEC_ID_NULL_AUDIO {
        return Err(unsupported_format(
            "audio file does not contain a supported codec".to_owned(),
        ));
    }

    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    codecs.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    let mut decoder = codecs
        .make_audio_decoder(&codec_params, &AudioDecoderOptions::default())
        .map_err(|error| AppError::AudioDecoding(error.to_string()))?;
    let mut resampler = MonoResampler::new(source_rate);

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => return Err(AppError::AudioDecoding(error.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }

        let decoded = decoder
            .decode(&packet)
            .map_err(|error| AppError::AudioDecoding(error.to_string()))?;
        append_decoded(&mut resampler, decoded)?;
    }

    resampler.finish();
    if resampler.samples.is_empty() {
        return Err(AppError::AudioDecoding(
            "audio file did not contain any decodable samples".to_owned(),
        ));
    }

    Ok(CapturedAudio {
        samples: resampler.samples,
        sample_rate: TARGET_SAMPLE_RATE,
        channels: 1,
    })
}

fn append_decoded(
    resampler: &mut MonoResampler,
    decoded: GenericAudioBufferRef<'_>,
) -> AppResult<()> {
    let channels = decoded.spec().channels().count();
    if channels == 0 {
        return Err(AppError::AudioDecoding(
            "decoded audio reported no channels".to_owned(),
        ));
    }
    if decoded.spec().rate() != resampler.source_rate {
        return Err(AppError::AudioDecoding(
            "audio track changed sample rate while decoding".to_owned(),
        ));
    }

    let mut samples = Vec::with_capacity(decoded.samples_interleaved());
    decoded.copy_to_vec_interleaved::<f32>(&mut samples);
    resampler.push_interleaved(&samples, channels)
}

fn validate_file_metadata(path: &Path) -> AppResult<()> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(AppError::AudioDecoding(
            "selected audio path is not a file".to_owned(),
        ));
    }
    if metadata.len() > MAX_AUDIO_FILE_BYTES {
        return Err(AppError::AudioFileTooLarge {
            max_bytes: MAX_AUDIO_FILE_BYTES,
        });
    }
    Ok(())
}

fn validate_duration(frame_count: u64, sample_rate: u32) -> AppResult<()> {
    let max_frames = u64::from(sample_rate)
        .checked_mul(MAX_AUDIO_DURATION_SECS)
        .ok_or_else(|| AppError::AudioFileTooLong {
            max_seconds: MAX_AUDIO_DURATION_SECS,
        })?;
    if frame_count > max_frames {
        return Err(AppError::AudioFileTooLong {
            max_seconds: MAX_AUDIO_DURATION_SECS,
        });
    }
    Ok(())
}

fn unsupported_format(detail: String) -> AppError {
    AppError::UnsupportedAudioFormat(format!(
        "{detail}. Supported formats: {SUPPORTED_FORMATS}"
    ))
}

struct MonoResampler {
    source_rate: u32,
    source_frames: u64,
    next_target_frame: u64,
    previous_sample: Option<f32>,
    samples: Vec<f32>,
}

impl MonoResampler {
    fn new(source_rate: u32) -> Self {
        Self {
            source_rate,
            source_frames: 0,
            next_target_frame: 0,
            previous_sample: None,
            samples: Vec::new(),
        }
    }

    fn push_interleaved(&mut self, samples: &[f32], channels: usize) -> AppResult<()> {
        for frame in samples.chunks_exact(channels) {
            let mono = frame.iter().copied().sum::<f32>() / channels as f32;
            let source_index = self.source_frames;

            if let Some(previous) = self.previous_sample {
                while self.next_target_frame < MAX_AUDIO_DURATION_SECS * u64::from(TARGET_SAMPLE_RATE)
                {
                    let position = self.next_target_frame as f64
                        * f64::from(self.source_rate)
                        / f64::from(TARGET_SAMPLE_RATE);
                    if position > source_index as f64 {
                        break;
                    }
                    let fraction = (position - (source_index.saturating_sub(1)) as f64) as f32;
                    self.samples
                        .push(previous * (1.0 - fraction) + mono * fraction);
                    self.next_target_frame += 1;
                }
            }

            self.previous_sample = Some(mono);
            self.source_frames = self.source_frames.checked_add(1).ok_or_else(|| {
                AppError::AudioFileTooLong {
                    max_seconds: MAX_AUDIO_DURATION_SECS,
                }
            })?;
        }

        if self.source_frames > u64::from(self.source_rate) * MAX_AUDIO_DURATION_SECS {
            return Err(AppError::AudioFileTooLong {
                max_seconds: MAX_AUDIO_DURATION_SECS,
            });
        }
        Ok(())
    }

    fn finish(&mut self) {
        let Some(last_sample) = self.previous_sample else {
            return;
        };
        let max_samples = MAX_AUDIO_DURATION_SECS * u64::from(TARGET_SAMPLE_RATE);
        while self.next_target_frame < max_samples {
            let position = self.next_target_frame as f64 * f64::from(self.source_rate)
                / f64::from(TARGET_SAMPLE_RATE);
            if position >= self.source_frames as f64 {
                break;
            }
            self.samples.push(last_sample);
            self.next_target_frame += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn rejects_files_over_the_centralized_size_limit() {
        let mut file = NamedTempFile::with_suffix(".wav").unwrap();
        file.as_file_mut()
            .set_len(MAX_AUDIO_FILE_BYTES + 1)
            .unwrap();

        let error = decode_audio_file(file.path()).unwrap_err();

        assert!(matches!(error, AppError::AudioFileTooLarge { .. }));
    }

    #[test]
    fn rejects_duration_over_the_centralized_limit() {
        let error = validate_duration(
            u64::from(16_000u32) * (MAX_AUDIO_DURATION_SECS + 1),
            16_000,
        )
        .unwrap_err();

        assert!(matches!(error, AppError::AudioFileTooLong { .. }));
    }

    #[test]
    fn decodes_wav_to_mono_16khz() {
        let mut file = NamedTempFile::with_suffix(".wav").unwrap();
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file.as_file_mut(), spec).unwrap();
        for _ in 0..8_000 {
            writer.write_sample(16_384i16).unwrap();
            writer.write_sample(-16_384i16).unwrap();
        }
        writer.finalize().unwrap();
        file.as_file_mut().flush().unwrap();

        let audio = decode_audio_file(file.path()).unwrap();

        assert_eq!(audio.sample_rate, 16_000);
        assert_eq!(audio.channels, 1);
        assert_eq!(audio.samples.len(), 16_000);
        assert!(audio.samples.iter().all(|sample| sample.abs() < 1e-6));
    }
}
