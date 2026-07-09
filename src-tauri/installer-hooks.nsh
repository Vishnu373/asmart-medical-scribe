; Custom NSIS installer hooks for ASmart Medical Scribe (Tauri v2).
;
; Problem: the app stores data at runtime in the roaming app-data dir
; ($APPDATA\com.asmartmedicalscribe.app), NOT the install dir — downloaded model weights
; (multi-GB), the encrypted clinical PHI database (clinical.db) and its DPAPI key
; (db.key), and settings.json. The default uninstaller only removes what it
; installed, so all of that would be left orphaned.
;
; Scope decision: on uninstall we remove the ENTIRE app-data dir — models,
; database, key, and settings — so nothing is left behind (explicit product
; requirement). NOTE: this destroys the doctor's local patient records; it is a
; deliberate full wipe, not a bug.
;
; $APPDATA = the current user's Roaming AppData (matches Tauri's app_data_dir on
; Windows). Per-user install; nothing to clean under a machine-wide location.

!macro NSIS_HOOK_POSTUNINSTALL
  RMDir /r "$APPDATA\com.asmartmedicalscribe.app"
!macroend
