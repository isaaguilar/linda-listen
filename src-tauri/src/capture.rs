use crate::error::{AppError, AppResult};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SampleFormat, SizedSample, Stream,
};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct CapturedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

pub struct CaptureSession {
    stream: Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    error_slot: Arc<Mutex<Option<String>>>,
    sample_rate: u32,
    channels: u16,
}

impl CaptureSession {
    pub fn start() -> AppResult<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(AppError::NoInputDevice)?;
        let supported_config = device.default_input_config()?;
        let sample_rate = supported_config.sample_rate();
        let channels = supported_config.channels();
        let stream_config = supported_config.config();
        let samples = Arc::new(Mutex::new(Vec::new()));
        let error_slot = Arc::new(Mutex::new(None));

        let stream = match supported_config.sample_format() {
            SampleFormat::F32 => {
                build_stream::<f32>(&device, &stream_config, samples.clone(), error_slot.clone())?
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, &stream_config, samples.clone(), error_slot.clone())?
            }
            SampleFormat::U16 => {
                build_stream::<u16>(&device, &stream_config, samples.clone(), error_slot.clone())?
            }
            other => {
                return Err(AppError::AudioCapture(format!(
                    "unsupported input sample format: {other:?}"
                )))
            }
        };

        stream.play()?;

        Ok(Self {
            stream,
            samples,
            error_slot,
            sample_rate,
            channels,
        })
    }

    pub fn finish(self) -> AppResult<CapturedAudio> {
        let CaptureSession {
            stream,
            samples,
            error_slot,
            sample_rate,
            channels,
        } = self;

        drop(stream);

        if let Some(message) = error_slot.lock().ok().and_then(|slot| slot.clone()) {
            return Err(AppError::AudioCapture(message));
        }

        let samples = Arc::try_unwrap(samples)
            .map_err(|_| AppError::AudioCapture("recording buffer is still in use".to_owned()))?
            .into_inner()
            .map_err(|_| AppError::AudioCapture("recording buffer was poisoned".to_owned()))?;

        if samples.is_empty() {
            return Err(AppError::AudioCapture(
                "no microphone samples were captured".to_owned(),
            ));
        }

        Ok(CapturedAudio {
            samples,
            sample_rate,
            channels,
        })
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    error_slot: Arc<Mutex<Option<String>>>,
) -> AppResult<Stream>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let err_slot = error_slot.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            if let Ok(mut buffer) = samples.lock() {
                buffer.extend(data.iter().map(|sample| f32::from_sample(*sample)));
            }
        },
        move |err| {
            let message = err.to_string();
            eprintln!("audio input stream error: {message}");
            if let Ok(mut slot) = err_slot.lock() {
                *slot = Some(message);
            }
        },
        None,
    )?;

    Ok(stream)
}
