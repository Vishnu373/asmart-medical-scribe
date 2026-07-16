// Post-build: upload the models to Cloudflare R2 via the
// aws-s3-multipart upload (AWS SDK, as documented). Run after `tauri build`.

import { readdirSync, createReadStream } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { S3Client } from "@aws-sdk/client-s3";
import { Upload } from "@aws-sdk/lib-storage";

const { ACCOUNT_ID, ACCESS_KEY_ID, SECRET_ACCESS_KEY, R2_BUCKET } = process.env;
for (const [k, v] of Object.entries({ ACCOUNT_ID, ACCESS_KEY_ID, SECRET_ACCESS_KEY, R2_BUCKET })) {
  if (!v) throw new Error(`${k} is not set (put it in .env)`);
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const modelsDir = join(root, "scripts/models");

const files = readdirSync(modelsDir, { withFileTypes: true })
  .filter((e) => e.isFile() && !e.name.startsWith("."))
  .map((e) => e.name);
if (files.length === 0) {
  console.log(`no models in ${modelsDir} — nothing to upload`);
  process.exit(0);
}

const s3 = new S3Client({
  region: "auto",
  endpoint: `https://${ACCOUNT_ID}.r2.cloudflarestorage.com`,
  credentials: { accessKeyId: ACCESS_KEY_ID, secretAccessKey: SECRET_ACCESS_KEY },
});

for (const name of files) {

  const upload = new Upload({
    client: s3,
    params: { Bucket: R2_BUCKET, Key: name, Body: createReadStream(join(modelsDir, name)) },
    partSize: 64 * 1024 * 1024,
    queueSize: 4,
  });
  upload.on("httpUploadProgress", (p) => {
    if (p.loaded && p.total) process.stdout.write(`\r${name}: ${Math.round((p.loaded / p.total) * 100)}%`);
  });
  await upload.done();
  console.log(`\nuploaded: ${name}`);
}
