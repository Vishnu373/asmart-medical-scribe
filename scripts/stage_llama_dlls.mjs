// Stage the llama.cpp shared libraries into `src-tauri/libs/` so the NSIS bundle
// ships them (tauri.conf.json maps `libs/*` to the install root, beside the exe).
//
// `llama-cpp-4`'s default features include `dynamic-link`, so llama/ggml link as
// DLLs. `llama-cpp-sys-4`'s build.rs hard-links them into the cargo target dir,
// which is enough for `tauri dev` and `cargo test` but invisible to the bundler.
// Run as Tauri's `beforeBundleCommand`, i.e. after the release build, so the
// staged DLLs always match the rlib they were compiled with — a stale copy is an
// ABI mismatch that crashes at model load, not at link time.
//
// The same reasoning covers OpenSSL, which the build links by an ABI-versioned
// filename (see below): it is staged from the build host, never committed.

import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const target = process.env.CARGO_TARGET_DIR
  ? resolve(process.env.CARGO_TARGET_DIR)
  : join(root, "src-tauri", "target");
const to = join(root, "src-tauri", "libs");

// Prefix match rather than a fixed list: the set shifts with llama.cpp's build
// (ggml-cpu, ggml-base, llama-common, …) and with feature changes. Everything
// else in the target dir — DirectML.dll from ort, the app's own lib — stays out.
const OWNED = /^(ggml|llama|mtmd)[-.\w]*\.dll$/i;
// OpenSSL, imported by the exe (SQLCipher via `openssl-sys`) and, unless
// `LLAMA_OPENSSL=OFF` took effect, by llama-common. Its DLL name carries the ABI
// version — `libcrypto-4-x64.dll` — so a committed copy stops matching the moment
// the host's OpenSSL is bumped, and the miss surfaces as an installed build that
// fails to start. Staged from the host install instead, like the llama DLLs.
const OPENSSL = /^lib(crypto|ssl)-[-.\w]*\.dll$/i;
const OURS = (f) => OWNED.test(f) || OPENSSL.test(f);

// The profile comes from Tauri, never from guessing which dir happens to exist:
// preferring `release` would stage yesterday's release DLLs beside a `--debug`
// exe, which is exactly the ABI mismatch this script exists to prevent.
const profile = process.env.TAURI_ENV_DEBUG === "true" ? "debug" : "release";
// The triple is always exported, but cargo only nests the profile under it when the
// build actually passed `--target`, so try the nested layout first.
const triple = process.env.TAURI_ENV_TARGET_TRIPLE;
const candidates = [
  triple && join(target, triple, profile),
  join(target, profile),
].filter(Boolean);

const built = candidates.filter((d) => existsSync(d));
if (built.length === 0) {
  throw new Error(
    `no ${profile} build output under ${target} — build before bundling`,
  );
}
const from = built.find((d) => readdirSync(d).some((f) => OWNED.test(f)));
if (!from) {
  throw new Error(
    `no llama/ggml DLLs in ${built.join(" or ")} — did dynamic-link get turned off?`,
  );
}

/** The DLL names in a PE file's import directory, i.e. what the loader must find. */
function importedDlls(file) {
  const b = readFileSync(file);
  const pe = b.readUInt32LE(0x3c);
  const nsec = b.readUInt16LE(pe + 6);
  const optSize = b.readUInt16LE(pe + 20);
  const opt = pe + 24;
  // PE32+ (magic 0x20b) holds 16 more bytes before the data directories than PE32.
  const dirs = opt + (b.readUInt16LE(opt) === 0x20b ? 112 : 96);
  const importRva = b.readUInt32LE(dirs + 8); // directory entry 1 is the import table
  const secs = [];
  for (let i = 0; i < nsec; i++) {
    const s = opt + optSize + i * 40;
    secs.push({
      va: b.readUInt32LE(s + 12),
      // Virtual size is 0 in some object layouts; raw size covers those.
      size: Math.max(b.readUInt32LE(s + 8), b.readUInt32LE(s + 16)),
      raw: b.readUInt32LE(s + 20),
    });
  }
  const at = (rva) => {
    const s = secs.find((s) => rva >= s.va && rva < s.va + s.size);
    return s ? s.raw + (rva - s.va) : -1;
  };
  const names = [];
  // Descriptors are 20 bytes each, terminated by an all-zero one.
  for (let o = at(importRva); o > 0; o += 20) {
    const nameRva = b.readUInt32LE(o + 12);
    if (nameRva === 0) break;
    const p = at(nameRva);
    if (p < 0) break;
    names.push(b.toString("latin1", p, b.indexOf(0, p)));
  }
  return names;
}

// Ask the build what it needs rather than assuming: read the import tables of every
// PE that ships in the install root. A static or `no-shared` OpenSSL leaves this
// empty and nothing is staged; a name change is followed automatically.
const linked = new Set();
for (const f of readdirSync(from).filter((f) => /\.(dll|exe)$/i.test(f))) {
  for (const dep of importedDlls(join(from, f))) {
    if (OPENSSL.test(dep)) linked.add(dep);
  }
}
// The install `openssl-sys` was pointed at (docs/setup.md) is by definition the one
// the build linked against; PATH is the fallback for a host that set neither.
const searchDirs = [
  process.env.OPENSSL_DIR && join(resolve(process.env.OPENSSL_DIR), "bin"),
  process.env.OPENSSL_LIB_DIR &&
    join(resolve(process.env.OPENSSL_LIB_DIR), "..", "bin"),
  ...(process.env.PATH ?? "").split(delimiter).filter(Boolean),
].filter(Boolean);
const openssl = [...linked].map((dll) => {
  const dir = searchDirs.find((d) => existsSync(join(d, dll)));
  if (!dir) {
    throw new Error(
      `${dll} is linked by the build but not found in %OPENSSL_DIR%\\bin or on PATH — ` +
        `set OPENSSL_DIR to the OpenSSL install (docs/setup.md)`,
    );
  }
  return [join(dir, dll), dll];
});

mkdirSync(to, { recursive: true });
const staged = readdirSync(from).filter((f) => OWNED.test(f));
// Clear our own namespace before staging. A DLL the current build no longer produces
// would otherwise survive here forever — never overwritten precisely *because* it is
// no longer produced — and `libs/*` would still ship it. Scoped to what we stage: the
// MSVC redistributables live in this directory too and are committed, not built.
for (const dll of readdirSync(to).filter(OURS)) {
  rmSync(join(to, dll));
}
for (const dll of staged) {
  copyFileSync(join(from, dll), join(to, dll));
}
for (const [src, dll] of openssl) {
  copyFileSync(src, join(to, dll));
}
console.log(
  `staged ${staged.length} llama.cpp DLLs into src-tauri/libs/: ${staged.join(", ")}`,
);
console.log(
  openssl.length === 0
    ? "no OpenSSL imports in the build — none staged"
    : `staged ${openssl.length} OpenSSL DLLs from ${dirname(openssl[0][0])}: ` +
        openssl.map(([, dll]) => dll).join(", "),
);
