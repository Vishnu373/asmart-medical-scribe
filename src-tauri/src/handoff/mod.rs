//! Clipboard write behind the note's Copy button. The global-hotkey EMR hand-off
//! (no-activate overlay, Alt+P, simulated Ctrl+V) has been withdrawn; this module
//! no longer does anything beyond a plain clipboard write.

use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

/// Put plain text on the clipboard for manual paste into the EMR. No auto-clear
/// here: the clinician controls when the paste happens, so a timed wipe could
/// clear the text before they use it.
#[tauri::command]
pub fn copy_to_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}
