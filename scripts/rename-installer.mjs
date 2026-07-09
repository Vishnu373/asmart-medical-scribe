// Post-build: rename Tauri's installer artifacts from the productName-derived
// name (e.g. "ASmart Medical Scribe_0.1.0_x64-setup.exe") to a stable slug form
// ("asmart-medical-scribe-0.1.0-x64-setup.exe"). Tauri has no config to set the
// installer filename (tauri-apps/tauri#13999), so we rename after `tauri build`.
//
// Renames .exe, .exe.sig, .msi, .msi.sig alike. Signatures stay valid — they
// sign file *contents*, not the filename. It then regenerates website/latest.json
// so the auto-updater's `url` + `signature` match the renamed NSIS artifact.
import { readFileSync, writeFileSync, readdirSync, renameSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const conf = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8"));
const productName = conf.productName; // "ASmart Medical Scribe"
const version = conf.version; // "0.1.0"
const slug = productName.toLowerCase().replaceAll(" ", "-"); // "asmart-medical-scribe"
// R2 base URL is derived from the updater endpoint so it stays a single source of truth.
const base = conf.plugins.updater.endpoints[0].replace(/latest\.json$/, "");

const bundleRoot = join(root, "src-tauri/target/release/bundle");
const dirs = ["nsis", "msi"].map((d) => join(bundleRoot, d));

let renamed = 0;
let nsisDir = null;
let nsisExe = null; // renamed setup .exe filename, used for latest.json
for (const dir of dirs) {
  let files;
  try {
    files = readdirSync(dir);
  } catch {
    continue; // this bundle target wasn't produced
  }
  for (const name of files) {
    if (!name.startsWith(productName + "_")) continue;
    // Swap productName for the slug, then flatten remaining "_" separators to "-".
    const next = name.replace(productName, slug).replaceAll("_", "-");
    renameSync(join(dir, name), join(dir, next));
    console.log(`renamed: ${name} -> ${next}`);
    renamed++;
    if (dir.endsWith("nsis") && next.endsWith("-setup.exe")) {
      nsisDir = dir;
      nsisExe = next;
    }
  }
}
if (renamed === 0) console.log("no installer artifacts found to rename");

// Regenerate website/latest.json against the renamed NSIS installer + its .sig.
if (nsisExe) {
  const signature = readFileSync(join(nsisDir, nsisExe + ".sig"), "utf8").trim();
  const manifest = {
    version,
    notes: `ASmart Medical Scribe ${version}.`,
    pub_date: new Date().toISOString(),
    platforms: {
      "windows-x86_64": { signature, url: base + nsisExe },
    },
  };
  writeFileSync(join(root, "website/latest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`latest.json updated -> ${nsisExe}`);
} else {
  console.log("no NSIS installer found — latest.json left unchanged");
}
