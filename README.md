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