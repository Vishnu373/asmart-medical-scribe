# Setup & Build

Everything needed to build, run, test, and package **ASmart Medical Scribe** on Windows.

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
| **Vulkan SDK** | `glslc` compiles the LLM's GPU shaders | [vulkan.lunarg.com/sdk/home](https://vulkan.lunarg.com/sdk/home#windows) |
| **Perl + NASM** | Build OpenSSL from source | [Strawberry Perl](https://strawberryperl.com/) · [NASM](https://www.nasm.us/) |

> Set up the [Windows native dependencies](#-windows-native-dependencies) (OpenSSL, LLVM, CMake, Vulkan SDK) **before** running or testing the app.

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

### Vulkan SDK & the Ninja generator — for GPU note generation

Note generation runs on the GPU via llama.cpp's Vulkan backend (design §8.8). The build needs the **Vulkan SDK** installed — `glslc` compiles the GPU shaders into the binary. Install it from [vulkan.lunarg.com](https://vulkan.lunarg.com/sdk/home#windows) and confirm `%VULKAN_SDK%` is set.

> The runtime loader `vulkan-1.dll` is **not** in the SDK — the SDK ships only the link stub `Lib\vulkan-1.lib`. The loader lives in `C:\Windows\System32`, installed by your graphics driver, and a copy is committed to `src-tauri/libs/` so it ships with the app.
>
> If you ever re-copy it from `System32`, clear the read-only attribute it carries over — otherwise the build fails with `Access is denied. (os error 5)` when it copies the resource into the target directory:
>
> ```
> attrib -r src-tauri\libs\vulkan-1.dll
> ```

**Build from the Developer PowerShell for VS** (or the x64 Native Tools Command Prompt) — **not** a plain PowerShell:

```powershell
Get-Command cl.exe, ninja.exe | Select-Object Name, Source   # both must resolve
```

`src-tauri/.cargo/config.toml` sets `CMAKE_GENERATOR = "Ninja"`, and Ninja finds the compiler via `PATH`, which only the developer shell sets up. Nothing to install — Ninja ships with the Visual Studio Build Tools.

> **Why Ninja and not the default Visual Studio generator?** llama.cpp builds its shader compiler (`vulkan-shaders-gen`) as a nested CMake sub-project whose steps must run in order. Under the Visual Studio generator, parallel MSBuild starts those steps on different worker threads simultaneously — install before configure — and they race over the same directories. The build fails with a scatter of unrelated-looking errors (`MSB6003: link.exe could not be run`, `MSB1009: Project file does not exist`, `cannot find the batch label VCEnd`). Ninja sequences them correctly.

### Build output directory — `C:\ms`

Rust builds this project into **`C:\ms`**, not the usual `src-tauri/target/`. This is set in `src-tauri/.cargo/config.toml`, so it applies automatically — there is nothing to install or configure.

It is **required, not a preference.** Windows caps a file path at 260 characters. The Vulkan backend builds its shader compiler (`vulkan-shaders-gen`) as a nested sub-project, and those paths reach **~294 characters** under the default `src-tauri/target/`. The build fails on path length before it can finish. A short root recovers the ~66 characters needed:

| Target directory | Deepest build path | Result |
| --- | --- | --- |
| `src-tauri/target/` (default) | ~294 chars | ✗ exceeds the 260 limit |
| `C:\ms` | ~228 chars | ✓ builds |

> **Gotcha:** a `CARGO_TARGET_DIR` environment variable **overrides** this config file. If a build lands somewhere unexpected, check for a stale one:
>
> ```powershell
> $env:CARGO_TARGET_DIR      # should print nothing
> Remove-Item Env:CARGO_TARGET_DIR
> ```
>
> Environment changes only reach newly-started terminals, so open a fresh one after removing it.

`C:\ms` is disposable build cache — deleting it costs a full rebuild and nothing else. Clone the repo wherever you like; only the build output needs the short path.

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

## 📦 Release & packaging

Steps 2 → 5 are strictly ordered: the `.sig` doesn't exist until the build runs, and
`upload_installer` publishes whatever `website/latest.json` says at that moment.

### 0a. R2 credentials (one-time)

Both upload scripts read these from a gitignored `.env` at the repo root and refuse to run
without them:

```bash
ACCOUNT_ID=<cloudflare-account-id>
ACCESS_KEY_ID=<r2-access-key-id>
SECRET_ACCESS_KEY=<r2-secret-access-key>
R2_BUCKET=<bucket-name>
```

### 0b. Updater signing keypair (one-time)

Every release is signed so the updater can verify it. Generate the keypair once:

```bash
bunx tauri signer generate -w %USERPROFILE%\.tauri\asmart.key
```

| Output | Goes to |
|--------|---------|
| Private key + password | The `$env:TAURI_SIGNING_PRIVATE_KEY*` vars in [step 3](#3-build) |
| Public key | `plugins.updater.pubkey` in `src-tauri/tauri.conf.json` |

> ⚠️ Regenerating the keypair invalidates every installed client's update check — they can
> no longer verify new releases and must reinstall manually. Generate once, back it up, and
> don't rotate it casually.

The per-release `.sig` file is **not** made here — `tauri build` emits it automatically
(`bundle.createUpdaterArtifacts` is on).

### 1. Prepare the models

Only needed when the model weights change.

**Download** each model into `scripts/models/` — `upload_models` uploads that folder and
nothing else (any terminal; example uses Command Prompt):

```bash
curl -L -o scripts\models\<model_file_name> <model_file_url>
```

**Hash** it with SHA-256:

```bash
certutil -hashfile scripts\models\<model_file_name> SHA256
```

Copy the resulting hash into the matching model entry in:

```
src-tauri\src\models\mod.rs
```

**Upload the models files to R2:**

```bash
bun run upload_models
```

### 2. Bump the version

Update the version in **both** files:

| File | Why |
|------|-----|
| `src-tauri/tauri.conf.json` | Names the installer, and is the version `upload_installer` publishes |
| `package.json` | Keeps the frontend package in sync |

### 3. Build

```bash
$env:TAURI_SIGNING_PRIVATE_KEY="<your-key>"
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD="<your-key-password>"
bun run release
```

Output lands in `C:\ms\release\bundle\nsis\` (see [Build output directory](#build-output-directory--cms) — **not** `src-tauri/target/`):

```
asmart_medical_scribe_<version>_x64-setup.exe
asmart_medical_scribe_<version>_x64-setup.exe.sig
```

### 4. Update the updater manifest

Edit `website/latest.json` — this is the file that actually publishes the release (it's the
endpoint `tauri.conf.json` polls):

| Field | Value |
|-------|-------|
| `version` | The new version |
| `notes` | Release notes shown in the update prompt |
| `pub_date` | ISO 8601 timestamp |
| `signature` | The **contents** of the `.sig` file, not its filename |
| `url` | `https://<r2-public-domain>/asmart_medical_scribe_<version>_x64-setup.exe` |

### 5. Publish

Sweeps superseded installers locally and in the bucket, then uploads this version's
installer, its `.sig`, and `latest.json`:

```bash
bun run upload_installer
```

### 6. Local device testing (Windows VM)

Test the installer on a clean virtual machine before shipping.

**Install the hypervisor:**

| Host OS | VMware product | Link |
|---------|----------------|------|
| **Windows** | VMware Workstation Pro | [vmware.com/products/desktop-hypervisor](https://www.vmware.com/products/desktop-hypervisor/workstation-and-fusion) |
| **Mac** | VMware Fusion Pro | [vmware.com/products/desktop-hypervisor](https://www.vmware.com/products/desktop-hypervisor/workstation-and-fusion) |

1. Create an account on the VMware website.
2. Open the **Downloads** section of the dashboard and click **free software**.
3. Find the VM and install it.
4. For the ISO (used during VM setup), download the **Windows 11 English** image — [microsoft.com/software-download/windows11](https://www.microsoft.com/en-ca/software-download/windows11).