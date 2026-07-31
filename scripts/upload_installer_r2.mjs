// Post-build: publish a release to Cloudflare R2 via the S3-compatible API (AWS SDK,
// as documented). Sweeps superseded installers from the build output and the bucket
// first, then uploads this version's installer, its .sig, and the updater manifest.
// Run after `tauri build`.

import { existsSync, readdirSync, readFileSync, unlinkSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import {
  S3Client,
  PutObjectCommand,
  ListObjectsV2Command,
  DeleteObjectsCommand,
} from "@aws-sdk/client-s3";

const { ACCOUNT_ID, ACCESS_KEY_ID, SECRET_ACCESS_KEY, R2_BUCKET } = process.env;
for (const [k, v] of Object.entries({ ACCOUNT_ID, ACCESS_KEY_ID, SECRET_ACCESS_KEY, R2_BUCKET })) {
  if (!v) throw new Error(`${k} is not set (put it in .env)`);
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const nsisDir = join(root, "src-tauri/target/release/bundle/nsis");
// `bundle.targets: "all"` builds an MSI alongside the NSIS setup, in its own folder.
// Only the NSIS pair is published (that's what latest.json points at), but both pile
// up on disk across versions, so both are swept.
const msiDir = join(root, "src-tauri/target/release/bundle/msi");
// The updater manifest. Its bucket key is the endpoint `tauri.conf.json` polls, so
// this is the object that actually publishes a release — the repo copy is only source.
const manifestPath = join(root, "website/latest.json");
const MANIFEST_KEY = "latest.json";

/** True for a release artefact we manage: an NSIS or MSI installer, or its signature. */
const isInstaller = (name) =>
  /-setup\.exe(\.sig)?$/.test(name) || /\.msi(\.sig)?$/.test(name);

// The version being published. `tauri.conf.json` is what names the installer, so it —
// not package.json — decides which files count as current.
const { version } = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8"));
/** `_<version>_` is the tauri bundle naming convention, shared by both targets. */
const isCurrent = (name) => name.includes(`_${version}_`);

// Only the NSIS pair is uploaded; the MSI is swept but never published.
const files = readdirSync(nsisDir).filter((f) => isInstaller(f) && isCurrent(f));
if (files.length === 0) {
  throw new Error(`no ${version} installer in ${nsisDir} — did tauri build run for this version?`);
}

// Superseded builds across both bundle folders, as paths so they can be unlinked.
const localStale = [nsisDir, msiDir]
  .filter((dir) => existsSync(dir))
  .flatMap((dir) =>
    readdirSync(dir)
      .filter((f) => isInstaller(f) && !isCurrent(f))
      .map((f) => join(dir, f)),
  );

const s3 = new S3Client({
  region: "auto",
  endpoint: `https://${ACCOUNT_ID}.r2.cloudflarestorage.com`,
  credentials: { accessKeyId: ACCESS_KEY_ID, secretAccessKey: SECRET_ACCESS_KEY },
});

// — Sweep, local first.

console.log(`checking locally for older installer files (publishing ${version})`);
if (localStale.length === 0) {
  console.log("none found");
} else {
  console.log(`found ${localStale.length}, deleting`);
  for (const path of localStale) unlinkSync(path);
}

// Listing, not downloading: ListObjectsV2 returns keys only. Scoped by suffix, plus
// the manifest, so the model weights sharing this bucket are never candidates.
console.log("connecting to R2");
const listed = await s3.send(new ListObjectsV2Command({ Bucket: R2_BUCKET }));
const remoteKeys = new Set((listed.Contents ?? []).map((o) => o.Key));
const remoteStale = [...remoteKeys].filter(
  (key) => (isInstaller(key) && !files.includes(key)) || key === MANIFEST_KEY,
);

console.log("checking for older installer files and latest.json");
if (remoteStale.length === 0) {
  console.log("none found");
} else {
  console.log(`found ${remoteStale.length}, deleting`);
  await s3.send(
    new DeleteObjectsCommand({
      Bucket: R2_BUCKET,
      Delete: { Objects: remoteStale.map((Key) => ({ Key })) },
    }),
  );
}

// — Publish. The manifest goes last: between the sweep above and this upload the
// bucket has no manifest at all, so a client polling in that window gets a 404 and
// simply retries next launch — never a manifest pointing at a deleted installer.

console.log("uploading to R2");
for (const name of files) {
  // The key carries the version, so a name already in the bucket is this same release
  // — re-uploading would just burn the transfer. The manifest below is always rewritten.
  if (remoteKeys.has(name)) {
    console.log(`  ${name} — already exists, skipping`);
    continue;
  }
  await s3.send(
    new PutObjectCommand({ Bucket: R2_BUCKET, Key: name, Body: readFileSync(join(nsisDir, name)) }),
  );
  console.log(`  ${name}`);
}

await s3.send(
  new PutObjectCommand({
    Bucket: R2_BUCKET,
    Key: MANIFEST_KEY,
    Body: readFileSync(manifestPath),
    ContentType: "application/json",
  }),
);
console.log(`  ${MANIFEST_KEY}`);
