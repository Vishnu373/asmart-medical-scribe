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

; The app links OpenSSL 3 (libcrypto-4/libssl-4, bundled to $INSTDIR) and the
; MSVC runtime (MSVCP140/VCRUNTIME140/VCOMP140). A clean client machine usually
; lacks the VC++ redistributable, so libcrypto-4 and the app fail to load with a
; missing-DLL error. We ship vc_redist.x64.exe as a resource (installed to
; $INSTDIR\redist) and run it silently here, then remove it. The redist installer
; is a no-op if a newer VC++ runtime is already present.
!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$INSTDIR\redist\vc_redist.x64.exe" /install /quiet /norestart'
  Delete "$INSTDIR\redist\vc_redist.x64.exe"
  RMDir "$INSTDIR\redist"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  RMDir /r "$APPDATA\com.asmartmedicalscribe.app"
!macroend
