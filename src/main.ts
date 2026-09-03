import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

type AppStatus =
  | "idle"
  | "recording"
  | "downloading"
  | "transcribing"
  | "rewriting"
  | "copying"
  | "pasting"
  | "ready"
  | "error";

interface AppConfig {
  gemini_enabled: boolean;
  gemini_model: string;
  gemini_prompt: string;
  whisper_model_path: string;
  whisper_language: string;
  shortcut: string;
  auto_paste: boolean;
}

interface AppSnapshot {
  status: AppStatus;
  status_detail: string;
  capture_active: boolean;
  config: AppConfig;
  api_key_present: boolean;
  last_transcript: string | null;
  last_output: string | null;
  last_error: string | null;
}

interface SettingsInput {
  gemini_enabled: boolean;
  gemini_model: string;
  gemini_prompt: string;
  whisper_model_path: string;
  whisper_language: string;
  shortcut: string;
  auto_paste: boolean;
  api_key: string | null;
}

interface ProcessOutcome {
  raw_transcript: string;
  final_text: string;
  used_gemini: boolean;
  clipboard_updated: boolean;
  pasted: boolean;
}

interface HistoryEntryInfo {
  timestamp: string;
  preview: string;
  has_audio: boolean;
}

const brandMarkUrl = new URL("./assets/linda-listen-logo.svg", import.meta.url).href;

const STATUS_LABELS: Record<AppStatus, string> = {
  idle: "Idle",
  recording: "Recording",
  downloading: "Downloading",
  transcribing: "Transcribing",
  rewriting: "Rewriting",
  copying: "Copying",
  pasting: "Pasting",
  ready: "Ready",
  error: "Error",
};

const elements = {
  brandMark: document.querySelector<HTMLImageElement>("#brand-mark"),
  statusPill: document.querySelector<HTMLSpanElement>("#status-pill"),
  statusDetail: document.querySelector<HTMLParagraphElement>("#status-detail"),
  statusShortcut: document.querySelector<HTMLParagraphElement>("#status-shortcut"),
  configPath: document.querySelector<HTMLParagraphElement>("#config-path"),
  apiKeyState: document.querySelector<HTMLParagraphElement>("#api-key-state"),
  manualInput: document.querySelector<HTMLTextAreaElement>("#manual-input"),
  geminiEnabled: document.querySelector<HTMLInputElement>("#gemini-enabled"),
  apiKey: document.querySelector<HTMLInputElement>("#api-key"),
  geminiModel: document.querySelector<HTMLInputElement>("#gemini-model"),
  geminiPrompt: document.querySelector<HTMLTextAreaElement>("#gemini-prompt"),
  whisperModelPath: document.querySelector<HTMLInputElement>("#whisper-model-path"),
  whisperLanguage: document.querySelector<HTMLInputElement>("#whisper-language"),
  shortcut: document.querySelector<HTMLInputElement>("#shortcut"),
  autoPaste: document.querySelector<HTMLInputElement>("#auto-paste"),
  lastTranscript: document.querySelector<HTMLPreElement>("#last-transcript"),
  lastOutput: document.querySelector<HTMLPreElement>("#last-output"),
  lastError: document.querySelector<HTMLPreElement>("#last-error"),
  settingsForm: document.querySelector<HTMLFormElement>("#settings-form"),
  saveSettings: document.querySelector<HTMLButtonElement>("#save-settings"),
  startCapture: document.querySelector<HTMLButtonElement>("#start-capture"),
  stopCapture: document.querySelector<HTMLButtonElement>("#stop-capture"),
  processText: document.querySelector<HTMLButtonElement>("#process-text"),
  importAudio: document.querySelector<HTMLButtonElement>("#import-audio"),
  historyList: document.querySelector<HTMLDivElement>("#history-list"),
  openHistory: document.querySelector<HTMLButtonElement>("#open-history"),
} as const;

let configPath = "—";

function requireElement<T extends HTMLElement>(element: T | null, selector: string): T {
  if (!element) {
    throw new Error(`Missing required element: ${selector}`);
  }
  return element;
}

function snapshotToSettings(): SettingsInput {
  const apiKey = requireElement(elements.apiKey, "#api-key").value.trim();
  return {
    gemini_enabled: requireElement(elements.geminiEnabled, "#gemini-enabled").checked,
    gemini_model: requireElement(elements.geminiModel, "#gemini-model").value.trim(),
    gemini_prompt: requireElement(elements.geminiPrompt, "#gemini-prompt").value.trim(),
    whisper_model_path: requireElement(elements.whisperModelPath, "#whisper-model-path").value.trim(),
    whisper_language: requireElement(elements.whisperLanguage, "#whisper-language").value.trim(),
    shortcut: requireElement(elements.shortcut, "#shortcut").value.trim(),
    auto_paste: requireElement(elements.autoPaste, "#auto-paste").checked,
    api_key: apiKey.length > 0 ? apiKey : null,
  };
}

function setTextContent(
  element: HTMLElement | null,
  value: string,
) {
  if (element) {
    element.textContent = value;
  }
}

function renderSettings(snapshot: AppSnapshot) {
  requireElement(elements.geminiEnabled, "#gemini-enabled").checked = snapshot.config.gemini_enabled;
  requireElement(elements.geminiModel, "#gemini-model").value = snapshot.config.gemini_model;
  requireElement(elements.geminiPrompt, "#gemini-prompt").value = snapshot.config.gemini_prompt;
  requireElement(elements.whisperModelPath, "#whisper-model-path").value =
    snapshot.config.whisper_model_path;
  requireElement(elements.whisperLanguage, "#whisper-language").value =
    snapshot.config.whisper_language;
  requireElement(elements.shortcut, "#shortcut").value = snapshot.config.shortcut;
  requireElement(elements.autoPaste, "#auto-paste").checked = snapshot.config.auto_paste;
  requireElement(elements.apiKey, "#api-key").value = "";

  const modelMessage = "Speech model assets download automatically into app storage.";
  setTextContent(
    elements.apiKeyState,
    snapshot.config.gemini_enabled
      ? snapshot.api_key_present
        ? `Gemini cleanup is enabled and the key is stored in Keychain. ${modelMessage}`
        : `Gemini cleanup is enabled, but no key is saved. ${modelMessage}`
      : modelMessage,
  );
}

function renderRuntime(snapshot: AppSnapshot) {
  const statusPill = requireElement(elements.statusPill, "#status-pill");
  statusPill.textContent = STATUS_LABELS[snapshot.status];
  statusPill.className = `status-pill status-pill--${snapshot.status}`;

  setTextContent(elements.statusDetail, snapshot.status_detail);
  setTextContent(
    elements.statusShortcut,
    snapshot.config.shortcut === "F18"
      ? `Hotkey: ${snapshot.config.shortcut}`
      : `Hotkey: ${snapshot.config.shortcut} (F18 fallback)`,
  );
  setTextContent(elements.configPath, `Config: ${configPath}`);

  setTextContent(
    elements.lastTranscript,
    snapshot.last_transcript && snapshot.last_transcript.trim().length > 0
      ? snapshot.last_transcript
      : "—",
  );
  setTextContent(
    elements.lastOutput,
    snapshot.last_output && snapshot.last_output.trim().length > 0 ? snapshot.last_output : "—",
  );
  setTextContent(
    elements.lastError,
    snapshot.last_error && snapshot.last_error.trim().length > 0 ? snapshot.last_error : "—",
  );
}

function renderOutcome(outcome: ProcessOutcome) {
  setTextContent(
    elements.lastTranscript,
    outcome.raw_transcript.trim().length > 0 ? outcome.raw_transcript : "—",
  );
  setTextContent(
    elements.lastOutput,
    outcome.final_text.trim().length > 0 ? outcome.final_text : "—",
  );
}

function formatTimestamp(nanos: string): string {
  const ms = Number(BigInt(nanos) / BigInt(1_000_000));
  const date = new Date(ms);
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function renderHistory(entries: HistoryEntryInfo[]) {
  const container = elements.historyList;
  if (!container) return;

  if (entries.length === 0) {
    container.innerHTML = `<p class="history-empty">No history yet.</p>`;
    return;
  }

  container.innerHTML = entries
    .map((entry) => {
      const time = formatTimestamp(entry.timestamp);
      const audioBadge = entry.has_audio
        ? `<span class="badge-tiny">🎤 audio</span>`
        : "";
      const preview = entry.preview || "(empty)";
      return `<div class="history-item">
        <div class="history-item-meta">
          <span>${time}</span>
          ${audioBadge}
        </div>
        <div class="history-item-preview">${escapeHtml(preview)}</div>
      </div>`;
    })
    .join("");
}

function escapeHtml(text: string): string {
  const div = document.createElement("div");
  div.textContent = text;
  return div.innerHTML;
}

async function refreshHistory() {
  try {
    const entries = await invoke<HistoryEntryInfo[]>("list_history");
    renderHistory(entries);
  } catch {
    // silently ignore
  }
}

function setButtonsDisabled(disabled: boolean) {
  requireElement(elements.saveSettings, "#save-settings").disabled = disabled;
  requireElement(elements.startCapture, "#start-capture").disabled = disabled;
  requireElement(elements.stopCapture, "#stop-capture").disabled = disabled;
  requireElement(elements.processText, "#process-text").disabled = disabled;
  requireElement(elements.importAudio, "#import-audio").disabled = disabled;
}

async function refreshSnapshot() {
  const snapshot = await invoke<AppSnapshot>("get_snapshot");
  renderRuntime(snapshot);
}

async function loadInitialState() {
  configPath = "App-managed storage";
  const snapshot = await invoke<AppSnapshot>("get_snapshot");
  renderSettings(snapshot);
  renderRuntime(snapshot);
  await refreshHistory();
}

async function saveSettings() {
  const input = snapshotToSettings();
  const snapshot = await invoke<AppSnapshot>("save_settings", { input });
  renderSettings(snapshot);
  renderRuntime(snapshot);
}

async function startCapture() {
  const snapshot = await invoke<AppSnapshot>("start_capture");
  renderRuntime(snapshot);
}

async function stopCapture() {
  const outcome = await invoke<ProcessOutcome>("stop_capture");
  renderOutcome(outcome);
  await refreshSnapshot();
  await refreshHistory();
}

async function processManualText() {
  const manualText = requireElement(elements.manualInput, "#manual-input").value.trim();
  if (manualText.length === 0) {
    setTextContent(elements.lastError, "Type or paste text before processing it.");
    return;
  }

  const outcome = await invoke<ProcessOutcome>("process_text", { text: manualText });
  renderOutcome(outcome);
  await refreshSnapshot();
  await refreshHistory();
}

async function importAudio() {
  const selected = await open({
    multiple: false,
    directory: false,
    filters: [
      {
        name: "Audio files",
        extensions: ["wav", "mp3", "m4a", "aac", "flac", "ogg", "opus"],
      },
    ],
  });
  if (typeof selected !== "string") {
    return;
  }

  const outcome = await invoke<ProcessOutcome>("import_audio", { path: selected });
  renderOutcome(outcome);
  await refreshSnapshot();
  await refreshHistory();
}

document.addEventListener("DOMContentLoaded", async () => {
  requireElement(elements.brandMark, "#brand-mark").src = brandMarkUrl;
  const startButton = requireElement(elements.startCapture, "#start-capture");
  const stopButton = requireElement(elements.stopCapture, "#stop-capture");
  const processButton = requireElement(elements.processText, "#process-text");
  const importButton = requireElement(elements.importAudio, "#import-audio");
  const openHistoryButton = requireElement(elements.openHistory, "#open-history");
  const form = requireElement(elements.settingsForm, "#settings-form");

  await loadInitialState();

  void listen<AppSnapshot>("state-changed", ({ payload }) => {
    renderRuntime(payload);
  });

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    setButtonsDisabled(true);
    try {
      await saveSettings();
    } catch (error) {
      setTextContent(elements.lastError, String(error));
      await refreshSnapshot();
    } finally {
      setButtonsDisabled(false);
    }
  });

  startButton.addEventListener("click", async () => {
    setButtonsDisabled(true);
    try {
      await startCapture();
    } catch (error) {
      setTextContent(elements.lastError, String(error));
      await refreshSnapshot();
    } finally {
      setButtonsDisabled(false);
    }
  });

  stopButton.addEventListener("click", async () => {
    setButtonsDisabled(true);
    try {
      await stopCapture();
    } catch (error) {
      setTextContent(elements.lastError, String(error));
      await refreshSnapshot();
    } finally {
      setButtonsDisabled(false);
    }
  });

  processButton.addEventListener("click", async () => {
    setButtonsDisabled(true);
    try {
      await processManualText();
    } catch (error) {
      setTextContent(elements.lastError, String(error));
      await refreshSnapshot();
    } finally {
      setButtonsDisabled(false);
    }
  });

  importButton.addEventListener("click", async () => {
    setButtonsDisabled(true);
    try {
      await importAudio();
    } catch (error) {
      setTextContent(elements.lastError, String(error));
      await refreshSnapshot();
    } finally {
      setButtonsDisabled(false);
    }
  });

  openHistoryButton.addEventListener("click", async () => {
    try {
      await invoke("open_history_folder");
    } catch (error) {
      setTextContent(elements.lastError, String(error));
    }
  });
});
