# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

On-device medical scribe for **Windows 11** (CPU-only, no GPU). Records a doctor–patient consult, transcribes it locally, and generates a SOAP-R note. **No PHI ever leaves the device** — all audio capture, transcription, and note generation run locally; the only outbound calls are one-time model-weight downloads and the app-update check.

Tauri 2 app: **Rust backend** (`src-tauri/`) + **React/TypeScript frontend** (`src/`), package-managed with **Bun**. The authoritative architecture spec is [`docs/design.md`](docs/design.md) — sections are cited throughout the code as `§6.4`, `§8.2`, etc.; read the referenced section before changing the code that cites it. Build/packaging steps live in [`docs/setup.md`](docs/setup.md).

## Commands

Frontend (from repo root):
```bash
bun install
bun run tauri dev          # run the full app in dev
bun run build              # tsc typecheck + vite bundle
bun run test               # vitest run (frontend unit tests)
bun run lint               # eslint src   (lint:fix to autofix)
bunx vitest run src/lib/soap.test.ts              # single test file
bunx vitest run src/lib/soap.test.ts -t "name"    # single test by name
```

Backend (from `src-tauri/`):
```bash
cargo test                 # all backend tests
cargo test <name>          # single test by substring match
cargo clippy               # linter
cargo fmt --check          # format check
```

Whole-repo format (both stacks): `bun run format` / `bun run format:check`.

Release/packaging (`bun run release`, `upload_models`, `upload_installer`) requires signing keys and R2 credentials — see `docs/setup.md`. Do not run these unless explicitly asked.

## Platform constraint

The native Rust build only compiles on **Windows with MSVC** and requires OpenSSL, LLVM/libclang, and CMake ≥ 4.1 installed and on `PATH` (see `docs/setup.md` → "Windows native dependencies"). `cargo` commands and `bun run tauri dev` will not build in a plain Linux/WSL shell. Frontend-only commands (`bun run test`, `bun run lint`, `bun run build`) run anywhere.

## Architecture

### Frontend ↔ backend bridge
All IPC goes through `src/bridge/` — typed `invoke` wrappers (`commands.ts`), typed event listeners (`events.ts`), shared payload types (`types.ts`). **Views and state never import `@tauri-apps/api` directly** — the entire IPC surface stays in this one typed place. The backend's registered commands are the `invoke_handler![...]` list in `src-tauri/src/lib.rs`; the event contract is documented in design §9.5.

Frontend state is a single **Zustand** store (`src/state/store.ts`) split into slices (recording / transcript / notes / records / settings / ui). Backend events are wired into the store once at the root via `useBackendEvents` (`src/hooks/`). Path alias `@/` → `src/`.

### Backend pipeline (the core)
A recording is a state machine — **IDLE → RECORDING → PROCESSING → IDLE** — owned by the **orchestrator** (`src-tauri/src/orchestrator/coordinator.rs`, design §6.6). The UI only *requests* transitions (`start_recording`/`stop_recording`); the coordinator owns the actual state and guards against illegal/duplicate transitions. Data flows across parallel threads:

- **`audio_toolkit/`** — mic capture via `cpal`, resample to 16 kHz mono `f32` (`rubato`), and Silero neural VAD (`vad/`) to detect speech vs. silence (design §6.1–6.2).
- **`segment/`** — buffers speech into utterances, cuts a segment at each VAD pause (or a max-duration cap), and hands finished segments to a transcription **worker** thread over an mpsc queue (design §6.3).
- **`stt/`** — the Parakeet TDT v3 engine (ONNX, CPU) behind a swappable `transcribe(audio) -> text` interface; kept warm during use and idle-unloaded after `STT_IDLE_TIMEOUT` (design §6.4). `stt/mock.rs` is the test double.
- Finished segments are pushed to the UI as `transcript-segment` events with a sequence number — **append-only; the frontend editor owns the document** so clinician edits are never clobbered (design §6.5).

### Note generation (post-recording)
- **`llm/`** — in-process GGUF inference via `llama-cpp-2` (no server, no network). `generator.rs` produces the SOAP-R note on explicit **Generate**; `prompt.rs` holds the SOAP schema / anti-fabrication prompt; `engine.rs` owns the loaded model (shared `Arc<LlmEngine>`).
- **Model residency** — **co-resident always** (design §7): both STT and LLM stay warm for the life of the process. Targets a 16 GB (or higher) machine within the ~12 GB budget; there is no per-device mode decision or swap (no RAM probe).
- **`models/`** — resolves model files across the download dir then the bundled resource dir; downloads required models on first run (**Setup** gate) and verifies each against a SHA-256 checksum. Installer ships no LLM/STT weights (only the small VAD model).

### Persistence & security
- **`store/`** — encrypted clinical DB via `rusqlite` with `bundled-sqlcipher` (AES-256). Transcripts and notes persist across sessions; audio is never written to disk.
- **`crypto/`** — the DB key is wrapped by **Windows DPAPI** (`db.key`), never persisted in the clear; transient plaintext key copies are zeroized on drop (design §10.1).
- **`settings/`** — `settings.json` in the app data dir; `model_choice`, residency mode, cached total-RAM.

### Other backend modules
- **`handoff/`** — EMR clipboard hand-off; per-section Copy in v1. The global paste-hotkey machinery (`paste_section`, `rebind_paste_hotkey`) is present but **dormant** — no shortcut is registered at startup.
- **`telemetry/`** — opt-in crash reporting, compiled out unless the `crash-reporting` cargo feature + a DSN are present (offline by default). `bun run release` builds with this feature.
- **`trial/`** — compiled-in beta expiry, **removed**: `trial.rs` and `ExpiredView.tsx` are still on disk but no longer wired in (`mod trial` and the `trial_status` command are commented out). The app never expires.

## Other directories
- `landing_page/` — a **separate** Vite/React marketing site with its own `package.json`; unrelated to the app.
- `website/latest.json` — the Tauri updater manifest for auto-updates.
- `scripts/` — R2 upload scripts for installers and model weights.

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
