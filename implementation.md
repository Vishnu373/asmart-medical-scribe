# Implementation Plan

> Source design: design.md
> Conventions: CLAUDE.md (none in this project — defaults noted below)
> STT source: reused from the `handy/` reference codebase in this repo
> Generated: 2026-06-26

## Overview

We build an on-device Windows medical scribe as a Tauri 2 app (React + TypeScript
frontend, Rust backend) that records a consult, transcribes locally, and generates
a SOAP note — with zero PHI egress and encryption at rest. The **STT pipeline is
not written from scratch**: we lift Handy's proven audio + VAD + engine code
(`audio_toolkit/`, the `transcribe-rs` engine integration, model lifecycle) and
adapt it from push-to-talk batch dictation into long-form streaming segmentation
that emits live transcript segments. Everything else (encrypted storage, model
residency, LLM note generation, records, EMR hand-off, UI) is built fresh on top.

Work is split into **Backend (B1–B11)** and **Frontend (F1–F7)** phases, each
small and independently verifiable. STT-related backend phases (B3–B6) are
explicitly framed as *port-and-adapt from `handy/`* rather than green-field.

## Assumptions & Decisions

- **Scope:** Full end-to-end pipeline (capture → transcript → SOAP note → store →
  hand-off), not a thin MVP. (User-confirmed.)
- **Dev/build target:** Develop, build, and run directly on Windows 11 native,
  MSVC toolchain, CPU-only (no WSL split — one environment). DPAPI, global
  hotkeys, the no-activate overlay, and audio capture are Windows-only and
  exercised there. (User-confirmed.)
- **Frontend package manager:** Bun. (User-confirmed.)
- **Test depth:** Heavy automated coverage — `cargo test` (unit + integration with
  a mock `Transcriber`) and Vitest/RTL on the frontend. (User-confirmed.)
- **STT reuse (binding instruction):** Reuse the `handy/` codebase for STT.
  Concretely we port:
  - `handy/src-tauri/src/audio_toolkit/` → capture (`cpal`), resampling
    (`rubato` → 16 kHz mono f32), Silero VAD (`vad-rs`), text filters.
  - `handy/src-tauri/src/managers/transcription.rs` + `managers/model.rs` → the
    `transcribe-rs` engine wrapper (`LoadedEngine`, load/unload, idle watcher,
    `catch_unwind` transcribe, language validation), trimmed to the two engines
    we need.
  - Dependency: `transcribe-rs = { version = "0.3.8", default-features = false,
    features = ["onnx"] }` (CPU-only; the ONNX feature provides Parakeet).
- **STT models (design §6.4) — v1 is Parakeet-only.** Sole engine = **Parakeet TDT
  0.6B v3** via the `transcribe-rs` ONNX engine (multilingual EN+FR, CPU), behind
  the `Transcriber` trait. **Divergence from design §6.4**, which lists Whisper
  small/medium as a selectable fallback: dropped for v1 (user decision, 2026-06-29)
  because whisper.cpp and `llama-cpp-2` (B10) each statically vendor **ggml**, which
  collides at link time (`LNK2005`/`LNK1169`). Parakeet (ONNX) carries no ggml, so
  the LLM links cleanly. Whisper-as-a-runtime-option is deferred to a possible later
  out-of-process-sidecar design (keeps each ggml in its own binary).
- **Key difference from Handy we must build, not copy:** Handy is push-to-talk
  (record → stop → transcribe the whole buffer → paste). Medical-scribe is a
  long-form consult that must emit **live `transcript-segment` events** during
  recording. So we keep Handy's capture + VAD + engine primitives but write a new
  **streaming segmenter** (onset/hangover/prefill smoothing → numbered segments →
  mpsc queue → STT worker thread) on top.
- **Persistence:** single SQLCipher-encrypted SQLite DB via `rusqlite`
  (bundled-sqlcipher) for PHI; separate plain JSON settings store (no PHI). Schema
  per design §9.2 (`records`, `notes`).
- **Key management:** Windows DPAPI wraps a random AES-256 DB key, scoped to the
  Windows user; no passphrase (design §10.1).
- **Note generation:** `llama-cpp-2` GGUF in-process CPU; ≥16 GB → Mistral-7B-
  Instruct-v0.3 Q4_K_M, <16 GB → Phi-3.5-mini-instruct (design §8). Phase 2.
- **No CLAUDE.md** exists in the medical-scribe project. Defaults adopted: Rust
  2021 + `rustfmt` + `clippy`; TypeScript strict + ESLint + Prettier + Vite;
  `cargo test` + Vitest/RTL; conventional commits. (Handy's own AGENTS.md
  conventions are followed only when porting its files.)
- **Standalone constraint:** `design.md` must never name Handy. This plan and the
  shipped product are standalone; `handy/` is a build-time source we copy from,
  not a runtime dependency, and no "Handy" branding/strings carry over.

## Target folder structure

```
medical-scribe/
├── design.md
├── implementation.md
├── package.json / vite.config.ts / tsconfig.json / tailwind.config.js
├── index.html
├── src/                              # React + TS frontend
│   ├── main.tsx / App.tsx
│   ├── bridge/                       # invoke() wrappers + event listeners
│   ├── state/                        # store (recording, transcript, notes, settings)
│   ├── views/                        # RecordingView, RecordsView, SettingsView
│   ├── components/                   # TranscriptEditor, SoapEditor, handoff overlay, etc.
│   └── overlay/                      # no-activate hand-off picker window entry
└── src-tauri/
    ├── Cargo.toml / tauri.conf.json / build.rs
    ├── resources/models/             # silero_vad + STT/LLM model files
    └── src/
        ├── main.rs / lib.rs          # Tauri setup, manager wiring, commands registry
        ├── audio_toolkit/            # PORTED from handy: audio/, vad/, text, utils
        ├── stt/                      # engine wrapper (ported) + Transcriber trait
        ├── segment/                  # NEW streaming segmenter (numbered segments)
        ├── orchestrator/             # state machine IDLE/RECORDING/PROCESSING/GENERATING
        ├── residency/               # RAM probe + co-resident vs swap decision (§7)
        ├── llm/                      # llama-cpp-2 SOAP note generation (§8)
        ├── store/                    # SQLCipher DB (records, notes) + migrations
        ├── crypto/                   # DPAPI-wrapped AES-256 key (§10.1)
        ├── settings/                # plain JSON settings (no PHI)
        ├── handoff/                  # global shortcut + overlay + clipboard paste (§8.6)
        ├── telemetry/               # crash reporting, PHI fields excluded (§10.3)
        └── commands/                # Tauri command handlers (§9.4)
```

## Backend

### Phase B1 — Project scaffold & Tauri shell  `[x] done`
**Goal:** A buildable, empty Tauri 2 app that launches a window on Windows.
**Depends on:** none
**Tasks:**
- [x] Initialize Tauri 2 project (Rust backend + Vite/React/TS frontend), MSVC target.
- [x] Set up `Cargo.toml` with pinned deps (tauri, serde, anyhow, log, thiserror)
      and the workspace module layout above (empty modules with `//!` docs).
- [x] Configure `tauri.conf.json` (single window, app identifier, no auto-updater).
- [x] Wire `rustfmt.toml`, `clippy` in CI-style `cargo check`, ESLint/Prettier/TS-strict.
- [x] Add a trivial `ping` command + frontend `invoke('ping')` to prove the bridge.
**Deliverables:** `src-tauri/`, `src/`, config files, module skeletons.
**Verification:** `bun run tauri dev` launches; `ping` round-trips; `cargo fmt
--check`, `cargo clippy`, `bun run build` all pass on Windows.

### Phase B2 — Encrypted storage, crypto & settings  `[x] done`
**Goal:** Persist records/notes in a SQLCipher DB keyed by a DPAPI-wrapped key,
plus a plain JSON settings store.
**Depends on:** B1
**Tasks:**
- [x] `crypto/`: generate random AES-256 key; wrap/unwrap with Windows DPAPI
      (`CryptProtectData`/`CryptUnprotectData` via `windows` crate); persist blob.
- [x] `store/`: open `rusqlite` with bundled-sqlcipher, apply key, run migrations
      for `records` + `notes` (schema §9.2) via `rusqlite_migration`.
- [x] CRUD helpers: insert/list/open/delete record; insert/list notes, `is_active`
      version toggling.
- [x] `settings/`: plain JSON store (selected models, language, hotkey, device) —
      assert no PHI fields.
**Deliverables:** `crypto/`, `store/`, `settings/` modules + migrations.
**Verification:** `cargo test` — round-trip encrypt/decrypt key; open DB with key,
write+read a record/note, reject wrong key; migration idempotency.

### Phase B3 — Port audio capture & resampling  `[x] done`
**Goal:** Capture mic audio and resample to 16 kHz mono f32 using the ported toolkit.
**Depends on:** B1
**Tasks:**
- [x] Copy the reference `audio_toolkit/audio/` (device, recorder, resampler,
      utils, visualizer) into `src-tauri/src/audio_toolkit/audio/`; strip branding/i18n;
      keep `cpal` + `rubato` logic.
- [x] Adapt `AudioRecorder` to expose a continuous f32 sample stream (frame
      callback) suitable for streaming, not just stop-and-drain.
- [x] Port device enumeration (`list_input_devices`) for the settings UI.
- [x] Emit an `input-level` value from the visualizer path (design §9.5).
**Deliverables:** ported `audio_toolkit/audio/`, capture API.
**Verification:** `cargo test` on resampler (rate-convert a known WAV, assert
length/format); manual: capture a few seconds on Windows and dump a 16 kHz WAV.

### Phase B4 — Port Silero VAD  `[x] done`
**Goal:** Voice-activity detection over the 16 kHz stream via the ported Silero VAD.
**Depends on:** B3
**Tasks:**
- [x] Copy the reference `audio_toolkit/vad/` (silero, smoothed, mod) and the
      `vad-rs` dep; vendor the `silero_vad_v4.onnx` model into `resources/models/`.
- [x] Expose a `VoiceActivityDetector` API returning a per-frame speech decision.
- [x] Unit-test the smoothing wrapper with synthetic speech/silence frames.
**Deliverables:** ported `audio_toolkit/vad/`, VAD model resource.
**Verification:** `cargo test` — silence → no speech, tone/speech fixture → speech;
smoothing onset/hangover behaves.

### Phase B5 — Port STT engine wrapper (`transcribe-rs`)  `[x] done`
**Goal:** A `Transcriber` trait backed by Parakeet (default) and Whisper (fallback),
with model load/unload + idle watcher — adapted from the reference transcription manager.
**Depends on:** B1, B2
**Tasks:**
- [x] Add `transcribe-rs = { version = "0.3.8", features = ["whisper-cpp","onnx"] }`.
- [x] Port the relevant parts of the reference `managers/transcription.rs`:
      `LoadedEngine` (trimmed to `Whisper` + `Parakeet` only), `load`/`unload`,
      idle-unload watcher, `catch_unwind` transcribe, language validation (EN/FR).
- [x] Define `trait Transcriber { fn transcribe(&self, audio: &[f32]) -> Result<String>; }`
      and implement it over the ported engine (`SttEngine`).
- [x] Provide a `MockTranscriber` (echo/fixture) for tests, mirroring the reference mock.
- [x] Port the `text.rs` filters (`filter_transcription_output`, `apply_custom_words`).
**Deliverables:** `stt/` module, `Transcriber` trait + Parakeet/Whisper impls + mock.
**Verification:** `cargo test` with `MockTranscriber`; manual Windows run loading
Parakeet v3 and transcribing a fixture WAV to expected text.

### Phase B6 — Streaming segmenter (NEW, on top of ported VAD/STT)  `[x] done`
**Goal:** Turn the continuous stream into numbered speech segments that flow to the
STT worker and emit live `transcript-segment{seq,text}` events.
**Depends on:** B3, B4, B5
**Tasks:**
- [x] `segment/`: consume capture frames + VAD probabilities; apply onset/hangover/
      prefill smoothing to cut speech segments (design §6.5).
- [x] Assign monotonic `seq` numbers; push segments onto an mpsc queue.
- [x] STT worker thread drains the queue, calls `Transcriber`, emits
      `transcript-segment{seq,text}` in order (design §9.5).
- [x] Backpressure/ordering tests so out-of-order completion still emits in `seq` order.
**Deliverables:** `segment/` module + STT worker thread.
**Verification:** `cargo test` — feed a multi-utterance fixture through capture→VAD→
mock STT, assert correct segment count, `seq` ordering, and emitted text.

### Phase B7 — Recording orchestrator & state machine  `[x] done`
**Goal:** Backend-owned IDLE → RECORDING → PROCESSING with guarded transitions and
`state-changed` events (design §6.6), serialized through a single coordinator.
**Depends on:** B6
**Tasks:**
- [x] `orchestrator/`: single-threaded coordinator owning the state; reject
      illegal/duplicate transitions.
- [x] Commands: `start/stop/pause/resume_recording` (design §9.4) drive the machine.
- [x] Emit `state-changed{state}` and `error{code,message}` events.
- [x] On stop → PROCESSING until the segment queue drains, then IDLE.
**Deliverables:** `orchestrator/` module, recording commands.
**Verification:** `cargo test` — transition table (legal/illegal), duplicate-start
rejected, stop drains then returns IDLE.

### Phase B8 — Records & transcript commands  `[x] done`
**Goal:** Persist a completed consult and expose record/transcript commands.
**Depends on:** B2, B7
**Tasks:**
- [x] On stop, assemble the full transcript and insert a `records` row.
- [x] Commands: `update_transcript`, `list_records`, `open_record`, `delete_record`.
- [x] Map errors to `error{code,message}`; ensure deletes cascade to `notes`.
**Deliverables:** records/transcript commands wired to `store/`.
**Verification:** `cargo test` — record lifecycle end-to-end with the in-memory/mock
pipeline; delete removes notes.

### Phase B9 — Model residency strategy  `[x] done`
**Goal:** Decide STT+LLM co-residency vs swap from a one-time RAM probe (design §7).
**Depends on:** B5
**Tasks:**
- [x] `residency/`: `sysinfo` total-RAM probe; footprint = STT + LLM + 2–3 GB
      headroom; require ≥2 GB margin for co-resident, else swap; decide-once-cache.
- [x] Manual override surfaced via settings.
- [x] Integrate so the LLM phase (B10) and STT (B5) honor the residency decision.
**Deliverables:** `residency/` module.
**Verification:** `cargo test` — decision boundaries (8/16/32 GB synthetic probes,
override path).

### Phase B10 — LLM SOAP note generation  `[x] done`
**Goal:** Generate streaming SOAP notes in-process from the transcript (design §8).
**Depends on:** B8, B9
**Tasks:**
- [x] `llm/`: `llama-cpp-2` GGUF loader; model pick by RAM (Mistral-7B vs Phi-3.5).
- [x] Zero-shot SOAP prompt (four `##` headers), low temperature, anti-hallucination
      guardrails, warmup, cancel; add GENERATING state to the orchestrator.
- [x] Stream `generation-token{text}`; persist result as a `notes` row (`is_active`).
- [x] Commands: `generate_note`, `regenerate_note`, `cancel_generation`,
      `update_note`, `revert_version`.
**Deliverables:** `llm/` module, note commands, GENERATING state.
**Verification:** `cargo test` with a stub/tiny model or mocked generator —
streaming tokens, cancel, version revert; manual Windows run produces a SOAP note.

### Phase B11 — EMR hand-off & crash reporting  `[x] done`
**Goal:** Global-hotkey overlay paste of SOAP sections, plus PHI-safe crash reports.
**Depends on:** B10
**Tasks:**
- [x] `handoff/`: `tauri-plugin-global-shortcut` Alt+P (rebindable) → no-activate
      always-on-top overlay (`WS_EX_NOACTIVATE`) → S/O/A/P picker.
- [x] Deterministic markdown parser → clipboard + simulated Ctrl+V → clipboard
      auto-clear; command `paste_section`.
- [x] `telemetry/`: Sentry-class crash reporting with `transcript`/`soap_data`/
      `label` structurally excluded.
**Deliverables:** `handoff/`, `telemetry/` modules + `paste_section` command.
**Verification:** `cargo test` — markdown→section parser; manual Windows: hotkey
shows overlay without stealing focus, pastes the chosen section, clipboard clears;
crash report payload asserted to contain no PHI fields.

## Frontend

### Phase F1 — App shell & Tauri bridge  `[x] done`
**Goal:** React shell with typed `invoke`/event wrappers and global app state.
**Depends on:** B1
**Tasks:**
- [x] `bridge/`: typed wrappers for all §9.4 commands and §9.5 event listeners.
- [x] `state/`: store slices (recording, transcript, notes, records, settings).
- [x] App layout/router for Recording / Records / Settings views; Tailwind theme.
**Deliverables:** `bridge/`, `state/`, shell layout.
**Verification:** Vitest — bridge wrappers call `invoke` with correct args; shell
renders; `ping` works through the typed bridge.

### Phase F2 — Recording view  `[x] done`
**Goal:** Start/stop/pause recording with live input level + state display.
**Depends on:** B7, F1
**Tasks:**
- [x] Record controls bound to `start/stop/pause/resume_recording`.
- [x] Input-level meter from `input-level`; status from `state-changed`.
- [x] Error toasts from `error` events.
**Deliverables:** `RecordingView` + meter/status components.
**Verification:** RTL — controls dispatch correct commands; meter reacts to mocked
`input-level`; state label follows `state-changed`.

### Phase F3 — Live transcript editor  `[x] done`
**Goal:** Render streaming segments and allow edits saved via `update_transcript`.
**Depends on:** B6, B8, F2
**Tasks:**
- [x] Append `transcript-segment{seq,text}` in `seq` order; live-growing view.
- [x] Editable transcript; debounced save to `update_transcript`.
**Deliverables:** `TranscriptEditor`.
**Verification:** RTL — segments render in order from mocked events; edit → save
invoked with merged text.

### Phase F4 — SOAP note generation UI  `[ ] not started`
**Goal:** Trigger/stream/edit SOAP notes with versioning.
**Depends on:** B10, F3
**Tasks:**
- [ ] Generate/regenerate/cancel buttons; stream `generation-token` into a SOAP view.
- [ ] Four-section editor; save via `update_note`; version revert via `revert_version`.
**Deliverables:** `SoapEditor` + generation controls.
**Verification:** RTL — streaming tokens accumulate; cancel stops; revert calls
`revert_version`.

### Phase F5 — Records browser  `[ ] not started`
**Goal:** List/open/delete past consults and their notes.
**Depends on:** B8, F1
**Tasks:**
- [ ] `RecordsView` listing from `list_records`; open loads transcript + notes.
- [ ] Delete with confirm → `delete_record`.
**Deliverables:** `RecordsView`.
**Verification:** RTL — list renders from mocked data; open/delete invoke correct
commands.

### Phase F6 — Settings view  `[ ] not started`
**Goal:** Configure model, language, input device, hotkey, residency override.
**Depends on:** B5, B9, F1
**Tasks:**
- [ ] Forms bound to `get_settings`/`update_settings`; STT/LLM model pickers;
      language (EN/FR); input device (from B3 enumeration); residency override.
- [ ] Hotkey rebinding control for hand-off.
**Deliverables:** `SettingsView`.
**Verification:** RTL — load/save round-trip; invalid input guarded.

### Phase F7 — Hand-off overlay UI  `[ ] not started`
**Goal:** The no-activate S/O/A/P picker overlay window.
**Depends on:** B11, F4
**Tasks:**
- [ ] `overlay/` entry window; S/O/A/P picker calling `paste_section`.
- [ ] Minimal always-on-top styling; keyboard navigable.
**Deliverables:** overlay window + picker.
**Verification:** RTL on the picker component; manual Windows: overlay appears via
hotkey without focus steal and pastes the chosen section.

## Progress Log

- 2026-06-26 — Plan drafted. STT phases (B3–B6) reframed to port-and-adapt from the
  `handy/` codebase per user instruction; non-STT phases built fresh per design.md.
  Awaiting approval before any code is written.
- 2026-06-27 — Plan approved. Decisions added: develop/build/run directly on Windows
  (no WSL split), Bun as the frontend package manager.
- 2026-06-27 — **B1 built.** Scaffolded the Tauri 2 shell:
  - Frontend: `package.json` (Bun, React 18 + TS strict + Vite 6 + Tailwind v4),
    `vite.config.ts`, `tsconfig*.json`, `index.html`, `src/main.tsx`, `src/App.tsx`,
    `src/styles.css`, `src/bridge/index.ts` (typed `ping` wrapper), `eslint.config.js`
    (flat, `no-explicit-any: error`), `.prettierrc`.
  - Backend: `src-tauri/Cargo.toml`, `build.rs`, `tauri.conf.json` (single 1100×740
    window, id `com.medscribe.app`, updater off), `capabilities/default.json`,
    `rustfmt.toml`, `src/main.rs`, `src/lib.rs` (registers `ping`, `tauri-plugin-log`),
    `src/commands/mod.rs` (`ping`), and 11 empty module skeletons (`audio_toolkit`,
    `stt`, `segment`, `orchestrator`, `residency`, `llm`, `store`, `crypto`,
    `settings`, `handoff`, `telemetry`) each with a `//!` doc pointing at its phase.
  - `.gitignore` excludes `target/`, `gen/`, `node_modules/`, model weights, and any
    `*.db`/`*.sqlite` (PHI). `resources/models/.gitkeep` + empty `icons/` created.
  - **Icons:** placeholder icon set generated (teal square + white medical cross) at
    `src-tauri/icons/` — `tauri-build` requires `icon.ico` even for `dev` on Windows.
    Replace with real artwork via `bun run tauri icon <png>` before release.
  - **Windows dev-box setup encountered (one-time):** install Rust via rustup, install
    VS Build Tools "Desktop development with C++" (provides `link.exe`), and set Smart
    App Control to Evaluation/Off (it blocked unsigned Rust build-script `.exe`s,
    os error 4551). **Release follow-up:** Windows code signing so end users don't hit
    SmartScreen/SAC warnings — separate from the dev-box workaround.
  - **Verified (Windows):** `bun run tauri dev` opens the window showing the
    `pong:` bridge reply — frontend↔backend round-trip confirmed. B1 done.
  - **Pending Windows verification (user):** `bun install`, `bun run tauri dev`
    (window shows "bridge: pong: hello from frontend"), `cargo fmt --check`,
    `cargo clippy`, `bun run build`.
- 2026-06-27 — **B2 built.** Encrypted storage, crypto & settings:
  - `crypto/mod.rs`: `load_or_create_key(path)` generates a random 32-byte key via
    `rand::OsRng`, wraps it with Windows DPAPI (`CryptProtectData`, `CRYPTPROTECT_UI_FORBIDDEN`,
    user-scoped) and writes only the wrapped blob; unwraps on reload. Non-Windows builds
    get a stub that errors. `#[cfg(all(test, windows))]` round-trip + persist/reload tests.
  - `store/mod.rs`: `Store::open(path, key)` opens SQLCipher via `rusqlite`, applies the key
    as raw hex (`PRAGMA key = "x'…'"`), enables FKs, verifies decryption by touching the
    schema, then runs `rusqlite_migration` to latest. Migration creates `records` + `notes`
    (schema §9.2) with `ON DELETE CASCADE` + index. CRUD: `create_record`/`update_transcript`/
    `list_records`(summary)/`open_record`/`delete_record`; `insert_note` (auto-activates,
    deactivates siblings in a tx)/`list_notes`/`set_active_note`. Tests: migration
    validity+idempotency, record round-trip, single-active-note toggling+cascade,
    wrong-key rejection.
  - `settings/mod.rs`: plain-JSON `Settings` (model/mic/hotkey + internal residency/RAM/
    vad/idle) with sensible defaults; `load` (defaults if absent) / `save` (pretty JSON).
    Test asserts the serialized form carries no PHI field names.
  - **Deps added** (`src-tauri/Cargo.toml`): `rusqlite` (feature
    `bundled-sqlcipher`), `rusqlite_migration`, `rand`, `uuid`, `zeroize`,
    `windows` (target-gated to `cfg(windows)`, Foundation + Security_Cryptography),
    dev-dep `tempfile`.
  - **DECISION / Windows build prerequisite:** use `bundled-sqlcipher`, linking the
    **system OpenSSL** (user is installing OpenSSL from the GitHub source / Win64 build).
    Set `OPENSSL_DIR` to the install (e.g. `C:\Program Files\OpenSSL-Win64`). No Perl/NASM
    needed (that was the earlier vendored-openssl path). Add to README once confirmed building.
  - **Hardening:** transient unwrapped key copy is `zeroize()`'d; `set_active_note` enforces
    the single-active invariant (errors + rolls back on an unknown note_id); settings struct is
    `#[serde(default)]` for forward-compat; list ordering tiebreaks by `rowid`.
  - **Pending Windows verification (user):** `cargo test` (DPAPI round-trip is Windows-only;
    SQLCipher tests need the `bundled-sqlcipher` build to link OpenSSL → `OPENSSL_DIR` set).
    Expect `dead_code` warnings for the new modules until B7 wires them into Tauri commands.
- 2026-06-28 — **B3 built.** Ported audio capture & resampling:
  - `audio_toolkit/mod.rs`: module wiring + re-exports; inlined `get_cpal_host()` and a
    `TARGET_SAMPLE_RATE = 16_000` const (replaces the reference's `WHISPER_SAMPLE_RATE`).
  - `audio_toolkit/audio/`: `resampler.rs` (`FrameResampler`, rubato FFT downsample →
    fixed 30 ms frames, pass-through when rates match), `visualizer.rs` (`AudioVisualiser`
    FFT spectrum → 16 normalised level buckets), `utils.rs` (WAV read/save/verify),
    `device.rs` (`list_input_devices` + `CpalDeviceInfo`; dropped unused output enumeration),
    `recorder.rs` (`AudioRecorder`).
  - **Key adaptation (streaming, not push-to-talk):** removed the recorder's VAD coupling
    (VAD moves to the segmenter in B4/B6) and added `with_frame_callback` — each captured
    16 kHz mono frame is delivered live during recording for the streaming segmenter, while
    `stop()` still returns the full buffer. `with_level_callback` feeds the input-level meter.
    Capture stays at the device's native rate; `FrameResampler` downsamples to 16 kHz.
  - **Deps added** (`src-tauri/Cargo.toml`): `cpal = "0.16"`, `rubato = "0.16"`,
    `hound = "3.5"`, `rustfft = "6"`.
  - **Tests:** resampler 48k→16k length, equal-rate pass-through, fixed 480-sample frame
    size; WAV save/verify/read round-trip; the mic-error string classifiers.
  - **Note:** the actual Tauri `emit("input-level")` / device-list command wiring is deferred
    to B7 (orchestrator) and F6 (settings) — the recorder exposes callbacks, not an AppHandle,
    to stay decoupled and unit-testable.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test` (needs `OPENSSL_DIR`
    for the bundled-sqlcipher link from B2; cpal links WASAPI). Manual: capture a few seconds
    and dump a 16 kHz WAV via `save_wav_file`. Expect `dead_code` warnings until B6/B7 wire it.
- 2026-06-28 — **B4 built.** Ported Silero VAD:
  - `audio_toolkit/vad/`: `mod.rs` (`VadFrame` Speech/Noise enum + `VoiceActivityDetector`
    trait with `push_frame`/`is_voice`/`reset`), `silero.rs` (`SileroVad` over `vad-rs`,
    16 kHz 30 ms frames, probability thresholded to keep/drop), `smoothed.rs` (`SmoothedVad`
    onset/hangover/prefill wrapper). Re-exported from `audio_toolkit`.
  - Updated `silero.rs` to use `TARGET_SAMPLE_RATE` (replaces the reference's
    `constants::WHISPER_SAMPLE_RATE`).
  - **Trait shape:** the reference trait surfaces a thresholded keep/drop decision
    (`VadFrame`), not a raw float — kept as-is since the B6 segmenter consumes Speech/Noise
    frames via `SmoothedVad`, not probabilities. (Plan said "per-frame speech probability";
    ported faithfully as the equivalent decision API.)
  - **Dep added:** `vad-rs = { git = "https://github.com/cjpais/vad-rs", default-features = false }`
    (same fork the reference uses; pulls an ONNX runtime).
  - **Model:** `silero_vad_v4.onnx` (1.8 MB) copied to `src-tauri/resources/models/`. Per user
    decision, added a `.gitignore` carve-out (`!silero_vad_v4.onnx`) so this small, startup-required
    model IS committed; the large STT/LLM weights remain excluded.
  - **Tests:** `SmoothedVad` onset (N consecutive voice frames required), hangover (holds speech
    past brief silence), prefill (prepends buffered pre-roll), and sustained-silence stays Noise —
    driven by a scripted boolean mock VAD, so no ONNX model needed for `cargo test`.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test`. The smoothing tests run
    pure-Rust; loading `SileroVad` itself needs the ONNX model + runtime and is exercised in B6.
- 2026-06-28 — **B5 built.** Ported the STT engine wrapper behind a `Transcriber` trait:
  - `stt/transcriber.rs`: `trait Transcriber { fn transcribe(&self, audio: &[f32]) -> Result<String> }`
    (design's interface; the segmenter/orchestrator depend only on this).
  - `stt/engine.rs`: `SttEngine` over `transcribe-rs`. `LoadedEngine` trimmed from the reference's
    eight engines to `Whisper(WhisperEngine)` + `Parakeet(ParakeetModel)`; `ModelKind` enum;
    `load(kind, path)` / `unload` / `is_loaded` / `current_model` / `set_language` /
    `touch_activity`. `transcribe` keeps the reference's **`catch_unwind`** discipline (take the
    engine out, run the native call unlocked, put it back on success; on panic drop it = unload +
    clear model id, no mutex poisoning) and runs the result through `filter_transcription_output`.
    **Idle watcher** decoupled from the reference's AppHandle/settings/recording-state checks: a
    background thread unloads the model after `idle_timeout` of no activity (0 = never); `Drop`
    signals shutdown + joins. Language validation trimmed to EN/FR (+auto) for Whisper; Parakeet
    TDT v3 auto-detects (no language param, as in the reference).
  - `stt/mock.rs`: `MockTranscriber` — echo (`new`) or queued (`with_responses`) responses with a
    `call_count`; empty audio → empty string. For B6/B7 tests. Mirrors the reference mock.
  - `stt/text.rs`: ported verbatim (fuzzy custom-word correction + filler/stutter filtering) with
    its own 30+ tests.
  - **Decoupling (key adaptation):** dropped the reference's `AppHandle`/`Emitter` events,
    download `ModelManager`, `specta` bindings, GPU/Vulkan/DirectML accelerator plumbing and the
    `model-state-changed` events. The orchestrator (B7) drives load/unload and language; model
    file paths come from `resources/models/` (residency decision lands in B9).
  - **Deps added** (`src-tauri/Cargo.toml`): `transcribe-rs = "0.3.8"` (`whisper-cpp` + `onnx`,
    CPU-only), `natural`, `once_cell`, `regex`, `strsim` (for `text.rs`).
  - **Tests:** `MockTranscriber` ordering/echo/empty, `SttEngine` language validation + the
    not-loaded/empty-audio paths (no native model needed), plus the verbatim `text.rs` suite.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test`. First build compiles
    `transcribe-rs` (whisper-cpp C++ + ONNX runtime) — slow, needs the MSVC toolchain. Real
    Parakeet/Whisper transcription (loading a model + a fixture WAV) is a manual Windows check;
    the unit tests above don't load a native model. Expect `dead_code` warnings until B6/B7 wire it.

- 2026-06-29 — **B3/B4/B5 review fixes** (post-review pass; all verified on Windows `cargo test`):
  1. `audio_toolkit/audio/resampler.rs`: `finish()` now clears `in_buf` AND calls
     `resampler.reset()` — the worker reuses one `FrameResampler` across Start/Stop cycles, so
     leftover input bytes / internal FFT state were leaking the previous consult's tail into the
     next recording. New `finish_resets_state_between_recordings` test.
  2. `stt/engine.rs`: default language `"en"` → `"auto"` (design FR-2/FR-5) so a French consult
     isn't force-decoded as English under the Whisper fallback before `set_language` is called.
  3. `stt/engine.rs`: added a recording-aware guard — `set_recording(bool)` + a `recording` flag
     the idle watcher checks, so the model is never unloaded mid-consult through a long silence
     gap (the reference's `is_recording()` keep-alive, which the port had dropped).
  4. `audio_toolkit/vad/smoothed.rs`: frame-buffer cap `prefill_frames + 1` →
     `prefill_frames + onset_frames`, so onset voice frames aren't evicted before the trigger
     fires (clipped leading syllable when `onset > prefill + 1`). New no-clip regression test.
  5. `stt/engine.rs`: idle timer switched from wall-clock `SystemTime` to a monotonic `Instant`
     epoch (via `once_cell::Lazy`) so clock changes can't pin or prematurely unload the model.
  6. `audio_toolkit/audio/visualizer.rs`: `feed()` drains only the consumed window instead of
     `clear()`-ing, so a cpal chunk larger than `window_size` and partial tails carry across calls.
  7. `stt/engine.rs`: removed the redundant `is_none()` pre-check in `transcribe()` (double-lock /
     small TOCTOU); the `take()` match already handles the not-loaded case.
  - Custom-words (`apply_custom_words`) left in place but unwired (`&None`) — intentional, B7
    will pass the doctor's vocabulary from settings.

- 2026-06-29 — **B6 — Streaming segmenter** done. New code on the ported VAD/STT pieces.
  - `segment/segmenter.rs`: `Segmenter` takes a `Box<dyn VoiceActivityDetector>` (production:
    `SmoothedVad`; tests: a scripted VAD), accumulates speech frames, and cuts a numbered
    `Segment{seq, audio}` onto an mpsc `Sender` at each pause boundary. `SegmenterConfig` carries
    the min-floor (discard sub-0.2 s blips) and max-cap (force-cut a non-pausing speaker at 25 s);
    `finish()` force-flushes the open segment on Stop (final words never lost) and resets the VAD.
  - `segment/worker.rs`: `spawn_stt_worker(rx, Arc<dyn Transcriber>, sink)` drains the FIFO queue
    on its own thread, calls `transcribe`, and pushes `TranscriptSegment{seq, text}` to the sink
    in order (empty/whitespace skipped). `SttWorkerHandle::join`/`Drop` waits for the queue to
    drain — B7's PROCESSING→IDLE.
  - **Key decisions:** (a) ordering is inherent — one worker over a FIFO channel, so no reorder
    buffer (the task's "out-of-order completion" can't occur by construction). (b) The worker
    emits through a `FnMut` sink, not a Tauri `Emitter`, keeping B6 decoupled/testable like B5;
    B7 supplies the real `transcript-segment` emit closure. (c) Onset/hangover/prefill smoothing
    lives in B4's `SmoothedVad` (which the segmenter wraps), not re-implemented here.
  - **Tests:** segmenter boundary cut / min-floor discard / max-cap force-cut / Stop tail-flush /
    silence-only; worker seq-ordering + empty-skip. All pure-Rust (scripted VAD + `MockTranscriber`),
    no native model.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test`.

- 2026-06-29 — **B7 — Recording orchestrator & state machine** done. Wires B3–B6
  into a single guarded lifecycle.
  - `orchestrator/coordinator.rs`: `Coordinator` owns `RecordingState`
    (Idle/Recording/Processing) behind a `Mutex` and serializes every transition.
    Guards reject illegal/duplicate requests (second Start, Stop while Idle,
    Start during PROCESSING) returning `Err(String)` to the command. Start failure
    keeps the machine Idle and emits `error`; a panic-poisoned lock is recovered,
    not wedged (design §6.6). It drives a `Pipeline` trait (`start/stop/set_paused`),
    so the whole state machine is unit-tested with a `MockPipeline` — no native deps.
  - `orchestrator/pipeline.rs`: `RealPipeline` is the production `Pipeline` — built
    fresh per recording: cpal `AudioRecorder` (frame cb → `Arc<Mutex<Segmenter>>`,
    level cb → `input-level`) → segment queue → `spawn_stt_worker` over the warm
    `Arc<SttEngine>`, emitting `transcript-segment` on Ok / `error` on Err. Stop
    order (no audio lost): recorder.stop() tail-flushes frames → close + drop the
    recorder (release its frame-cb Arc) → `segmenter.finish()` + drop (closes the
    queue) → `worker.join()` drains → `engine.set_recording(false)`. `emit_app_event`
    maps `AppEvent` → Tauri `emit` (`state-changed` / `error`, §9.5).
  - Commands `start/stop/pause/resume_recording` (`commands/mod.rs`) call the managed
    `Coordinator`; registered in `lib.rs` `setup`, which builds the long-lived
    `SttEngine` (5-min idle-unload) + `RealPipeline` and manages the coordinator.
  - **Key decisions:** (a) Coordinator is generic over a `Pipeline` trait + an
    `EmitFn` closure (not a Tauri `Emitter`) so the state machine is testable; the
    Tauri/audio glue lives entirely in `pipeline.rs`. (b) Stop moves the pipeline
    out of the lock during the blocking drain so a concurrent Start observes
    PROCESSING and is rejected. (c) Pause/resume gate the capture frame-callback via
    an `AtomicBool` and stay in RECORDING — design's `state-changed` contract (§9.5)
    has no PAUSED wire state, so none is emitted (UI tracks its own paused toggle).
  - **Deviations / deferred:** (i) VAD/STT model **asset paths + STT preload** aren't
    wired yet (no model-bundling phase) — `RealPipeline` resolves the Silero model
    from the resource dir, and until the models are bundled/loaded a recording
    surfaces an `error` event instead of transcribing. (ii) On a transcribe error the
    worker emits `error{code:"transcription_failed"}`; the model **reload** the design
    mentions is deferred to when the STT model path exists. Recorded here per the
    implement-from-design "surface divergence" rule.
  - **Tests:** transition walk (Idle→Recording→Processing→Idle + event order),
    duplicate-Start rejected, Stop-while-Idle rejected, start-failure stays Idle +
    emits error, drain-failure still returns to Idle, pause/resume guards (double
    pause / resume-when-not-paused rejected, no extra `state-changed`). All pure-Rust
    via `MockPipeline`.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test`.

- 2026-06-29 — **B8 — Records & transcript commands** done. Persists a consult and
  exposes the records/transcript bridge.
  - `store/mod.rs`: added `SharedStore` — a `Clone`able `Arc<Mutex<Store>>` (the
    `rusqlite::Connection` is `Send` but `!Sync`, so access is serialized; a poisoned
    lock is recovered). One handle is managed as Tauri state and another clone lives
    in `RealPipeline`, so the pipeline's save-on-stop and the records commands hit the
    same keyed connection.
  - `orchestrator/pipeline.rs`: the STT worker sink now also pushes each segment's
    text into a per-recording `Arc<Mutex<Vec<String>>>` (in `seq` order, since the
    worker emits in order). On stop — after the drain and `set_recording(false)` — the
    segments are joined (`assemble_transcript`, trims + drops blanks) and, if non-empty,
    inserted as a `records` row; the new id is returned. An empty consult saves nothing.
  - `orchestrator/coordinator.rs`: `Pipeline::stop` and `Coordinator::stop_recording`
    now return `Result<Option<String>, String>` — the saved record id flows back to the
    caller. The frontend's `invoke('stop_recording')` resolves with the id so it can load
    the record for editing / note generation. Mock + the transition-walk test updated to
    assert the id propagates.
  - `commands/mod.rs`: `stop_recording` returns `Option<String>`; added
    `update_transcript`, `list_records`, `open_record`, `delete_record` over the managed
    `SharedStore`, each mapping store errors to an `Err(String)` the frontend surfaces.
    Deletes cascade to `notes` via the existing FK (B2).
  - `lib.rs` setup: loads/creates the DPAPI-wrapped key (`crypto`) and opens the
    SQLCipher DB at `app_data_dir/clinical.db` (key blob at `app_data_dir/db.key`),
    wraps it in `SharedStore`, hands a clone to `RealPipeline`, and manages it. Registers
    the four new commands.
  - **Key decisions:** (a) record creation lives in `RealPipeline` (it owns the
    accumulated transcript), keeping the `Coordinator` a pure, Tauri/store-free state
    machine. (b) The new record id is delivered as the `stop_recording` return value
    rather than a new event — no `record-saved` event exists in §9.5, and a command
    resolving with its result is the conventional Tauri shape.
  - **Deviations / deferred:** (i) The saved `language` is hardcoded `"en"`
    (`DEFAULT_LANGUAGE`) — design §9.2 expects `en`/`fr` from settings; real language
    wiring lands with F6/settings (and any detection). Recorded per the surface-divergence
    rule. (ii) `label` is saved empty; the doctor titles the encounter in the Records
    view (F5).
  - **Tests:** `assemble_transcript` join/trim/blank-skip + empty cases (pure-Rust in
    `pipeline.rs`); coordinator walk asserts the mock pipeline's record id propagates
    through `stop_recording`. Record CRUD + delete→notes cascade already covered by the
    B2 `store` tests.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test` (the
    DPAPI/SQLCipher paths need Windows + `OPENSSL_DIR`, as in B2).

- 2026-06-29 — **B9 — Model residency strategy** done. One-time co-resident-vs-swap
  decision from a total-RAM probe (design §7).
  - `residency/mod.rs`: `ResidencyMode { CoResident, Swap }` (+ `as_str`/`from_str`
    for persistence). `decide_mode(total_ram, llm_footprint)` is the pure §7
    feasibility calc — `footprint = STT + LLM + headroom`, co-resident only when
    `total_ram ≥ footprint + 2 GB margin`, else swap ("margin, not bare fit").
    `default_llm_footprint(total_ram)` encodes the §8.2 model-choice sizes that feed
    the budget (≥16 GB → Mistral-7B Q4_K_M ~4.4 GB, <16 GB → Phi-3.5-mini Q8_0
    ~4.0 GB); STT ~2.5 GB (Parakeet) + ~3 GB app/OS headroom. `resolve(settings,
    total_ram)` applies precedence — manual override > cached decision (valid only
    while total RAM is unchanged) > (re)decide+cache — returning `(mode, changed)`.
    `probe_total_ram()` reads `sysinfo` total physical RAM (stable per-device; we
    never sample momentary *available* RAM, which would flip-flop).
  - `settings/mod.rs`: added a doctor-facing `residency_override: Option<String>`
    (force `co_resident`/`swap`; precedence over the cached auto decision) alongside
    the existing internal `residency_mode` + `observed_total_ram` cache.
  - `lib.rs` setup: load `settings.json`, `resolve` the mode against the probed RAM,
    persist only if the cache changed, and log the chosen mode. The decision is made
    once at the startup probe per §7.
  - **Footprint constants are design-target estimates** (named consts with comments)
    to validate during benchmarking — the residency *logic* holds whatever the real
    model sizes turn out to be; only the constants move.
  - **Deviation / scope:** the residency mode is *produced and cached* here; the
    actual STT-unload-on-Stop / LLM-load-at-hand-off that *consumes* it is the
    lifecycle's job and lands with B10 (LLM). B5's STT idle-unload already exists.
    Recorded per the surface-divergence rule.
  - **Deps added:** `sysinfo = { version = "0.32", default-features = false,
    features = ["system"] }`.
  - **Tests:** 8/16/32 GiB boundary decisions (swap / co-resident / co-resident),
    override precedence + bogus-override fallthrough, decide-once caching, and
    hardware-change re-trigger. All pure-Rust (synthetic RAM values; no real probe).
  - **Pending Windows verification (user):** `cd src-tauri && cargo test`.

- **2026-06-29 — Phase B10 (LLM SOAP note generation) done.** In-process GGUF
  note generation (§8), built on the same testable-core / native-glue split as the
  recording pipeline so the state machine and prompt logic are unit-tested on Linux
  while only the `llama-cpp-2` binding needs the Windows build.
  - **State machine (`orchestrator/coordinator.rs`):** added a distinct `GENERATING`
    state (§8.4) — `IDLE ──generate──► GENERATING ──complete/cancel/fail──► IDLE`.
    `generate_note(record_id, transcript)` mirrors `stop_recording`: transition +
    emit `state-changed{GENERATING}` under the lock, release it for the blocking
    generation (so recording is blocked and a concurrent cancel can act), reacquire
    to emit `IDLE`. Resolves with the new note id (`None` if cancelled).
    `cancel_generation()` flips an `Arc<AtomicBool>` the running generator polls;
    the partial note is discarded. Both reject unless in the right state.
  - **`NoteGenerator` trait** (next to `Pipeline`): keeps the coordinator Tauri/
    store/model-free. Production `RealNoteGenerator` (`llm/generator.rs`) streams
    each piece as `generation-token{text}` and persists the result via
    `insert_note` (new active version, §8.5); cancel → nothing persisted.
  - **`llm/prompt.rs` (pure, tested):** zero-shot `SOAP_SYSTEM_PROMPT` — four fixed
    `##` headers, "use only information explicitly stated / do not add, assume, or
    infer", empty-section-kept rule (§8.3) — wrapped in the per-model instruct
    template (Mistral `[INST]` vs Phi `<|system|>…`).
  - **`llm/engine.rs` (native, `llama-cpp-2`):** `LlmModel::for_total_ram` picks
    Mistral-7B Q4_K_M (≥16 GB) vs Phi-3.5-mini Q8_0 (<16 GB) per §8.2; lazy load
    with an available-RAM guard (§8.4, fails gracefully to IDLE, no silent OOM),
    a hidden warmup pass after load, and a low-temperature streaming decode loop
    that stops on EOG / token cap / cancel.
  - **`store`:** added `update_note` (autosave edits in place, §8.5); `revert_version`
    reuses the existing `set_active_note`.
  - **Commands (§9.4):** `generate_note`, `regenerate_note` (identical — each is a
    new retained version), `cancel_generation`, `update_note`, `revert_version`.
    `generate_note`/`regenerate_note` load the record and **reject an empty
    transcript** (§8.1 guard) before transitioning.
  - **`lib.rs`:** picks the model from the same RAM probe (§8.2); co-resident →
    warm the model at startup, swap → load per generation and unload after
    (`swap_mode` into `RealNoteGenerator`).
  - **Deviations / follow-ups (surface-divergence rule):**
    - **`llama-cpp-2` binding pending first Windows compile.** The native API
      surface (sampler chain, `token_to_str`/`is_eog_token`, context params) is
      written to the ~0.1.x API but **not compiled here** (no Rust build on the
      Linux box, as with every native dep); the version may need a small bump/
      adjustment on the first `cargo build` on Windows.
    - **No note-read command in §9.4.** `generate_note` returns the note id so the
      UI has the just-generated note; listing prior versions for the revert UI is a
      F4 follow-up (no `list_notes`/`get_active_note` command was invented now).
    - **Swap-mode STT↔LLM handoff is LLM-side only.** B10 loads/unloads the *LLM*
      by residency mode; the design's "unload STT before loading LLM" half waits on
      STT model bundling (STT preload isn't wired yet — same gap as B5/B9), then
      becomes a small lib-level wiring step.
    - **Models not bundled** → a real generation surfaces an `error` until the
      asset-bundling phase, exactly as STT does today.
    - **`n_threads`** uses `available_parallelism` (logical cores) as a proxy for
      the physical-core target (§8.2 tuning, deferred to benchmarking).
  - **Deps added:** `llama-cpp-2 = "0.1.122"`.
  - **Tests (pure-Rust, run on Linux):** coordinator GENERATING walk + note-id
    return, generate-rejected-unless-IDLE, generation-failure → IDLE + `error`,
    cancel-rejected-when-not-generating, and a threaded cancel test (generation
    blocks while `cancel_generation` flips the flag → `None`, back to IDLE);
    prompt structure/anti-hallucination/per-model template; `store.update_note`
    edit-without-new-version. The native engine is exercised only by the manual
    Windows run.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test` (+ a
    manual run once a GGUF model is present produces a SOAP note).

- **2026-06-29 — B10 review fix (concurrency):** `generate_note`/`regenerate_note`
  are now `async` commands that run the blocking generation on
  `tauri::async_runtime::spawn_blocking` with an owned `Arc<Coordinator>`. The
  coordinator is now managed as `Arc<Coordinator>` (all coordinator command
  signatures take `State<'_, Arc<Coordinator>>`). Previously the sync command
  blocked Tauri's IPC thread for the whole multi-second generation, so
  `cancel_generation` could never dispatch and the window froze — the coordinator's
  lock-release-during-generation design and the threaded unit test only worked
  because the test moves generation off-thread. Now the IPC thread stays free, so
  cancel dispatches and the UI stays responsive (§8.4).

- **2026-06-29 — Phase B11 (EMR hand-off & crash reporting) done.** Split like B10:
  a pure, unit-tested core plus isolated native glue flagged for Windows verify.
  - **`handoff/parser.rs` (pure, tested).** `SoapSection` (S/O/A/P) with a stable
    lowercase `key()` for the Tauri boundary and `from_key`; `section_body()` — the
    deterministic §8.3 splitter that pulls one section's lines (header → next `## `
    header), strips bold/bullets to plain text, and trims. Tests: per-section
    extraction, body stops at next header, missing/empty section → empty, bullets
    stripped, numbered prefixes kept, header trailing-space/case tolerance.
  - **`store.active_note(record_id)`** (tested) — the current active note, so §8.6
    always pastes the latest edited/regenerated version.
  - **`handoff/mod.rs` (native).** `paste_section(record_id, section)` command:
    active note → `section_body` → clipboard (`tauri-plugin-clipboard-manager`) →
    Ctrl+V → timed clipboard self-clear (15 s, only if unchanged). Ctrl+V is Win32
    `SendInput` via the **existing `windows` crate** (added the
    `Win32_UI_Input_KeyboardAndMouse` feature) — no input-sim dependency.
    `register_paste_hotkey` registers the rebindable accelerator (default Alt+P from
    settings) and emits `handoff-requested` on press.
  - **`telemetry/mod.rs`.** `TechnicalContext` (app version/os/arch — no PHI) and
    `scrub_event` (recursively drops any PHI-named key: transcript/soap/note/label/
    record, at any depth, incl. arrays) are pure and tested. `init()` is behind the
    off-by-default `crash-reporting` cargo feature + a `MEDSCRIBE_CRASH_DSN` env var;
    when enabled it inits Sentry with `send_default_pii: false` and a `before_send`
    that serialize→scrub→deserialize-drops PHI.
  - **lib.rs/Cargo/capabilities:** registered the global-shortcut + clipboard
    plugins, `telemetry::init()` pre-builder, hotkey registration in setup (non-fatal
    on failure), `paste_section` in the invoke handler (now 14 commands); added
    `tauri-plugin-global-shortcut`/`-clipboard-manager`, optional `sentry`, the
    `[features] crash-reporting` flag, the windows keyboard feature; capability grants
    `global-shortcut:default` + clipboard read/write-text.
  - **Deviations / decisions:**
    - **No-activate overlay *window* + picker is F7 (frontend).** B11 ships the
      backend mechanism — the hotkey fires `handoff-requested`, F7's overlay window
      (with `WS_EX_NOACTIVATE` window config) listens and renders the S/O/A/P picker
      that calls `paste_section`. The focus-preservation window styling is a
      window-config concern that lands with the overlay window in F7.
    - **`paste_section` takes `record_id`** (the frontend knows the open record) and
      resolves the active note server-side, matching §8.6 "always the current active
      note version."
    - **Crash reporting is opt-in (feature + DSN), default off.** The default build
      is fully offline (NFR-6) and sends nothing; the user has no DSN yet, and gating
      keeps the (currently fragile) native build lean. Surfacing this for confirmation
      rather than forcing a network/Sentry dep into every build.
    - **Ctrl+V via Win32 `SendInput`** (reusing the `windows` crate) instead of a
      cross-platform input-sim crate — fewer deps; non-Windows is a compile stub.
  - **Deps added:** `tauri-plugin-global-shortcut`, `tauri-plugin-clipboard-manager`,
    optional `sentry` (feature `crash-reporting`), windows `Win32_UI_Input_KeyboardAndMouse`.
  - **Tests (pure-Rust):** parser (6), telemetry scrubber/context (2), `active_note`.
  - **Pending Windows verification (user):** `cd src-tauri && cargo test` (note the
    unresolved whisper.cpp↔llama.cpp ggml link clash still blocks the full build);
    manual: Alt+P shows the F7 overlay without focus steal, pastes the chosen section,
    clipboard clears. Sentry `before_send` round-trip is pending a first
    `--features crash-reporting` build.

- **2026-06-29 — Dropped Whisper STT for v1 (Parakeet-only).** Resolves the
  whisper.cpp↔llama-cpp-2 **ggml duplicate-symbol link error** (`LNK2005`/`LNK1169`)
  that blocked the full build after B10: both crates statically vendor their own
  ggml. Parakeet (ONNX) has no ggml, so the LLM now links cleanly. Changes:
  - `Cargo.toml`: `transcribe-rs` → `default-features = false, features = ["onnx"]`
    (whisper-cpp feature removed). *Verify on Windows that disabling defaults still
    pulls everything the ONNX/Parakeet engine needs.*
  - `stt/engine.rs`: removed the `whisper_cpp` import, the `LoadedEngine::Whisper`
    variant, the `ModelKind::Whisper` variant, the Whisper load + transcribe arms,
    and `validated_whisper_language` (+ its test). `ModelKind`/`LoadedEngine` stay
    single-variant enums so a future engine slots in without an interface change.
    Replaced the whisper-language test with a `base_lang` test (still used to drive
    the transcript-cleanup filter; Parakeet auto-detects the spoken language).
  - Doc comments in `stt/mod.rs`, `stt/transcriber.rs`, and `engine.rs` updated to
    Parakeet-only.
  - **Divergence from design §6.4** (Whisper listed as a selectable fallback) is
    recorded in Assumptions & Decisions above; whisper as a runtime option is
    deferred to a possible out-of-process-sidecar phase. The user's swap-residency +
    "expect a short delay" dialog idea would become that phase's UX, not a v1 change.

- **2026-06-30 — F1 built (app shell & Tauri bridge).** Frontend foundation:
  - `src/bridge/`: typed IPC surface in one place. `types.ts` mirrors the backend
    serde shapes (`Record`/`RecordSummary`/`Note`/`Settings`, the five §9.5 event
    payloads, `AppState`, `SoapSection`) in snake_case to match the wire format;
    `commands.ts` wraps every §9.4 command (camelCase Tauri arg keys); `events.ts`
    wraps the §9.5 listeners returning the `UnlistenFn` for cleanup; `index.ts`
    re-exports. Views/state import only from `@/bridge`, never `@tauri-apps/api`.
  - `src/state/`: one Zustand store (the "slices" pattern named in the design)
    with recording / transcript / notes / records / settings slices + a small UI
    slice for the active view. F1 sets shape + plain setters; F2–F6 wire events.
  - Shell: `App.tsx` (header + bridge-liveness dot carried from B1, nav + view
    switch), `components/NavBar.tsx`, and stub `views/{Recording,Records,Settings}View`
    for F2/F5/F6 to flesh out. Simple view-state nav, no router dependency (3 tabs).
  - Tooling: added `zustand`; added Vitest + Testing-Library + jsdom dev deps,
    `vitest.config.ts`, `src/test/setup.ts`, and `test`/`test:watch` scripts. Tests:
    `commands.test.ts` (every wrapper invokes the right command with the right args,
    incl. the camelCase mapping), `store.test.ts` (defaults + setters), `App.test.tsx`
    (shell renders, nav switches views). Test files excluded from the `tsc` build.
  - **Divergence from design §9.4:** `get_settings`/`update_settings` are specified
    but were never registered in the backend `invoke_handler` (only `settings::Settings`
    + load/save exist). The bridge wrappers are written to the §9.4 contract so the
    frontend is complete, but **these two backend commands must be added before F6
    (Settings view) can round-trip.** Flagged here and inline in `bridge/commands.ts`.
  - **Verification pending on Windows:** `bun install` then `bun test` (no Node/Bun
    on the Linux dev box) and `bun run build` (`tsc && vite build`).

- **2026-06-30 — Resolved the §9.4 settings-command gap (F1 follow-up).** Registered
  `get_settings`/`update_settings` so the F1 bridge wrappers no longer reject at
  runtime (they would have failed F6). Added `settings::SharedSettings` (a
  `SharedStore`-style `Arc<Mutex<Settings>>` + path handle) managed in state; the
  two commands read/persist through it. `update_settings` saves to disk first, then
  updates the cache (a failed write leaves the in-memory copy intact), and takes the
  full `Settings` object so internal keys survive the frontend round-trip. Verified
  by a new `shared_settings_update_persists_and_caches` unit test (Windows `cargo test`).

- **2026-06-30 — F2 built (Recording view).** Start/stop/pause/resume, live meter,
  status and error toasts:
  - `hooks/useBackendEvents.ts`: subscribes `state-changed` / `input-level` / `error`
    (§9.5) into the store once at the app root; views read state reactively and never
    touch the event layer. `transcript-segment` / `generation-token` deferred to F3/F4.
  - State: added `paused` to the recording slice (the backend has **no PAUSED state** —
    it stays RECORDING and emits no event on pause/resume per `coordinator.rs`, so the
    UI owns the flag) and a `toasts` slice (`pushToast`/`dismissToast`).
  - Components: `RecordingControls` (commands + state-derived button set; a rejected
    `Err(String)` becomes an error toast), `StatusBadge` (state label + "Paused"
    override), `LevelMeter` (one bar per `input-level` bucket, FR-12), `Toaster`
    (auto-dismiss, mounted globally in `App`). `RecordingView` composes them.
  - `App` now calls `useBackendEvents()` and renders `<Toaster/>`.
  - Tests: `RecordingView.test.tsx` (controls dispatch the right commands, Stop stores
    the returned record id, pause→Resume, rejected command→toast, status follows state,
    meter bar count = bucket count), `useBackendEvents.test.tsx` (events update the
    store), plus the event-module mock added to `App.test.tsx`.
  - **Verification pending on Windows:** `bun test` + `bun run build`.

- **2026-06-30 — F3 built (Live transcript editor).** Streaming segments → editable,
  debounced-saved transcript:
  - State: added `addSegment` to the transcript slice — inserts a `transcript-segment`
    in `seq` order (dedupes a repeated `seq` from STT retries) and mirrors the ordered
    segments into the editable `transcript` buffer. Segments only stream during
    RECORDING, so this never clobbers post-stop manual edits.
  - `hooks/useBackendEvents.ts`: now also subscribes `transcript-segment` → `addSegment`.
  - `components/TranscriptEditor.tsx`: a textarea bound to `transcript`; user edits
    debounce-save (600 ms) via `update_transcript`, guarded on `currentRecordId` (only
    set once `stop_recording` returns) — matching the record → stop → edit flow. Mounted
    in `RecordingView` (replaces the F3 placeholder).
  - `RecordingControls.onStart` clears the prior session's `segments`/`transcript`/
    `currentRecordId` before a fresh consult.
  - Tests: `TranscriptEditor.test.tsx` (renders merged text; debounced save fires with
    merged text once a record id exists; no save without a record id), store tests for
    `addSegment` ordering + dedupe, and a `transcript-segment` case in
    `useBackendEvents.test.tsx`.
  - **Verification pending on Windows:** `bun test` + `bun run build`.
