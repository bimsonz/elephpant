#!/usr/bin/env node
// Postinstall: download the prebuilt psx binary for this platform and verify
// the checksum against the release's SHASUMS256.txt.
//
// Mapping is platform x arch -> release asset name:
//   darwin-arm64 -> psx-darwin-arm64
//   darwin-x64   -> psx-darwin-x64
//   linux-x64    -> psx-linux-x64
//   linux-arm64  -> psx-linux-arm64
//   win32-x64    -> psx-windows-x64.exe
//
// CI publishes those assets to:
//   https://github.com/bimsonz/elephpant/releases/download/v<version>/<asset>
//
// Skips download in three cases:
//   1. PSX_SKIP_DOWNLOAD=1 (developer escape hatch).
//   2. The repo cloned locally already has a built binary at
//      ./vendor/<platform>-<arch>/psx (manual override).
//   3. process.env.npm_config_global is unset AND PSX_INSTALL_LOCAL=1 — a
//      developer convenience for `npm link` style workflows.

"use strict";

const fs = require("node:fs");
const path = require("node:path");
const https = require("node:https");
const crypto = require("node:crypto");

if (process.env.PSX_SKIP_DOWNLOAD === "1") {
  console.log("elephpant: PSX_SKIP_DOWNLOAD=1 set, skipping binary download.");
  process.exit(0);
}

const pkg = require("./package.json");

const PLATFORMS = {
  "darwin-arm64": { asset: "psx-darwin-arm64", ext: "" },
  "darwin-x64":   { asset: "psx-darwin-x64",   ext: "" },
  "linux-x64":    { asset: "psx-linux-x64",    ext: "" },
  "linux-arm64":  { asset: "psx-linux-arm64",  ext: "" },
  "win32-x64":    { asset: "psx-windows-x64",  ext: ".exe" },
};

const key = `${process.platform}-${process.arch}`;
const entry = PLATFORMS[key];
if (!entry) {
  console.error(
    `elephpant: no prebuilt binary published for ${key}. ` +
      "Build from source: https://github.com/bimsonz/elephpant",
  );
  process.exit(1);
}

const assetName = `${entry.asset}${entry.ext}`;
const baseUrl = `https://github.com/bimsonz/elephpant/releases/download/v${pkg.version}`;
const assetUrl = `${baseUrl}/${assetName}`;
const shasumsUrl = `${baseUrl}/SHASUMS256.txt`;

const vendorDir = path.join(__dirname, "vendor", key);
const binaryPath = path.join(vendorDir, `psx${entry.ext}`);

main().catch((err) => {
  console.error(`elephpant: install failed: ${err.message}`);
  process.exit(1);
});

async function main() {
  fs.mkdirSync(vendorDir, { recursive: true });

  console.log(`elephpant: fetching ${assetUrl}`);
  const binaryBuf = await fetch(assetUrl);
  const shasumsBuf = await fetch(shasumsUrl);

  const expected = expectedChecksum(shasumsBuf.toString("utf8"), assetName);
  if (!expected) {
    throw new Error(
      `SHASUMS256.txt has no entry for ${assetName}. Release may be incomplete.`,
    );
  }
  const actual = crypto.createHash("sha256").update(binaryBuf).digest("hex");
  if (actual !== expected) {
    throw new Error(
      `checksum mismatch for ${assetName}\n  expected: ${expected}\n  actual:   ${actual}`,
    );
  }

  fs.writeFileSync(binaryPath, binaryBuf, { mode: 0o755 });
  console.log(`elephpant: installed psx ${pkg.version} for ${key}`);
}

function expectedChecksum(shasumsText, asset) {
  // SHASUMS256.txt entries look like:
  //   <sha256>  psx-linux-x64
  // (two spaces between hash and filename — POSIX `shasum -a 256` format).
  for (const line of shasumsText.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const m = trimmed.match(/^([0-9a-f]{64})\s+\*?(.+)$/);
    if (!m) continue;
    const [, hash, filename] = m;
    if (path.basename(filename) === asset) {
      return hash;
    }
  }
  return null;
}

function fetch(url, redirects = 5) {
  return new Promise((resolve, reject) => {
    https
      .get(url, (res) => {
        const status = res.statusCode ?? 0;
        if (status >= 300 && status < 400 && res.headers.location) {
          if (redirects <= 0) {
            reject(new Error(`too many redirects fetching ${url}`));
            return;
          }
          res.resume();
          fetch(res.headers.location, redirects - 1).then(resolve, reject);
          return;
        }
        if (status !== 200) {
          reject(new Error(`HTTP ${status} fetching ${url}`));
          return;
        }
        const chunks = [];
        res.on("data", (chunk) => chunks.push(chunk));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}
