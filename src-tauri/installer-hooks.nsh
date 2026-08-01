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
;
; The identifier below is one of THREE hand-written copies that nothing cross-checks:
; tauri.conf.json (authoritative), src/prime_kv.rs (IDENTIFIER), and this file. Change all
; three together.

; The app links OpenSSL (libcrypto-4/libssl-4) and the MSVC runtime
; (MSVCP140/VCRUNTIME140/VCOMP140). Rather than running vc_redist.x64.exe — which
; needs admin elevation and stalls a per-user, non-elevated install — we ship the
; individual runtime DLLs app-locally in libs/, bundled next to the exe via the
; `libs/*` resource glob. Windows loads them from the exe dir, so no elevation,
; no separate installer, and no missing-DLL error on a clean machine.
!macro NSIS_HOOK_POSTUNINSTALL
  RMDir /r "$APPDATA\com.asmartmedicalscribe.app"
!macroend

; Prime the on-disk prefix KV cache while the app is not running (design §8.7, §14.3).
; A llama-cpp-sys-2 bump changes the blob's filename, so the first launch after such an
; update would otherwise pay ~22s of prefix decode in the doctor's face. Paying it here
; means the first session after an update is already fast.
;
; No-ops in well under a second whenever there is nothing to do: --prime-kv returns
; immediately if the models dir or the GGUF is missing (fresh install — the weights are
; downloaded later, and the Setup screen owns that prime) or if the correctly-named blob
; is already present (any update that didn't move llama.cpp).
;
; ExecToLog, not Exec, so the installer waits for the prime instead of racing the first
; launch, and the output lands in the install log. The return code is popped and ignored:
; --prime-kv always exits 0 by design, and a failed prime must never fail an install —
; the app re-primes at launch (§8.4). Failure is therefore silent here; medscribe.log is
; where the prime reports itself.
;
; ${MAINBINARYNAME} is the installer template's own define (the Cargo package name,
; `asmart-medical-scribe` — NOT `productName`), so the exe name is never hand-copied here
; and cannot drift. Hardcoding it produced a silently no-op hook.
!macro NSIS_HOOK_POSTINSTALL
  ; nsExec::ExecToLog '"$INSTDIR\asmart_medical_scribe.exe" --prime-kv'
  nsExec::ExecToLog '"$INSTDIR\${MAINBINARYNAME}.exe" --prime-kv'
  Pop $0
!macroend
