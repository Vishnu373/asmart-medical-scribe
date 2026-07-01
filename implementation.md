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

### Phase F4 — SOAP note generation UI  `[x] done`
**Goal:** Trigger/stream/edit SOAP notes with versioning.
**Depends on:** B10, F3
**Tasks:**
- [x] Generate/regenerate/cancel buttons; stream `generation-token` into a SOAP view.
- [x] Four-section editor; save via `update_note`; version revert via `revert_version`.
**Deliverables:** `SoapEditor` + generation controls.
**Verification:** RTL — streaming tokens accumulate; cancel stops; revert calls
`revert_version`.

### Phase F5 — Records browser  `[x] done`
**Goal:** List/open/delete past consults and their notes.
**Depends on:** B8, F1
**Tasks:**
- [x] `RecordsView` listing from `list_records`; open loads transcript + notes.
- [x] Delete with confirm → `delete_record`.
**Deliverables:** `RecordsView`.
**Verification:** RTL — list renders from mocked data; open/delete invoke correct
commands.

### Phase F6 — Settings view  `[x] done`
**Goal:** Configure model, language, input device, hotkey, residency override.
**Depends on:** B5, B9, F1
**Tasks:**
- [x] Forms bound to `get_settings`/`update_settings`; STT/LLM model pickers;
      language (EN/FR); input device (from B3 enumeration); residency override.
- [x] Hotkey rebinding control for hand-off.
**Deliverables:** `SettingsView`.
**Verification:** RTL — load/save round-trip; invalid input guarded.

### Phase F7 — EMR hand-off (manual copy/paste)  `[x] done`
**Goal:** Get a SOAP section into the EMR. **Scope changed (user decision):** the
no-activate Alt+P picker overlay is deferred; v1 hand-off is manual copy/paste.
**Depends on:** B11, F4
**Tasks:**
- [x] ~~`overlay/` entry window; S/O/A/P picker calling `paste_section`~~ →
      per-section **Copy** button → `copy_to_clipboard`; clinician pastes with Ctrl+V.
- [x] ~~Minimal always-on-top styling; keyboard navigable~~ → Alt+P global hotkey
      not registered at startup (B11 machinery left dormant).
**Deliverables:** per-section Copy buttons in `SoapEditor`; `copy_to_clipboard` command.
**Verification:** RTL — Copy invokes `copy_to_clipboard` with the section text.

## Model Distribution

### Phase D1 — Bundle core models & optional model download  `[x] implemented (Windows verify pending)`
**Goal:** Ship the installer with the three core models embedded so the product
works offline on first launch, and make the lightest LLM tier an **optional**
download the doctor pulls on demand from the UI.
**Depends on:** B5, B9, B10, F6 (all done).

**What ships bundled (in the installer):**
- **Parakeet TDT v3** — the only STT engine (§8.4); always required.
- **Mistral-7B-Instruct-v0.3 Q4_K_M** — LLM "best" tier (≥16 GB machines, §8.2).
- **Phi-3.5-mini-instruct Q8_0** — LLM "medium" tier (<16 GB machines, §8.2).

**What is optional (downloaded on demand):**
- **Phi-3.5-mini-instruct Q4_K_M** — LLM "okay" tier. Not bundled (keeps the
  installer smaller). Exposed in Settings; if the doctor selects/clicks it and the
  file is absent, a download is triggered, verified, and then the tier becomes
  selectable.

**Backend tasks:**
- [x] `llm/engine.rs`: added `LlmModel::PhiQ4` ("okay") → file
      `phi-3.5-mini-instruct-Q4_K_M-worthdoing.gguf`, `footprint` ~2.4 GB. Also
      **corrected the two bundled filenames** to the real release names
      (`Mistral-7B-Instruct-v0.3-Q4_K_M.gguf`, `phi-3.5-mini-instruct-Q8_0-worthdoing.gguf`).
      `from_tier`/`from_choice` map `model_choice` → model; startup now picks the
      model from the chosen tier (RAM-fit fallback). `prompt.rs` treats PhiQ4 as the
      Phi-3.5 family template.
- [x] **Model path resolution:** `models::resolve(file, dirs)` searches
      `app_data_dir/models/` (download dir) **first**, then `resource_dir/models/`
      (bundled). `LlmEngine` now holds a dir search list; `ensure_loaded` resolves
      through it. (STT loader is **not** yet wired to a model path — see divergence;
      the resolver is ready for it.)
- [x] **Bundle config:** the existing `resources/**/*` glob already bundles
      anything dropped under `resources/models/`, so no `tauri.conf.json` change —
      the three core files (Parakeet dir, Mistral, Phi Q8) are placed there before
      `tauri build`. They stay git-ignored.
- [x] `download_model` command — native Rust `ureq` (rustls, no OpenSSL) streaming,
      not a shell script. Spawns a worker thread, streams to a `.part` temp in
      `app_data_dir/models/`, hashes with `sha2`, verifies (when a SHA is pinned),
      atomically renames into place. Emits throttled `model-download-progress
      { tier, downloaded, total }` and terminal `model-download-done` /
      `model-download-error`. The only network call in the app, gated on the click.
- [x] `model_status` command → presence of each tier on disk (bundled + downloaded).

**Frontend tasks:**
- [x] `SettingsView`: loads `model_status`; the "okay" `<option>` is disabled and a
      **Download** affordance (with live %, from `model-download-progress`) shows
      when the tier is absent, becoming selectable after `model-download-done`; a
      failed download toasts and reverts.
- [x] Bridge wrappers (`modelStatus`, `downloadModel`) + event listeners
      (`onModelDownloadProgress/Done/Error`) + `ModelStatus`/`ModelDownloadProgressEvent`
      types.

**Deliverables:** bundled core models in the installer; `download_model` +
`model_status` commands; Settings optional-download UI.
**Verification:** `cargo test` — path resolver prefers app-data over resource;
SHA-256 mismatch rejects the file; `model_status` reflects presence. Manual Windows:
fresh install transcribes + generates offline with no download; selecting "okay"
downloads, verifies, then generates. RTL — picker shows Download when absent,
selectable after `model-download-done`.
**Follow-up:** `design.md` needs a matching update (§8.2 tiers: which are bundled
vs optional; a model-distribution note; the single gated network call) — do via the
`system-design-doc` skill after this phase is approved.

### Phase D2 — Per-build bundling + both Phi tiers downloadable  `[x] implemented (Windows verify pending)`
**Goal:** Refine D1's distribution per the confirmed decision: a build bundles
Parakeet + **exactly one** RAM-fit LLM (not both LLMs), and **both** Phi tiers are
on-demand downloads so a doctor can pull whichever their build didn't bundle.
**Depends on:** D1 (done).

**Distribution decision (locked):**
- **≥16 GB build** bundles Parakeet + **Mistral-7B** ("best").
- **<16 GB build** bundles Parakeet + **Phi-3.5 Q8** ("medium").
- **Downloadable on either build:** Phi-3.5 Q8 ("medium") and Phi-3.5 Q4 ("okay").
  On a <16 GB build Phi Q8 is already bundled, so `model_status` reports it present
  and the UI never offers its download — the same generic UI serves both builds.
- Which GGUF is bundled is a **packaging step** (place the right file under
  `resources/models/` before `tauri build`); no runtime code enforces it.

**Backend tasks:**
- [x] `models/mod.rs`: `OPTIONAL` now lists **both** Phi tiers ("medium" Q8 +
      "okay" Q4), each with its HF URL; `sha256` still `None` (TODO pin). Module doc
      updated — one LLM bundled per build, both Phi tiers downloadable. No change to
      `download_model`/`model_status` (already tier-generic).
- [x] `llm/engine.rs`: enum doc updated so `Mistral` reads "bundled on ≥16 GB
      builds", `Phi` "bundled on <16 GB builds; downloadable elsewhere". No logic
      change — `for_total_ram`/`from_choice` already implement the RAM rule.

**Frontend tasks:**
- [x] `SettingsView`: the picker + Download affordance are now **generic over any
      optional-and-absent tier** (was hardcoded to "okay"). Download progress is
      tracked **per tier** (`Record<string, number>`) since both Phi tiers can be
      pulled. Each absent optional tier renders its own Download row with live %.
- [x] `bridge/commands.ts`: corrected the two "only `okay`" comments to name both
      downloadable Phi tiers.

**Verification:** `bun run test` — `SettingsView.test.tsx` gains a ≥16 GB-build case
(both Phi tiers absent → two Download buttons; clicking Q8 invokes
`download_model {tier:"medium"}`); the existing <16 GB case (Q8 bundled, only Q4
offered) still passes. `cargo test` — `optional_catalog_filenames_match_the_loader`
now also covers the "medium" entry. Manual Windows: on a ≥16 GB install both Phi
downloads appear; on a <16 GB install only Q4 does.
**Follow-up:** carries D1's open items — pin both Phi SHA-256s; update `design.md`
§8.2 for the per-build bundling.

### Phase D3 — First-run setup: download core models (lean installer)  `[x] implemented — Windows verify pending`
**Goal:** Ship a lean installer that carries **no** large model weights, and download
the core models **once** on first launch through a one-time Setup step. The app is
**gated** until the required models are on disk; afterwards they are cached and reused
every launch, fully offline. This **supersedes the D1/D2 bundling** of the LLM and STT
weights — the installer ships only the app + VAD; the LLM and Parakeet are fetched at
first run.
**Depends on:** D1 (download infra), D2 (tier-generic catalog/UI). Reuses both.

**What is downloaded on first run:**
- **The RAM-fit default LLM** — Mistral-7B on ≥16 GB, Phi-3.5 Q8 on <16 GB (§8.2 rule;
  already wired as downloadable in D1/D2). Single GGUF, reuses `download_model`.
- **Parakeet TDT v3** (STT) — required for the app to function at all.

**Parakeet download (the new capability D3 adds):**
- Source: `https://blob.handy.computer/parakeet-v3-int8.tar.gz`, sha256
  `43d37191602727524a7d8c6da0eef11c4ba24320f5b4730f1a2497befc2efa77`, ~456 MB.
- **⚠ Third-party host risk:** this URL is an external host **we do not control**. For
  a standalone product, relying on it for the core STT model is fragile (it can change
  or disappear, and it is out of our trust boundary). **Rehost the tarball on our own
  storage and swap the URL before release.** Using it as the interim default.
- Parakeet is a **directory** model, so the download path differs from the single-file
  GGUFs: stream the `.tar.gz` → `.part`, sha256-verify, then **extract** into
  `app_data_dir/models/`. Needs new deps **`flate2` + `tar`** (not currently in
  `Cargo.toml`); `download_to` streams a single file today.
- Parakeet is STT (`ModelKind`), not an LLM tier — it needs its **own catalog entry +
  download path**, separate from the `LlmModel`-tier `OPTIONAL`/`download_model`.

**Backend tasks:**
- [x] Add `flate2` + `tar` deps; a directory-download variant of `download_to` that
      verifies the archive sha256 then extracts to the models dir. Done: refactored
      the D1 stream/verify into a shared `stream_verified` helper; `download_stt_to`
      reuses it then `extract_model_dir` unpacks to a staging dir and renames the
      model root into place under `dir_name`.
- [x] STT download entry (url + sha256 + expected dir name) and a command to trigger
      it + report progress, mirroring the LLM events. Done: `models::STT` catalog +
      `download_stt` command; emits the same `model-download-*` events keyed by tier
      `"stt"`; shares the `IN_FLIGHT` guard.
- [x] A `setup_status` reporting whether the **required** set — RAM-fit LLM +
      Parakeet — is present, so the frontend can gate. Done: `setup_status` returns
      `{ llm_tier, llm_present, stt_present, ready }` (`llm_tier` from
      `LlmModel::for_total_ram(...).tier()`).

**Frontend tasks:**
- [x] First-run **Setup** view: on launch, if the required set is absent, show it,
      trigger the download(s) with live progress, and only release into the app once
      complete. Retry on failure. Skipped entirely when models are present. Done:
      `SetupView` (auto-starts the missing downloads, per-model progress bars, Retry
      on error, `onReady` when `setup_status.ready`); `App` gates on `setup_status`.

**Open items / decisions:**
- **Extracted dir name.** The loader resolves `parakeet-tdt-0.6b-v3`
  (`ModelKind::dir_name`); the tarball's directory is `parakeet-tdt-0.6b-v3-int8`.
  The extraction must land the files under the name the loader expects (rename on
  extract, or align `dir_name`). Confirm the archive's internal layout on Windows.
- **UX:** blocking Setup screen vs. background download with a progress banner; and
  behavior if the user quits mid-download (resume from `.part`).
- **Rehost Parakeet** (see risk above) before release.
- Carries D1/D2 open items: pin the three LLM SHA-256s; update `design.md` §8.2 (now
  "nothing bundled; core models downloaded on first run").

**Verification (once built):** first launch with an empty models dir shows Setup;
LLM + Parakeet download, verify (sha256), and extract; app reaches IDLE and
transcribes + generates offline. Relaunch skips Setup. Mid-download quit resumes on
next launch. `cargo test` covers the extraction/verify; `bun run test` covers gating.

## Progress Log

- **2026-07-01 — Phase D3 (first-run setup / lean installer) implemented; Windows verify pending.**
  Core models are now downloaded once on first launch and the app is gated until they
  are present. **Backend:** added `flate2` + `tar`; refactored the D1 download so the
  stream-hash-verify loop is a shared `stream_verified` helper (the LLM path renames
  the `.part` into place; the STT path extracts it). New `models::STT` catalog entry
  (Parakeet tarball URL + pinned sha256 + `dir_name`) and a `download_stt` command that
  streams → verifies → `extract_model_dir` (unpacks to a `.staging` dir, picks the model
  root via `single_subdir` — handling the archive's `…-int8` wrapper folder — and
  renames it to the loader's `parakeet-tdt-0.6b-v3`); it emits the same
  `model-download-*` events keyed by tier `"stt"` and shares the `IN_FLIGHT` guard.
  Added `LlmModel::tier()` (inverse of `from_tier`) and a `setup_status` command
  reporting `{ llm_tier, llm_present, stt_present, ready }`. Registered both commands.
  Rust tests: STT `dir_name` matches the loader; `single_subdir` wrapper detection.
  **Frontend:** new `SetupView` (auto-starts the missing required downloads, per-model
  progress bars, Retry on error, calls `onReady` once `setup_status.ready`); `App` gates
  the shell behind `setup_status`. Bridge: `SetupStatus` type, `setupStatus`/`downloadStt`
  wrappers. Tests: `SetupView.test.tsx` (auto-start + already-present release); updated
  `App.test.tsx` for the async gate. **Verified here:** `tsc` (my files clean — one
  pre-existing `soap.ts` `replaceAll` error unrelated) and the full `vitest` suite (58
  passing). **Not verified here:** `cargo test`/build (no Rust toolchain on the Linux box
  — run on Windows).
  - **⚠ Third-party host (carried).** The Parakeet URL is `blob.handy.computer` — an
    external host we do not control. Rehost the tarball on our own storage and swap
    `models::STT.url` before release.
  - **Open — confirm the archive layout on Windows.** `extract_model_dir` assumes the
    tarball either wraps its files in a single top-level folder or lays them at the
    root; verify against the real archive (and that `ParakeetModel::load` finds the
    ONNX files under `parakeet-tdt-0.6b-v3` after extraction).
  - **Open — mid-download quit does not resume.** A failed/aborted STT download deletes
    its `.part`; a retry re-downloads from zero (no HTTP range resume). Acceptable for
    v1; revisit if the ~456 MB restart proves painful.
  - **`design.md` §8.2 updated** to the "lean installer; required models downloaded once
    on first-run Setup, verified, cached; offline thereafter" model (new *Model
    distribution & first-run setup* block + a Distribution decision row). Consistent with
    §6.4's existing "downloaded once on first selection and cached".
  - **Open (carried) — pin the three LLM SHA-256s** (`models::OPTIONAL` still `sha256: None`).

- **2026-07-01 — All three LLM tiers now downloadable (Mistral "best" wired); verify pending.**
  Extended D2 so the download catalog covers every LLM tier, not just the two Phi
  ones. `models/mod.rs` `OPTIONAL` gains a `"best"` entry (Mistral-7B GGUF URL from
  `models.json`, `sha256: None` — TODO pin); doc comments in `models/mod.rs`,
  `llm/engine.rs`, and `bridge/commands.ts` updated to "all three tiers downloadable;
  each build bundles the RAM-fit default". No logic change — `download_model`/
  `model_status`/`SettingsView` were already tier-generic, so Mistral's Download row
  appears automatically when it's absent. Repurposed the former "not available in this
  version" test into a <16 GB-build case (Phi Q8 bundled → Mistral + Q4 both offered);
  note the `isAbsent`/`canDownload` split is now defensive only, since every *LLM*
  tier is downloadable (an absent LLM tier is always downloadable).
  - **Parakeet still bundle-only (not wired).** STT is a *directory* model: wiring its
    download needs (1) a real `.tar.gz` URL for the int8 folder — `models.json`'s
    `nvidia/parakeet-tdt-0.6b-v3` is a web page shipping a `.nemo`, wrong format;
    (2) tar.gz **extraction** (`flate2`+`tar` deps, not present; `download_to` streams
    a single file only); and (3) a **non-tier** download path (`OPTIONAL`/
    `download_model`/`model_status` are keyed to `LlmModel` tiers; Parakeet is a
    `ModelKind`). Blocked pending the archive URL.

- **2026-07-01 — Phase D2 (per-build bundling + both Phi tiers downloadable) implemented; Windows verify pending.**
  Refined D1's distribution: a build bundles Parakeet + one RAM-fit LLM (Mistral on
  ≥16 GB, Phi Q8 on <16 GB), and **both** Phi tiers are downloadable. `models/mod.rs`
  `OPTIONAL` now carries "medium" (Phi Q8) alongside "okay" (Phi Q4); backend
  `download_model`/`model_status` were already tier-generic so no logic changed.
  Frontend: `SettingsView` download UI generalized from a hardcoded "okay" to any
  optional-and-absent tier, with per-tier progress — this was the real gap (the
  backend offered Phi Q8 but the UI never surfaced its button). Doc comments in
  `llm/engine.rs` and `bridge/commands.ts` updated to "one bundled per build, both
  Phi tiers downloadable". Added a ≥16 GB-build test case to `SettingsView.test.tsx`.
  No new deps. Cannot `cargo test`/`bun run test` on the Linux box — verify on Windows.
  - **Follow-up fix (same day) — unpickable ≠ downloadable.** The picker disabled a
    tier only on `optional && !present`, so a tier this build neither bundles nor
    offers (e.g. Mistral "best" on a <16 GB build: `present:false, optional:false`)
    stayed freely selectable and failed at generation. Split the conditions:
    `isAbsent` (any `!present`) disables the option; `canDownload` (`optional &&
    !present`) gates the Download row. Non-downloadable absent tiers now read "not
    available in this version". Added a <16 GB-build test.
  - **Open item — pin both Phi SHA-256s.** `models::OPTIONAL` has `sha256: None` for
    "medium" and "okay"; downloads are integrity-checked only by HTTPS + the size
    check until the released files' hashes are captured and pinned.
  - **Open item — `design.md` §8.2** still describes D1's "two LLMs bundled"; needs a
    per-build-bundling update (deferred with D1's follow-up).

- **2026-06-30 — Phase D1 (model distribution) implemented; Windows verify pending.**
  Ship-with-models + optional download. New `models` module: on-disk resolver
  (download dir before bundled dir), `model_status`, and `download_model` (ureq +
  rustls streaming → `.part` temp → sha2 verify → atomic rename, throttled progress
  events). `llm/engine.rs`: added the PhiQ4 "okay" tier, **corrected the Mistral and
  Phi-Q8 filenames** to the real release names (the old constants would never have
  resolved), and made `model_choice` drive model selection via `from_choice`.
  Frontend: Settings gates/Downloads the optional tier; bridge wrappers + events +
  types added. New deps: `ureq`, `sha2`. Cannot `cargo test`/`bun run test` on the
  Linux box — verify on Windows.
  - **Divergence 1 — model selection now follows `model_choice`, not the RAM probe.**
    §8.2 picks the model purely fit-to-machine; the SettingsView model picker now
    overrides that. Side effect: the default tier `"medium"` means Phi-Q8 on *every*
    machine (previously a ≥16 GB box auto-picked Mistral). If "auto by RAM" should be
    a first-class tier/default, that's a small follow-up (seed `model_choice` from
    `for_total_ram` on first run, or add an "Automatic" option).
  - **Divergence 2 — residency still sizes for the RAM-fit model.**
    `residency::default_llm_footprint` uses `for_total_ram`, so if the doctor picks a
    model heavier than the RAM default, co-residency was sized for the lighter one.
    The §8.4 load-time available-RAM guard still protects against OOM; flagged for a
    later reconciliation (size residency from the *chosen* model).
  - **Divergence 3 — STT model load wired into the pipeline (resolved 2026-06-30).**
    Originally D1 was distribution only and the pipeline never called `SttEngine::load`.
    Now `ModelKind::dir_name()` owns the bundled Parakeet directory name
    (`parakeet-tdt-0.6b-v3`), `SttEngine::ensure_loaded(kind, &model_dirs)` resolves it
    through the D1 resolver (download dir → bundled resource dir), and
    `RealPipeline::start()` calls it before capture begins (no-op if already warm). A
    missing model now fails Start cleanly instead of failing per-segment. `RealPipeline`
    gained a `model_dirs` field, threaded from `lib.rs`.
  - **Open item — pin the Phi-Q4 SHA-256.** `models::OPTIONAL[okay].sha256` is `None`,
    so the download is integrity-checked only by HTTPS until the released file's hash
    is captured and pinned.

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

- **2026-06-30 — F4 built (SOAP note generation UI).** Generate → stream → edit →
  version-revert:
  - **Divergence (backend gap, resolved): no command exposed a record's notes.**
    Design §9.5 says GENERATING→IDLE "loads the active note" and §8.5 requires version
    revert, but §9.4 listed no note-read command and `open_record` returns only the
    `Record`. Added a `list_notes(record_id) -> Vec<Note>` Tauri command (mirrors
    `list_records`, wraps the existing `store.list_notes`, newest-first) and registered
    it. It covers both "load the active note" (the `is_active` row) and the revertable
    history. Same pattern as the earlier settings-command gap.
  - State: `notes` slice gained `appendStreamingToken` (accumulates `generation-token`
    into `streamingNote`); `useBackendEvents` now subscribes `generation-token`.
  - `lib/soap.ts`: pure `parseSoap`/`serializeSoap` between the backend's four-`##`-header
    markdown and `{subjective,objective,assessment,plan}` (round-trips byte-compatibly;
    empty section → bare header, per the prompt contract).
  - `components/SoapEditor.tsx`: four labeled textareas for the active note; edits
    debounce-save (600 ms) via `update_note` with the sections reassembled to markdown,
    flushing on unmount and on note switch (regenerate/revert) so no edit is lost.
  - `components/NotePanel.tsx`: Generate/Regenerate (creates a new active version) +
    Cancel; live streaming `<pre>` during GENERATING; the four-section editor + a version
    list with per-version Revert otherwise. Loads notes via `list_notes` after a
    generation resolves (non-null id) and after a revert. Mounted in `RecordingView`.
  - `RecordingControls.onStart` also clears `notes`/`streamingNote` for a fresh consult.
  - Tests: `lib/soap.test.ts` (parse/serialize round-trip + bare header), `SoapEditor.test.tsx`
    (renders 4 sections, debounced `update_note` with reassembled markdown, unmount flush),
    `NotePanel.test.tsx` (generate→list_notes→editor, streaming view + cancel, revert→
    `revert_version`), plus store `appendStreamingToken` and a `generation-token` case in
    `useBackendEvents.test.tsx`.
  - **Verification pending on Windows:** `bun run test` + `bun run build`; `cargo test`
    for the new `list_notes` command.

- **2026-06-30 — F5 built (Records browser).** Saved-encounter list with open/delete:
  - `views/RecordsView.tsx`: loads `list_records` on mount into the `records` slice
    and renders each as a row (label, localized `created_at`, language). Empty state when
    none.
  - **Open** calls `open_record` then `list_notes`, loads the `Record.transcript` +
    note versions into the store (`currentRecordId`/`transcript`/`notes`, `segments`
    cleared, `streamingNote` reset) and switches `view` to recording — reusing the
    Recording view's transcript/SOAP editors, which are already keyed off
    `currentRecordId` (state stays IDLE, so both are editable). No new detail view needed.
  - **Delete** is a per-row inline two-step confirm (Delete → Confirm/Cancel) rather than a
    `window.confirm`, calling `delete_record` then refreshing the list. Permanent (NFR-9);
    the backend cascades notes (B8).
  - Tests: `RecordsView.test.tsx` — list renders from mocked `list_records`; Open invokes
    `open_record`+`list_notes` and populates the store/view; Delete only fires
    `delete_record` after the inline Confirm.
  - **Verification pending on Windows:** `bun run test` + `bun run build`.

- **2026-06-30 — F5 follow-up: lock nav while busy.** Opening a record mid-RECORDING
  would repoint `currentRecordId`/`transcript` while the backend kept streaming
  `transcript-segment` into the original session, corrupting state (and the F4 NotePanel
  shared the same nav-during-recording gap). Fixed at the nav level: `NavBar` disables
  every non-active view button while `recordingState !== "IDLE"`, keeping the clinician
  on the Recording view until the session returns to IDLE. Test: `NavBar.test.tsx` (nav
  works when idle; Records is disabled and a no-op during RECORDING).

- **2026-06-30 — F6 built (Settings view).** Doctor-facing config bound to
  `get_settings`/`update_settings`:
  - `views/SettingsView.tsx`: loads settings + the device list on mount into a local
    edit buffer; **Save** spreads the loaded object so internal keys
    (`residency_mode`/`observed_total_ram`/`residency_calc_version`/`vad_threshold`/
    `idle_timeout`) survive the read-modify-write. Controls: Note model (best/medium/okay,
    §9.3), Microphone (System default + enumerated devices; stores the chosen `name`,
    null = default), Paste hotkey (focus-and-press capture, formats e.g. `Ctrl+K`), Model
    residency override (Automatic/co_resident/swap).
  - **Backend addition:** exposed `list_input_devices` as a Tauri command (`commands::
    InputDevice { name, is_default }`, wraps the existing `audio_toolkit::list_input_devices`
    dropping the non-serializable cpal handle) + registered it + bridge `listInputDevices`.
    The enumeration existed since B3 but no §9.4 command surfaced it — same gap class as the
    earlier `list_notes`/settings-command additions.
  - Bridge: `InputDevice` type + `listInputDevices()` wrapper.
  - **Divergences from the F6 task / design (surfaced, not silently filled):**
    (i) **No language (EN/FR) picker** — `Settings` has no language field and §9.3 lists the
    doctor keys as Model/Mic/Paste-key only; language is per-segment auto-detect (FR-5,
    Parakeet) and was deferred at B5/B7, so there is nothing to bind. Not added.
    (ii) **No greying of unrunnable model tiers** (§9.3 'options the machine can't run are
    greyed out') — there is no per-choice feasibility query (B9 decides one mode, not
    per-tier runnability); all three tiers stay selectable. Deferred.
    (iii) **One model picker, not separate STT/LLM pickers** — the backend models a single
    `model_choice`; STT (Parakeet) is fixed for v1.
    (iv) `residency_override` is surfaced as a doctor control even though the §9.3 table omits
    it; the `Settings` struct doc marks it doctor-facing and the F6 task lists it.
  - Tests: `SettingsView.test.tsx` — load (settings + device list), save round-trip
    asserting the merged full object (internal keys preserved), hotkey capture
    (Ctrl+K), and the no-modifier guard (rejected, value unchanged).
  - **Verification pending on Windows:** `bun run test` + `bun run build`; `cargo test`
    for the new `list_input_devices` command (cpal enumeration needs the Windows host).

- **2026-06-30 — F6 follow-up: live hotkey rebind.** `register_paste_hotkey` ran only
  once at startup, so a Settings rebind needed an app relaunch to take effect. Added a
  `rebind_paste_hotkey(accelerator)` Tauri command (`handoff/mod.rs`: `unregister_all`
  then re-register) + registered it + bridge `rebindPasteHotkey`; `SettingsView.onSave`
  calls it after persisting so the new combo binds live. A rebind failure (combo taken)
  is toasted without undoing the saved settings. Also fixed the Meta-key token: the
  capture now emits `Super` (the accelerator parser has no `Win` arm). Test updated to
  assert the `rebind_paste_hotkey` call on save.

- **2026-06-30 — F7 built (manual EMR hand-off).** Scope changed by user decision: the
  §8.6 no-activate overlay + Alt+P auto-paste are **deferred**. The clinically-correct
  behavior (a pop-up that never steals focus so the EMR field keeps the caret, navigated
  by backend-forwarded global keys) is largely native Windows-only work — WS_EX_NOACTIVATE
  window styling Tauri config doesn't expose, plus a register/forward/unregister global-key
  system B11 never built — none of which is verifiable on the Linux dev box. Rather than
  ship unverifiable native code, v1 hand-off is **manual**:
  - `SoapEditor`: a per-section **Copy** button copies that section's current (live, incl.
    unsaved) text to the clipboard; the clinician pastes into the focused EMR field with
    Ctrl+V. Disabled for empty sections; toasts '<Section> copied'.
  - Backend `handoff::copy_to_clipboard(text)` (clipboard write, no keystroke, no auto-clear
    — the clinician controls paste timing, so a timed wipe could clear it too early) +
    registered + bridge `copyToClipboard`.
  - **Alt+P removed for now:** the startup `register_paste_hotkey` call is gone, so no global
    shortcut is grabbed. B11's `paste_section`/`register_paste_hotkey`/`rebind_paste_hotkey`
    stay in place, dormant, for when the overlay is built. The F6 Settings paste-hotkey
    control + its live-rebind-on-save were removed (the value still persists in settings for
    the future); SettingsView tests updated accordingly.
  - Tests: `SoapEditor.test.tsx` — Copy invokes `copy_to_clipboard` with the section text.
  - **Follow-up (when on Windows):** build the real no-activate overlay + global-key nav +
    record-id plumbing to restore the one-key §8.6 hand-off.
  - **Verification pending on Windows:** `bun run test` + `bun run build`; `cargo test`.
