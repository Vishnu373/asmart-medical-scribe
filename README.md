# ASmart Medical Scribe

> On-device medical scribe for Windows — records a doctor–patient consult, transcribes it locally, and generates a SOAP note. **Nothing leaves the machine.**

Built with **Tauri 2** (Rust backend + React / TypeScript frontend), fully offline and CPU-only.

| | |
|---|---|
| **Platform** | Windows 11 (MSVC, CPU-only) |
| **Stack** | Tauri 2 · Rust · React + TypeScript · Bun |
| **Speech-to-text** | Parakeet TDT v3 (ONNX, on-device) |
| **Note generation** | llama.cpp (GGUF, in-process) |
| **Storage** | SQLCipher (encrypted) + Windows DPAPI-wrapped key |

- 📐 Architecture → [`design.md`](docs/design.md)
- 🔧 Setup, build & packaging → [`setup.md`](docs/setup.md)

---

## ✨ What it does

- 🎙️ **Records & transcribes** a consult locally — no cloud, no PHI egress.
- 📝 **Generates a SOAP note** from the transcript with an on-device LLM.
- 🔒 **Encrypts everything at rest** (SQLCipher; key sealed by Windows DPAPI).
- 📋 **Hands off to your EMR** — paste any SOAP section into the focused field via a global hotkey.

---

## 🚀 Quick start

```bash
# 1. Clone
git clone https://github.com/Vishnu373/asmart-medical-scribe.git
cd asmart-medical-scribe

# 2. Install frontend dependencies
bun install

# 3. Run the app in development (after the native deps are set up)
bun run tauri dev
```

---

## 📁 Project structure

Two halves: a **Rust backend** that owns the recording pipeline, and a **React frontend** that only displays state and requests transitions. They talk over a single typed IPC bridge.

### `src-tauri/` — Rust backend

Organized one folder per domain. The pipeline runs top-to-bottom:

| Folder | What it does |
|---|---|
| `audio_toolkit/` | Mic capture (`cpal`), resample to 16 kHz mono, and Silero neural **VAD** to tell speech from silence. |
| `segment/` | Buffers speech into utterances, cuts a segment at each pause, and hands it to a worker thread for transcription. |
| `stt/` | Speech-to-text engine (Parakeet TDT v3, ONNX). `mock.rs` is the test double. |
| `llm/` | On-device SOAP note generation — the loaded GGUF model (`engine.rs`), the generation loop (`generator.rs`), and the prompt (`prompt.rs`). |
| `orchestrator/` | Owns the `IDLE → RECORDING → PROCESSING` state machine and wires the threads together. |
| `models/` | Finds model files on disk, downloads them on first run, verifies SHA-256. |
| `store/` | Encrypted SQLite (SQLCipher) — transcripts and notes. Audio is never written to disk. |
| `crypto/` | Seals the database key with Windows DPAPI so it never sits on disk in the clear. |
| `settings/` | `settings.json` — model choice and user preferences. |
| `handoff/` | Copies SOAP sections to the clipboard for pasting into an EMR. |
| `telemetry/` | Opt-in crash reporting. Compiled out by default. |
| `commands/` | Every function the frontend is allowed to call. |
| `lib.rs` · `main.rs` · `trial.rs` | App setup and command registration; beta expiry check. |

Also here: `models/silero_vad_v4.onnx` (the only weights shipped in the installer), `libs/` (Windows runtime DLLs), `tauri.conf.json` (build & bundle config).

### `src/` — React frontend

Organized by kind rather than by feature — a small app, so everything of one type lives together:

| Folder | What it does |
|---|---|
| `bridge/` | **The only place that touches Tauri.** Typed `invoke` wrappers, event listeners, shared payload types. |
| `state/` | One Zustand store, split into slices (recording / transcript / notes / records / settings / ui). |
| `hooks/` | `useBackendEvents` pipes backend events into the store; plus update-check and UI helpers. |
| `views/` | Full screens — Recording, Records, Settings, Setup, Expired. |
| `components/` | Reusable pieces used by the views — editors, nav bar, status badge, level meter, toasts. |
| `lib/` | Pure helpers, e.g. SOAP section parsing. |

### Everything else

| Path | What it does |
|---|---|
| `docs/` | [`design.md`](docs/design.md) — the authoritative spec, cited from the code as `§6.4` etc. [`setup.md`](docs/setup.md) — build & packaging. |
| `scripts/` | Uploads installers and model weights to R2. |
| `website/` | `latest.json`, the auto-update manifest. |
| `graphify-out/` | Generated knowledge graph of the codebase. |