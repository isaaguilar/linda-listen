use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("application data directory is unavailable")]
    ConfigDirUnavailable,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("keyring error: {0}")]
    Keyring(#[from] keyring_core::Error),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Gemini request error: {0}")]
    GeminiRequest(String),
    #[error("missing Gemini API key")]
    MissingGeminiApiKey,
    #[error("no microphone input device is available")]
    NoInputDevice,
    #[error("audio capture error: {0}")]
    AudioCapture(String),
    #[error("transcription model not found at {0}")]
    MissingModel(PathBuf),
    #[error("transcription error: {0}")]
    Transcription(String),
    #[error("clipboard error: {0}")]
    Clipboard(#[from] arboard::Error),
    #[error("automation error: {0}")]
    Automation(String),
    #[error("cpal default stream config error: {0}")]
    CpalDefaultStreamConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal build stream error: {0}")]
    CpalBuildStream(#[from] cpal::BuildStreamError),
    #[error("cpal play stream error: {0}")]
    CpalPlayStream(#[from] cpal::PlayStreamError),
    #[error("audio encoding error: {0}")]
    AudioEncoding(#[from] hound::Error),
    #[error("audio file exceeds the {max_bytes} byte limit")]
    AudioFileTooLarge { max_bytes: u64 },
    #[error("audio file exceeds the {max_seconds} second duration limit")]
    AudioFileTooLong { max_seconds: u64 },
    #[error("unsupported audio format: {0}")]
    UnsupportedAudioFormat(String),
    #[error("audio decoding error: {0}")]
    AudioDecoding(String),
    #[error("{0}")]
    Message(String),
}

pub type AppResult<T> = Result<T, AppError>;
