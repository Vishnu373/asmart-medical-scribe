fn main() {
    // The prefix KV blob's filename carries the llama.cpp version (design §8.7), and the
    // only place the compiled-in version is stated is Cargo.lock — Cargo.toml holds a range,
    // and the lockfile isn't shipped so it can't be read at runtime.
    //
    // Stamped from `llama-cpp-sys-4`, not the `llama-cpp-4` wrapper: the sys crate is what
    // vendors llama.cpp and therefore what defines the `state_seq_get_data_ext` blob layout,
    // and the wrapper depends on it by range (`"0.6.1"`, i.e. ^0.6.1). Stamping the
    // wrapper would let a sys-only bump change the layout without changing the filename.
    println!("cargo:rerun-if-changed=Cargo.lock");
    let lock = std::fs::read_to_string("Cargo.lock").expect("Cargo.lock not readable");
    // Pre-fix, stamped the wrapper instead of the crate that owns the blob format:
    // let version = locked_version(&lock, "llama-cpp-2")
    //     .expect("llama-cpp-2 not found in Cargo.lock — the prefix KV cache cannot be versioned");
    // println!("cargo:rustc-env=LLAMA_CPP_2_VERSION={version}");
    // Pre-migration, stamped the llama-cpp-2 sys crate:
    // let version = locked_version(&lock, "llama-cpp-sys-2").expect(
    //     "llama-cpp-sys-2 not found in Cargo.lock — the prefix KV cache cannot be versioned",
    // );
    // println!("cargo:rustc-env=LLAMA_CPP_SYS_2_VERSION={version}");
    let version = locked_version(&lock, "llama-cpp-sys-4").expect(
        "llama-cpp-sys-4 not found in Cargo.lock — the prefix KV cache cannot be versioned",
    );
    println!("cargo:rustc-env=LLAMA_CPP_SYS_4_VERSION={version}");

    tauri_build::build()
}

/// Version of `name` as resolved in a Cargo.lock, i.e. the `version` line following
/// its `[[package]]` header.
fn locked_version(lock: &str, name: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() != format!("name = \"{name}\"") {
            continue;
        }
        for line in lines.by_ref() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("version = \"") {
                return v.strip_suffix('"').map(str::to_string);
            }
            if line.starts_with("[[package]]") {
                break; // no version before the next package: malformed
            }
        }
    }
    None
}
