use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

// Put plain text on the clipboard for manual paste into the EMR.
#[tauri::command]
pub fn copy_to_clipboard(app: AppHandle, text: String) -> Result<(), String> {
    app.clipboard().write_text(text).map_err(|e| e.to_string())
}
