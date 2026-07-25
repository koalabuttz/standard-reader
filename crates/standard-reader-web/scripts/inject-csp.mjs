import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

const staging = process.env.TRUNK_STAGING_DIR;
if (!staging) {
  throw new Error("TRUNK_STAGING_DIR is not set");
}

const path = join(staging, "index.html");
let html = await readFile(path, "utf8");
// Trunk leaves indentation on otherwise-empty lines where asset markers were expanded. Normalize
// those lines here so the generated bundle remains clean in the source-controlled website repo.
html = html.replace(/[ \t]+$/gm, "");
const anchor = /<meta\s+name=["']sr-csp-anchor["']\s*\/?>/i;
if (!anchor.test(html)) {
  throw new Error("generated index.html is missing the CSP anchor");
}

// Hash exactly the inline script text the browser will see. Trunk's WASM bootstrap contains
// content-hashed asset names, so its CSP hash must be derived after every build.
const hashes = [];
const script = /<script\b(?![^>]*\bsrc\s*=)[^>]*>([\s\S]*?)<\/script>/gi;
for (const match of html.matchAll(script)) {
  const digest = createHash("sha256").update(match[1], "utf8").digest("base64");
  hashes.push(`'sha256-${digest}'`);
}
if (hashes.length === 0) {
  throw new Error("generated index.html contains no inline WASM bootstrap to authorize");
}

const policy = [
  "default-src 'none'",
  `script-src 'self' 'unsafe-eval' 'wasm-unsafe-eval' ${hashes.join(" ")}`,
  "style-src 'self' 'unsafe-inline'",
  "connect-src 'self' https:",
  "img-src 'self' blob: data:",
  "worker-src 'self' blob:",
  "object-src 'none'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-src 'none'",
].join("; ");
const meta = `<meta http-equiv="Content-Security-Policy" content="${policy}">`;
html = html.replace(anchor, meta);
await writeFile(path, html);
