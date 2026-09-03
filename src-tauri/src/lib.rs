mod capture;
mod audio_file;
mod clipboard;
mod config;
mod history;
mod model;
mod error;
mod gemini;
mod state;
mod tray;
mod transcription;

use crate::{
    audio_file::decode_audio_file,
    capture::CaptureSession,
    clipboard::{copy_text, type_text_on_main_thread},
    config::{normalize_shortcut, AppConfig, ConfigStore},
    error::{AppError, AppResult},
    gemini::GeminiProvider,
    history::HistoryStore,
    model::ParakeetModelManager,
    state::{AppSnapshot, AppStatus, ProcessOutcome, SettingsInput},
    tray::TrayIcons,
    transcription::ParakeetTranscriber,
};
use reqwest::Client;
use std::sync::{Mutex, RwLock};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri::tray::TrayIconBuilder;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const FALLBACK_SHORTCUT: &str = "F18";

pub struct AppRuntime {
    store: ConfigStore,
    config: RwLock<AppConfig>,
    runtime: Mutex<RuntimeState>,
    client: Client,
    model_manager: ParakeetModelManager,
    model_download_lock: tokio::sync::Mutex<()>,
    history: HistoryStore,
    tray_icons: TrayIcons,
}

struct RuntimeState {
    capture: Option<CaptureSession>,
    operation_active: bool,
    status: AppStatus,
    status_detail: String,
    last_transcript: Option<String>,
    last_output: Option<String>,
    last_error: Option<String>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            capture: None,
            operation_active: false,
            status: AppStatus::Idle,
            status_detail: "Ready to record".to_owned(),
            last_transcript: None,
            last_output: None,
            last_error: None,
        }
    }
}

impl AppRuntime {
    pub fn load() -> AppResult<Self> {
        let (store, config) = ConfigStore::load()?;
        let client = Client::builder()
            .user_agent("linda-listen/0.3.0")
            .build()?;
        let model_manager = ParakeetModelManager::load()?;
        let history = HistoryStore::new(store.config_dir())?;
        let tray_icons = TrayIcons::load()?;

        Ok(Self {
            store,
            config: RwLock::new(config),
            runtime: Mutex::new(RuntimeState::default()),
            client,
            model_manager,
            model_download_lock: tokio::sync::Mutex::new(()),
            history,
            tray_icons,
        })
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let config = self.config.read().unwrap().clone();
        let runtime = self.runtime.lock().unwrap();

        AppSnapshot {
            status: runtime.status.clone(),
            status_detail: runtime.status_detail.clone(),
            capture_active: runtime.capture.is_some(),
            config,
            api_key_present: self.store.read_api_key().map(|key| key.is_some()).unwrap_or(false),
            last_transcript: runtime.last_transcript.clone(),
            last_output: runtime.last_output.clone(),
            last_error: runtime.last_error.clone(),
        }
    }

    fn emit_snapshot(&self, app: &AppHandle) {
        if let Err(err) = app.emit("state-changed", self.snapshot()) {
            eprintln!("failed to emit state update: {err}");
        }
    }

    fn mark_status(&self, app: &AppHandle, status: AppStatus, detail: impl Into<String>) {
        let detail = detail.into();
        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.status = status;
            runtime.status_detail = detail;
            if !matches!(runtime.status, AppStatus::Error) {
                runtime.last_error = None;
            }
        }

        tray::update_tray(app, &self.tray_icons, status);
        self.emit_snapshot(app);
    }

    fn mark_error(&self, app: &AppHandle, detail: impl Into<String>) {
        let detail = detail.into();
        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.status = AppStatus::Error;
            runtime.status_detail = detail.clone();
            runtime.last_error = Some(detail);
        }

        tray::update_tray(app, &self.tray_icons, AppStatus::Error);
        self.emit_snapshot(app);
    }

    fn current_config(&self) -> AppConfig {
        self.config.read().unwrap().clone()
    }

    fn capture_active(&self) -> bool {
        self.runtime.lock().unwrap().capture.is_some()
    }

    async fn ensure_model_ready(&self, app: &AppHandle) -> AppResult<std::path::PathBuf> {
        let config = self.current_config();
        if self.model_manager.is_ready(&config).await? {
            return Ok(self.model_manager.resolved_model_dir(&config));
        }

        let _guard = self.model_download_lock.lock().await;
        let config = self.current_config();
        let model_dir = self.model_manager.resolved_model_dir(&config);

        if self.model_manager.is_ready(&config).await? {
            return Ok(model_dir);
        }

        self.mark_status(
            app,
            AppStatus::Downloading,
            format!("Downloading local speech model into {}", model_dir.display()),
        );

        match self.model_manager.ensure_ready(&self.client, &config).await {
            Ok(model_dir) => {
                self.mark_status(
                    app,
                    AppStatus::Ready,
                    format!("Local speech model ready in {}", model_dir.display()),
                );
                Ok(model_dir)
            }
            Err(err) => {
                self.mark_error(app, err.to_string());
                Err(err)
            }
        }
    }

    pub fn save_settings(
        &self,
        app: &AppHandle,
        input: SettingsInput,
    ) -> AppResult<AppSnapshot> {
        let mut config = self.current_config();
        let old_shortcut = config.shortcut.clone();

        config.gemini_enabled = input.gemini_enabled;
        if !input.gemini_model.trim().is_empty() {
            config.gemini_model = input.gemini_model.trim().to_owned();
        }
        if !input.gemini_prompt.trim().is_empty() {
            config.gemini_prompt = input.gemini_prompt.trim().to_owned();
        }
        config.whisper_model_path = input.whisper_model_path.trim().to_owned();
        config.whisper_language = input.whisper_language.trim().to_owned();
        if !input.shortcut.trim().is_empty() {
            config.shortcut = normalize_shortcut(input.shortcut.trim());
        }
        config.auto_paste = input.auto_paste;

        if let Some(api_key) = input.api_key.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            if let Err(err) = self.store.set_api_key(Some(api_key)) {
                self.mark_error(app, err.to_string());
                return Err(err);
            }
        }

        if let Err(err) = self.store.save_config(&config) {
            self.mark_error(app, err.to_string());
            return Err(err);
        }
        {
            let mut guard = self.config.write().unwrap();
            *guard = config.clone();
        }

        if old_shortcut != config.shortcut {
            if let Err(err) = self.rebind_shortcut(app) {
                self.mark_error(app, err.to_string());
                return Err(err);
            }
        }

        self.mark_status(app, AppStatus::Ready, "Settings saved");
        Ok(self.snapshot())
    }

    pub fn begin_capture(&self, app: &AppHandle) -> AppResult<()> {
        let runtime = self.runtime.lock().unwrap();
        if runtime.capture.is_some() || runtime.operation_active {
            let err = AppError::Message("capture is already active".to_owned());
            drop(runtime);
            self.mark_error(app, err.to_string());
            return Err(err);
        }
        drop(runtime);

        self.mark_status(app, AppStatus::Recording, "Opening microphone");
        let session = match CaptureSession::start() {
            Ok(session) => session,
            Err(err) => {
                self.mark_error(app, err.to_string());
                return Err(err);
            }
        };

        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.capture = Some(session);
            runtime.status = AppStatus::Recording;
            runtime.status_detail = "Recording microphone input".to_owned();
        }

        self.emit_snapshot(app);
        Ok(())
    }

    pub async fn finish_capture_and_process(
        &self,
        app: &AppHandle,
    ) -> AppResult<ProcessOutcome> {
        let capture = {
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.operation_active {
                let err = AppError::Message("another audio operation is already active".to_owned());
                drop(runtime);
                self.mark_error(app, err.to_string());
                return Err(err);
            }
            runtime.operation_active = true;
            runtime.capture.take()
        }
        .ok_or_else(|| {
            let err = AppError::Message("no active capture session".to_owned());
            self.runtime.lock().unwrap().operation_active = false;
            self.mark_error(app, err.to_string());
            err
        })?;

        let result = self.finish_capture_and_process_inner(app, capture).await;
        self.runtime.lock().unwrap().operation_active = false;
        result
    }

    async fn finish_capture_and_process_inner(
        &self,
        app: &AppHandle,
        capture: CaptureSession,
    ) -> AppResult<ProcessOutcome> {

        let audio = match capture.finish() {
            Ok(audio) => audio,
            Err(err) => {
                self.mark_error(app, err.to_string());
                return Err(err);
            }
        };
        let audio_for_history = audio.clone();

        let model_dir = match self.ensure_model_ready(app).await {
            Ok(model_dir) => model_dir,
            Err(err) => return Err(err),
        };

        self.mark_status(app, AppStatus::Transcribing, "Transcribing local speech");

        let transcript = match tauri::async_runtime::spawn_blocking({
            let model_dir = model_dir.clone();
            move || {
                let mut transcriber = ParakeetTranscriber::new(model_dir)?;
                transcriber.transcribe(&audio)
            }
        })
        .await
        {
            Ok(result) => match result {
                Ok(text) => text,
                Err(err) => {
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
            },
            Err(err) => {
                let app_err = AppError::Message(format!("transcription task failed: {err}"));
                self.mark_error(app, app_err.to_string());
                return Err(app_err);
            }
        };

        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.last_transcript = Some(transcript.clone());
        }
        self.emit_snapshot(app);

        let outcome = self.process_transcript(app, transcript).await?;

        let model_name = if outcome.used_gemini {
            Some(self.current_config().gemini_model.clone())
        } else {
            None
        };
        let _ = self.history.save_entry(
            &outcome.raw_transcript,
            if outcome.used_gemini { Some(&outcome.final_text) } else { None },
            model_name.as_deref(),
            Some(&audio_for_history),
        );

        if outcome.pasted {
            tray::reset_tray_idle(app, &self.tray_icons);
        }

        Ok(outcome)
    }

    pub async fn process_text(
        &self,
        app: &AppHandle,
        text: String,
    ) -> AppResult<ProcessOutcome> {
        {
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.capture.is_some() || runtime.operation_active {
                let err = AppError::Message("another audio operation is already active".to_owned());
                drop(runtime);
                self.mark_error(app, err.to_string());
                return Err(err);
            }
            runtime.operation_active = true;
        }

        let result = self.process_text_inner(app, text).await;
        self.runtime.lock().unwrap().operation_active = false;
        result
    }

    async fn process_text_inner(
        &self,
        app: &AppHandle,
        text: String,
    ) -> AppResult<ProcessOutcome> {
        let outcome = self.process_transcript(app, text).await?;

        let model_name = if outcome.used_gemini {
            Some(self.current_config().gemini_model.clone())
        } else {
            None
        };
        let _ = self.history.save_entry(
            &outcome.raw_transcript,
            if outcome.used_gemini { Some(&outcome.final_text) } else { None },
            model_name.as_deref(),
            None,
        );

        if outcome.pasted {
            tray::reset_tray_idle(app, &self.tray_icons);
        }

        Ok(outcome)
    }

    pub async fn import_audio(
        &self,
        app: &AppHandle,
        path: String,
    ) -> AppResult<ProcessOutcome> {
        {
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.capture.is_some() || runtime.operation_active {
                let err = AppError::Message("another audio operation is already active".to_owned());
                drop(runtime);
                self.mark_error(app, err.to_string());
                return Err(err);
            }
            runtime.operation_active = true;
        }

        let result = self.import_audio_inner(app, path).await;
        self.runtime.lock().unwrap().operation_active = false;
        result
    }

    async fn import_audio_inner(
        &self,
        app: &AppHandle,
        path: String,
    ) -> AppResult<ProcessOutcome> {
        self.mark_status(
            app,
            AppStatus::Transcribing,
            "Decoding and transcribing imported audio",
        );
        let audio = match tauri::async_runtime::spawn_blocking(move || {
            decode_audio_file(std::path::Path::new(&path))
        })
        .await
        {
            Ok(result) => match result {
                Ok(audio) => audio,
                Err(err) => {
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
            },
            Err(err) => {
                let app_err = AppError::Message(format!("audio decoding task failed: {err}"));
                self.mark_error(app, app_err.to_string());
                return Err(app_err);
            }
        };

        let model_dir = self.ensure_model_ready(app).await?;
        self.mark_status(app, AppStatus::Transcribing, "Transcribing imported audio locally");
        let transcript = match tauri::async_runtime::spawn_blocking({
            let model_dir = model_dir.clone();
            move || {
                let mut transcriber = ParakeetTranscriber::new(model_dir)?;
                transcriber.transcribe(&audio)
            }
        })
        .await
        {
            Ok(result) => match result {
                Ok(text) => text,
                Err(err) => {
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
            },
            Err(err) => {
                let app_err = AppError::Message(format!("transcription task failed: {err}"));
                self.mark_error(app, app_err.to_string());
                return Err(app_err);
            }
        };

        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.last_transcript = Some(transcript.clone());
        }
        self.emit_snapshot(app);
        let outcome = self.process_transcript(app, transcript).await?;

        let model_name = if outcome.used_gemini {
            Some(self.current_config().gemini_model.clone())
        } else {
            None
        };
        if let Err(err) = self.history.save_entry(
            &outcome.raw_transcript,
            if outcome.used_gemini { Some(&outcome.final_text) } else { None },
            model_name.as_deref(),
            None,
        ) {
            self.mark_error(app, err.to_string());
            return Err(err);
        }

        if outcome.pasted {
            tray::reset_tray_idle(app, &self.tray_icons);
        }
        Ok(outcome)
    }

    async fn process_transcript(
        &self,
        app: &AppHandle,
        transcript: String,
    ) -> AppResult<ProcessOutcome> {
        let transcript = transcript.trim().to_owned();
        if transcript.is_empty() {
            let err = AppError::Message("no text was provided".to_owned());
            self.mark_error(app, err.to_string());
            return Err(err);
        }

        let config = self.current_config();
        let mut final_text = transcript.clone();
        let mut used_gemini = false;

        if config.gemini_enabled {
            let api_key = match self.store.read_api_key() {
                Ok(Some(api_key)) => api_key,
                Ok(None) => {
                    let err = AppError::MissingGeminiApiKey;
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
                Err(err) => {
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
            };

            self.mark_status(
                app,
                AppStatus::Rewriting,
                format!("Rewriting with {}", config.gemini_model),
            );

            let provider = GeminiProvider::new(
                self.client.clone(),
                api_key,
                config.gemini_model.clone(),
                config.gemini_prompt.clone(),
            );

            final_text = match provider.rewrite(&transcript).await {
                Ok(text) => {
                    used_gemini = true;
                    text
                }
                Err(err) => {
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
            };
        }

        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.last_output = Some(final_text.clone());
        }

        let mut pasted = false;
        let mut paste_skipped = false;
        let mut paste_error: Option<String> = None;
        let mut clipboard_updated = false;

        if config.auto_paste {
            if main_window_is_focused(app) {
                // Window is focused — copy to clipboard for manual paste instead.
                self.mark_status(app, AppStatus::Copying, "Copying text to clipboard");
                if let Err(err) = copy_text(&final_text) {
                    self.mark_error(app, err.to_string());
                    return Err(err);
                }
                clipboard_updated = true;
                paste_skipped = true;
                self.mark_status(
                    app,
                    AppStatus::Ready,
                    "Copied the transcript to the clipboard; paste skipped while the app is open",
                );
            } else {
                // Type text directly into the frontmost app without touching the clipboard.
                self.mark_status(app, AppStatus::Pasting, "Typing into the active app");
                if let Err(err) = type_text_on_main_thread(app, final_text.clone()).await {
                    // Fall back to clipboard so the transcript is not lost.
                    if copy_text(&final_text).is_ok() {
                        clipboard_updated = true;
                    }
                    let message = format!(
                        "Auto-type failed, copied to clipboard instead: {}. On macOS, grant Accessibility permission to Linda Listen and allow it to control System Events if prompted.",
                        err
                    );
                    eprintln!("auto-type failed: {err}");
                    paste_error = Some(message);
                } else {
                    pasted = true;
                }
            }
        } else {
            // Auto-paste disabled — just copy to clipboard.
            self.mark_status(app, AppStatus::Copying, "Copying text to clipboard");
            if let Err(err) = copy_text(&final_text) {
                self.mark_error(app, err.to_string());
                return Err(err);
            }
            clipboard_updated = true;
        }

        {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.status = AppStatus::Ready;
            runtime.status_detail = if pasted {
                "Typed the transcript into the active app".to_owned()
            } else if let Some(message) = paste_error.clone() {
                message
            } else if paste_skipped {
                "Copied the transcript to the clipboard; paste skipped while the app is open".to_owned()
            } else {
                "Copied the transcript to the clipboard".to_owned()
            };
            runtime.last_error = paste_error;
        }
        self.emit_snapshot(app);

        Ok(ProcessOutcome {
            raw_transcript: transcript,
            final_text,
            used_gemini,
            clipboard_updated,
            pasted,
        })
    }

    fn rebind_shortcut(&self, app: &AppHandle) -> AppResult<()> {
        let shortcut = normalize_shortcut(&self.current_config().shortcut);
        if shortcut.trim().is_empty() {
            return Err(AppError::Message("shortcut cannot be empty".to_owned()));
        }

        if let Err(err) = app.global_shortcut().unregister_all() {
            eprintln!("failed to clear existing shortcuts: {err}");
        }

        register_shortcut(app, shortcut.as_str())?;
        Ok(())
    }
}

fn bind_shortcut(app: &AppHandle, shortcut: &str) -> AppResult<()> {
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _shortcut, event| {
            let event_state = event.state;
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let runtime = app.state::<AppRuntime>();
                let result = if event_state == ShortcutState::Pressed {
                    runtime.begin_capture(&app)
                } else if event_state == ShortcutState::Released {
                    if runtime.capture_active() {
                        runtime.finish_capture_and_process(&app).await.map(|_| ())
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                };

                if let Err(err) = result {
                    eprintln!("shortcut handler error: {err}");
                }
            });
        })
        .map_err(|err| AppError::Message(err.to_string()))?;

    Ok(())
}

fn register_shortcut(app: &AppHandle, shortcut: &str) -> AppResult<()> {
    let shortcut = normalize_shortcut(shortcut);
    if shortcut.trim().is_empty() {
        return Err(AppError::Message("shortcut cannot be empty".to_owned()));
    }

    let primary_error = match bind_shortcut(app, shortcut.as_str()) {
        Ok(()) => None,
        Err(err) => {
            eprintln!("failed to register shortcut `{shortcut}`: {err}");
            Some(err)
        }
    };

    let fallback_ok = if shortcut != FALLBACK_SHORTCUT {
        match bind_shortcut(app, FALLBACK_SHORTCUT) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("failed to register fallback shortcut `{FALLBACK_SHORTCUT}`: {err}");
                false
            }
        }
    } else {
        primary_error.is_none()
    };

    if primary_error.is_none() || fallback_ok {
        Ok(())
    } else {
        Err(primary_error.unwrap())
    }
}

fn main_window_is_focused(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_focused().ok())
        .unwrap_or(false)
}

#[tauri::command]
fn get_snapshot(state: State<AppRuntime>) -> AppSnapshot {
    state.snapshot()
}

#[tauri::command]
fn get_config_path(state: State<AppRuntime>) -> String {
    state.store.config_path().to_string_lossy().to_string()
}

#[tauri::command]
async fn save_settings(
    app: AppHandle,
    state: State<'_, AppRuntime>,
    input: SettingsInput,
) -> Result<AppSnapshot, String> {
    state.save_settings(&app, input).map_err(|err| err.to_string())
}

#[tauri::command]
fn start_capture(app: AppHandle, state: State<'_, AppRuntime>) -> Result<AppSnapshot, String> {
    state
        .begin_capture(&app)
        .map(|_| state.snapshot())
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn stop_capture(
    app: AppHandle,
    state: State<'_, AppRuntime>,
) -> Result<ProcessOutcome, String> {
    state
        .finish_capture_and_process(&app)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn process_text(
    app: AppHandle,
    state: State<'_, AppRuntime>,
    text: String,
) -> Result<ProcessOutcome, String> {
    state
        .process_text(&app, text)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn import_audio(
    app: AppHandle,
    state: State<'_, AppRuntime>,
    path: String,
) -> Result<ProcessOutcome, String> {
    state
        .import_audio(&app, path)
        .await
        .map_err(|err| err.to_string())
}

#[derive(serde::Serialize, Clone)]
struct HistoryEntryInfo {
    timestamp: String,
    preview: String,
    has_audio: bool,
}

#[tauri::command]
fn list_history(state: State<AppRuntime>) -> Result<Vec<HistoryEntryInfo>, String> {
    state
        .history
        .list_entries()
        .map(|entries| {
            entries
                .into_iter()
                .map(|e| HistoryEntryInfo {
                    timestamp: e.timestamp,
                    preview: e.preview,
                    has_audio: e.has_audio,
                })
                .collect()
        })
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn open_history_folder(app: AppHandle, state: State<AppRuntime>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = state.history.dir().to_string_lossy().to_string();
    app.opener()
        .open_path(&dir, None::<&str>)
        .map_err(|err| err.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let runtime = AppRuntime::load()?;
            let shortcut = runtime.snapshot().config.shortcut.clone();
            app.manage(runtime);

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let runtime = app_handle.state::<AppRuntime>();
                if let Err(err) = runtime.ensure_model_ready(&app_handle).await {
                    eprintln!("failed to prepare local speech model: {err}");
                }
            });

            register_shortcut(&app.handle(), shortcut.as_str())?;

            // --- System tray ---
            let show_item = tauri::menu::MenuItem::with_id(
                app, "show_window", "Show Window", true, None::<&str>,
            )?;
            let quit_item = tauri::menu::MenuItem::with_id(
                app, "quit", "Quit", true, None::<&str>,
            )?;
            let tray_menu = tauri::menu::MenuBuilder::new(app)
                .item(&show_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let tray_icon_image = tauri::image::Image::from_bytes(
                include_bytes!("../icons/tray-icon.png"),
            )?;

            TrayIconBuilder::with_id(tray::TRAY_ID)
                .icon(tray_icon_image)
                .icon_as_template(true)
                .menu(&tray_menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show_window" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // Hide window on close instead of quitting
            if let Some(window) = app.get_webview_window("main") {
                let app_handle_close = app.handle().clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Some(win) = app_handle_close.get_webview_window("main") {
                            let _ = win.hide();
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            get_config_path,
            save_settings,
            start_capture,
            stop_capture,
            process_text,
            import_audio,
            list_history,
            open_history_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running linda-listen");
}
