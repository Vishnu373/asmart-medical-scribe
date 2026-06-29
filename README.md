# Medical Scribe

On-device medical scribe for Windows — records a doctor–patient consult, transcribes it locally, and generates a SOAP note. Built with **Tauri 2** (Rust backend + React/TypeScript frontend).

- Architecture: [`design.md`](./design.md)
- Build plan & progress: [`implementation.md`](./implementation.md)

---

## Setup

### 1. Install Bun

Documentation: https://bun.com/docs/installation

### 2. Navigate to the project folder

### 3. Install frontend dependencies

```
bun install
```

### 4. Install Rust and MSVC

Documentation: https://rust-lang.org/tools/install/

### 5. Run the app in development

```
bun run tauri dev
```

> If this fails with a Smart App Control error, see
> [Troubleshooting](#troubleshooting) below, then run this step again.

---

## Development tooling

`rustfmt` is the Rust code formatter; `clippy` is a compiler helper that acts as
an automated code reviewer for Rust.

### 6. Install rustfmt and clippy

```
rustup component add rustfmt
rustup component add clippy
```

### 7. Check formatting

```
cargo fmt --check
```

If no output, the code is correctly formatted.

### 8. Run the linter

```
cargo clippy
```

### 9. Build the frontend

```
bun run build
```

---

## Troubleshooting

### Windows Smart App Control (SAC)

**Turn SAC OFF:**

1. Navigate:
   `Settings -> Privacy & security -> Windows Security -> App & browser control -> Smart App Control (SAC)`
2. Check if it's **ON**.
3. Change it to **OFF**.

**Turn SAC back ON:**

- Search for **Registry Editor** in the Windows search bar.
- Run as administrator.
- Go to:
  `HKEY_LOCAL_MACHINE -> SYSTEM -> CurrentControlSet -> Control -> CI -> Policy -> VerifiedAndReputablePolicyState`
- In `VerifiedAndReputablePolicyState`, change the **Value** field to `1`.
- Go back to the SAC options in Windows Settings; you should see it set to **ON**.

> **Note:** There are trade-offs for turning OFF the SAC — please check before
> taking action.

---

## OpenSSL (Windows)

The encrypted database (SQLCipher) links against OpenSSL, which is built from
source on Windows. Documentation: https://github.com/openssl/openssl

### Prerequisites

Install both and ensure they're on your `PATH`:

- **Perl** — https://strawberryperl.com/
- **NASM** — https://www.nasm.us/
  > Select the **win32 / x64** download → latest NASM version → download the
  > `.exe` → install by running it as administrator.

### 10. Clone OpenSSL outside the project folder

```
git clone https://github.com/openssl/openssl.git
```

### 11. Build and install OpenSSL

Open **x64 Native Tools Command Prompt for VS** as administrator, `cd` into the
cloned `openssl` folder, then run:

```
perl Configure VC-WIN64A no-shared enable-static-vcruntime
nmake
nmake test
nmake install
```

> Change the target architecture (`VC-WIN64A`) if required — see the repo docs
> for details.

### 12. Set the OpenSSL environment variable

```
set OPENSSL_DIR=C:\Program Files\OpenSSL
```

---

## libclang & CMake (Windows)

The local STT engine builds `whisper.cpp` from source. `bindgen` needs
**libclang** (shipped with LLVM) to generate the Rust bindings, and **CMake**
drives the C++ build.

### 13. Install LLVM

Documentation: https://github.com/llvm/llvm-project/releases/tag/llvmorg-22.1.0

Then point `bindgen` at libclang:

```
setx LIBCLANG_PATH "C:\Program Files\LLVM\bin"
```

### 14. Install CMake

Documentation: https://cmake.org/download/#latest

This project requires **CMake >= 4.1** (older versions can't target the
installed Visual Studio toolchain).

> **Note:** Strawberry Perl ships its own, older `cmake`. Make sure
> `C:\Program Files\CMake\bin` comes **above** `C:\Strawberry\c\bin` on your
> `PATH`, otherwise the wrong `cmake` is picked up. Verify with:
>
> ```
> cmake --version
> ```

---

## Verify the build

### 15. Run the tests

From the project folder:

```
cd src-tauri
cargo test
```