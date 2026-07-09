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

- 📐 Architecture → [`design.md`](./design.md) 

---

## ✨ What it does

- 🎙️ **Records & transcribes** a consult locally — no cloud, no PHI egress.
- 📝 **Generates a SOAP note** from the transcript with an on-device LLM.
- 🔒 **Encrypts everything at rest** (SQLCipher; key sealed by Windows DPAPI).
- 📋 **Hands off to your EMR** — paste any SOAP section into the focused field via a global hotkey.

---

## ✅ Prerequisites

Install these first — the native build won't compile without them.

| Tool | Why it's needed | Link |
|------|-----------------|------|
| **Bun** | Frontend package manager & test runner | [bun.com/docs/installation](https://bun.com/docs/installation) |
| **Rust + MSVC** | Backend toolchain | [rust-lang.org/tools/install](https://rust-lang.org/tools/install/) |
| **OpenSSL** | Linked by the encrypted DB (SQLCipher) | [github.com/openssl/openssl](https://github.com/openssl/openssl) |
| **LLVM (libclang)** | `bindgen` generates the LLM bindings | [llvm releases](https://github.com/llvm/llvm-project/releases/tag/llvmorg-22.1.0) |
| **CMake ≥ 4.1** | Drives the LLM's C++ build | [cmake.org/download](https://cmake.org/download/#latest) |
| **Perl + NASM** | Build OpenSSL from source | [Strawberry Perl](https://strawberryperl.com/) · [NASM](https://www.nasm.us/) |

> Set up the [Windows native dependencies](#-windows-native-dependencies) (OpenSSL, LLVM, CMake) **before** running or testing the app.

---

## 🚀 Quick start

```bash
# 1. Clone
git clone https://github.com/Vishnu373/asmart-medical-scribe.git
cd asmart-medical-scribe

# 2. Install frontend dependencies
bun install

# 3. Run the app in development (after the native deps below are set up)
bun run tauri dev
```

> If `bun run tauri dev` fails with a **Smart App Control** error, see [Troubleshooting](#-troubleshooting), then re-run it.

---

## 🪟 Windows native dependencies

The Rust backend compiles native code (encrypted DB + in-process LLM), so a few system libraries must be in place.

### OpenSSL — for the encrypted database

The encrypted database (SQLCipher) links against OpenSSL, built from source on Windows.

**Prerequisites** (both must be on your `PATH`):

- **Perl** — https://strawberryperl.com/
- **NASM** — https://www.nasm.us/ → select the **win32 / x64** download → latest version → run the `.exe` as administrator.

**Build & install** — open the **x64 Native Tools Command Prompt for VS** as administrator, clone OpenSSL *outside* the project folder, `cd` into it, then:

```bash
git clone https://github.com/openssl/openssl.git
# cd openssl
perl Configure VC-WIN64A no-shared enable-static-vcruntime
nmake
nmake test
nmake install
```

> Change the target architecture (`VC-WIN64A`) if required — see the OpenSSL repo docs.

**Point the build at it:**

```bash
set OPENSSL_DIR=C:\Program Files\OpenSSL
```

### LLVM (libclang) & CMake — for the LLM

The in-process note-generation engine (llama.cpp, via `llama-cpp-2`) is built from source. `bindgen` needs **libclang** (shipped with LLVM) to generate the Rust bindings, and **CMake** drives the C++ build.

```bash
# After installing LLVM, point bindgen at libclang:
setx LIBCLANG_PATH "C:\Program Files\LLVM\bin"
```

This project requires **CMake ≥ 4.1** (older versions can't target the installed Visual Studio toolchain).

> **Note:** Strawberry Perl ships its own older `cmake`. Make sure `C:\Program Files\CMake\bin` comes **above** `C:\Strawberry\c\bin` on your `PATH`, or the wrong `cmake` is picked up. Verify with `cmake --version`.

---

## 🧰 Development

```bash
# Add the Rust formatter and linter (one-time)
rustup component add rustfmt clippy

# Format check (no output = correctly formatted)
cargo fmt --check        # run inside src-tauri/

# Lint
cargo clippy             # run inside src-tauri/

# Type-check & bundle the frontend
bun run build
```

`rustfmt` formats Rust code; `clippy` is the Rust linter that flags common mistakes.

---

## 🧪 Testing

```bash
# Backend (Rust) — from src-tauri/
cd src-tauri
cargo test

# Frontend (Vitest) — from the project root
bun run test
```

---

## 🩹 Troubleshooting

<details>
<summary><b>Windows Smart App Control (SAC)</b></summary>

**Turn SAC OFF:**

1. Go to `Settings → Privacy & security → Windows Security → App & browser control → Smart App Control`.
2. If it's **ON**, switch it to **OFF**.

**Turn SAC back ON** (requires the Registry Editor):

1. Search **Registry Editor**, run as administrator.
2. Navigate to
   `HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\CI\Policy\VerifiedAndReputablePolicyState`.
3. Set the **Value** of `VerifiedAndReputablePolicyState` to `1`.
4. Re-check the SAC option in Windows Settings — it should now read **ON**.

> ⚠️ Turning SAC **off** has security trade-offs — review them before changing it.

</details>

---

## Uploading models
// to do section

1. Download the models to locally via command prompt or any terminal
For command prompt:
curl -L -o phi-q4.gguf  "hugging_face_url"

2. Hashing them via sha256 (need more info on this)
certutil -hashfile model_file_name SHA256

- Get the hashcode and paste it in each model section in the following file:
src-tauri\src\models\mod.rs

3. Run the script

4. 