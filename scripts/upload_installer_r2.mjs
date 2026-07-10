// Post-build: upload the NSIS installer (and its .sig) to Cloudflare R2 via the
// S3-compatible API (AWS SDK, as documented). Run after `tauri build`.
// Credentials come from the environment (bun auto-loads .env):
//   R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_BUCKET
import { readdirSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { S3Client, PutObjectCommand } from "@aws-sdk/client-s3";

const { ACCOUNT_ID, ACCESS_KEY_ID, SECRET_ACCESS_KEY, R2_BUCKET } = process.env;
for (const [k, v] of Object.entries({ ACCOUNT_ID, ACCESS_KEY_ID, SECRET_ACCESS_KEY, R2_BUCKET })) {
  if (!v) throw new Error(`${k} is not set (put it in .env)`);
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const nsisDir = join(root, "src-tauri/target/release/bundle/nsis");

// Upload the setup .exe and its .sig (the .sig is needed later for auto-update).
const files = readdirSync(nsisDir).filter(
  (f) => f.endsWith("-setup.exe") || f.endsWith("-setup.exe.sig"),
);
if (files.length === 0) throw new Error(`no installer found in ${nsisDir} — did tauri build run?`);

const s3 = new S3Client({
  region: "auto",
  endpoint: `https://${ACCOUNT_ID}.r2.cloudflarestorage.com`,
  credentials: { accessKeyId: ACCESS_KEY_ID, secretAccessKey: SECRET_ACCESS_KEY },
});

for (const name of files) {
  await s3.send(
    new PutObjectCommand({ Bucket: R2_BUCKET, Key: name, Body: readFileSync(join(nsisDir, name)) }),
  );
  console.log(`uploaded: ${name}`);
}
