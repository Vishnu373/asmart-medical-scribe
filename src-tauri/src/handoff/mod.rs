//! Global-shortcut no-activate overlay and clipboard paste of SOAP sections. Design §8.6. B11.
//!
//! `parser` is the pure, unit-tested deterministic SOAP splitter. This file is the
//! native glue (verified on Windows, like the rest of the native stack): the
//! `paste_section` command — active note → section body → clipboard → simulated
//! Ctrl+V → timed clipboard clear — and the global-hotkey registration that asks
//! the overlay window (F7) to show itself. The picker UI lives in the frontend; the
//! backend only delivers the keystroke and the text.

mod parser;

pub use parser::SoapSection;

use std::time::Duration;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::store::SharedStore;

/// How long a pasted section lingers on the clipboard before auto-clear (§8.6
/// "clipboard hygiene"): long enough for the paste to land, short enough to limit
/// PHI exposure in the globally-readable buffer.
const CLIPBOARD_CLEAR_DELAY: Duration = Duration::from_secs(15);

/// Paste one SOAP section of the record's current active note into the focused EMR
/// field (§8.6). Always reads the *active* version so edits/regenerations are
/// reflected. Rejects an unknown section or an empty body (nothing to paste).
#[tauri::command]
pub fn paste_section(
    app: AppHandle,
    store: State<'_, SharedStore>,
    record_id: String,
    section: String,
) -> Result<(), String> {
    let section = SoapSection::from_key(&section)
        .ok_or_else(|| format!("unknown SOAP section: {section}"))?;
    let note = store
        .lock()
        .active_note(&record_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("record {record_id} has no active note"))?;
    let text = parser::section_body(&note.soap_data, section);
    if text.is_empty() {
        return Err(format!("the {} section is empty", section.key()));
    }
    paste_text(&app, text)
}

/// Put `text` on the clipboard, deliver Ctrl+V into the focused field, and schedule
/// the clipboard to self-clear (§8.6).
fn paste_text(app: &AppHandle, text: String) -> Result<(), String> {
    app.clipboard()
        .write_text(text.clone())
        .map_err(|e| e.to_string())?;
    // Schedule the auto-clear *before* sending the keystroke: once PHI is on the
    // clipboard the wipe must be guaranteed, even if SendInput fails (e.g. UIPI
    // blocks it when the EMR runs at a higher integrity level). Otherwise the
    // section would sit on the globally-readable clipboard with no timer (§8.6).
    schedule_clipboard_clear(app, text);
    send_paste_keystroke()
}

/// After the delay, clear the clipboard — but only if it still holds the section we
/// pasted, so we never stomp on something the clinician copied since.
fn schedule_clipboard_clear(app: &AppHandle, pasted: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(CLIPBOARD_CLEAR_DELAY);
        if let Ok(current) = app.clipboard().read_text() {
            if current == pasted {
                let _ = app.clipboard().write_text(String::new());
            }
        }
    });
}

/// Register the rebindable paste hotkey (default Alt+P, §8.6). On press it emits
/// `handoff-requested`; the no-activate overlay window (F7) listens and shows its
/// S/O/A/P picker without stealing the EMR field's focus.
pub fn register_paste_hotkey(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let shortcut: tauri_plugin_global_shortcut::Shortcut = accelerator
        .parse()
        .map_err(|_| format!("invalid paste hotkey: {accelerator}"))?;
    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let _ = app.emit("handoff-requested", ());
            }
        })
        .map_err(|e| e.to_string())
}

/// Deliver a Ctrl+V keystroke to the focused window via the Win32 `SendInput` API
/// (no extra dependency — reuses the `windows` crate already pulled in for DPAPI).
#[cfg(windows)]
fn send_paste_keystroke() -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_V,
    };

    fn key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    let inputs = [
        key(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key(VK_V, KEYBD_EVENT_FLAGS(0)),
        key(VK_V, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err("failed to deliver the paste keystroke".to_string());
    }
    Ok(())
}

/// Non-Windows builds (the Linux test/dev box) can't synthesize the keystroke; the
/// app only ships on Windows (NFR target), so this is a compile stub.
#[cfg(not(windows))]
fn send_paste_keystroke() -> Result<(), String> {
    Err("paste keystroke is only implemented on Windows".to_string())
}
