use crate::error::{AppError, AppResult};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::{process::Command, thread, time::Duration};
use tauri::AppHandle;

pub fn copy_text(text: &str) -> AppResult<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_owned())?;
    Ok(())
}

#[allow(dead_code)]
fn paste_clipboard_impl() -> AppResult<()> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|err| AppError::Automation(err.to_string()))?;
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|err| AppError::Automation(err.to_string()))?;
    enigo
        .key(Key::Unicode('v'), Direction::Click)
        .map_err(|err| AppError::Automation(err.to_string()))?;
    enigo
        .key(Key::Meta, Direction::Release)
        .map_err(|err| AppError::Automation(err.to_string()))?;

    Ok(())
}

fn is_simulate_input_permission_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("permission to simulate input")
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn paste_clipboard_with_osascript() -> AppResult<()> {
    let output = Command::new("osascript")
        .args([
            "-e",
            r#"tell application "System Events" to keystroke "v" using command down"#,
        ])
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };

    Err(AppError::Automation(format!("AppleScript paste failed: {detail}")))
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn paste_clipboard_with_osascript() -> AppResult<()> {
    Err(AppError::Automation(
        "AppleScript paste fallback is only available on macOS".to_owned(),
    ))
}

#[allow(dead_code)]
pub async fn paste_clipboard_on_main_thread(app: &AppHandle) -> AppResult<()> {
    thread::sleep(Duration::from_millis(80));

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    app.run_on_main_thread(move || {
        let result = paste_clipboard_impl().map_err(|err| err.to_string());
        let _ = tx.send(result);
    })
    .map_err(|err| AppError::Message(err.to_string()))?;

    match rx
        .await
        .map_err(|_| AppError::Message("failed to receive paste result".to_owned()))?
    {
        Ok(()) => Ok(()),
        Err(message) if is_simulate_input_permission_error(&message) => paste_clipboard_with_osascript(),
        Err(message) => Err(AppError::Message(message)),
    }
}

/// Inject text directly into the frontmost app using CGEvent-based keyboard
/// events. This never touches the system clipboard.
fn type_text_impl(text: &str) -> AppResult<()> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|err| AppError::Automation(err.to_string()))?;
    enigo
        .text(text)
        .map_err(|err| AppError::Automation(err.to_string()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn type_text_with_osascript(text: &str) -> AppResult<()> {
    // Build an AppleScript that handles newlines and tabs as key codes
    // while typing everything else with keystroke.
    let mut parts: Vec<String> = Vec::new();
    for ch in text.chars() {
        match ch {
            '\n' | '\r' => parts.push("key code 36".to_owned()), // Return
            '\t' => parts.push("key code 48".to_owned()),        // Tab
            _ => {
                let escaped = ch.to_string().replace('\\', "\\\\").replace('"', "\\\"");
                parts.push(format!("keystroke \"{escaped}\""));
            }
        }
    }
    let body = parts.join("\n");
    let script = format!("tell application \"System Events\"\n{body}\nend tell");
    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };

    Err(AppError::Automation(format!("AppleScript type text failed: {detail}")))
}

#[cfg(not(target_os = "macos"))]
fn type_text_with_osascript(_text: &str) -> AppResult<()> {
    Err(AppError::Automation(
        "AppleScript type text fallback is only available on macOS".to_owned(),
    ))
}

/// Type text directly into the frontmost app without using the clipboard.
/// Uses enigo CGEvent injection as the primary method and falls back to
/// AppleScript keystroke if the Accessibility permission is missing.
pub async fn type_text_on_main_thread(app: &AppHandle, text: String) -> AppResult<()> {
    thread::sleep(Duration::from_millis(80));

    let text_for_fallback = text.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    app.run_on_main_thread(move || {
        let result = type_text_impl(&text).map_err(|err| err.to_string());
        let _ = tx.send(result);
    })
    .map_err(|err| AppError::Message(err.to_string()))?;

    match rx
        .await
        .map_err(|_| AppError::Message("failed to receive type text result".to_owned()))?
    {
        Ok(()) => Ok(()),
        Err(message) if is_simulate_input_permission_error(&message) => {
            type_text_with_osascript(&text_for_fallback)
        }
        Err(message) => Err(AppError::Message(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::is_simulate_input_permission_error;

    #[test]
    fn detects_macos_input_simulation_permission_errors() {
        assert!(is_simulate_input_permission_error(
            "The application does not have the permission to simulate input!"
        ));
        assert!(!is_simulate_input_permission_error(
            "something unrelated happened"
        ));
    }

    #[test]
    fn osascript_type_text_escapes_quotes_and_backslashes() {
        // The per-character escaping handles quotes and backslashes.
        let ch = '"';
        let escaped = ch.to_string().replace('\\', "\\\\").replace('"', "\\\"");
        assert_eq!(escaped, "\\\"");

        let ch2 = '\\';
        let escaped2 = ch2.to_string().replace('\\', "\\\\").replace('"', "\\\"");
        assert_eq!(escaped2, "\\\\");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn type_text_osascript_unavailable_off_macos() {
        let result = super::type_text_with_osascript("hello");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("only available on macOS"), "unexpected error: {msg}");
    }
}
