#!/usr/bin/env node
// Thin shim around the platform-specific psx binary. Once the release
// pipeline ships prebuilt binaries, this script will exec the binary in
// ../vendor/<platform>-<arch>/psx and forward stdio.
//
// For now it points users at the source build.

const { execFileSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

const candidate = path.resolve(__dirname, "..", "vendor", `${process.platform}-${process.arch}`, process.platform === "win32" ? "psx.exe" : "psx");

if (!fs.existsSync(candidate)) {
  console.error(
    "elephpant: no prebuilt binary found for " +
      `${process.platform}-${process.arch}. ` +
      "Build from source — see https://github.com/bimsonz/elephpant.",
  );
  process.exit(1);
}

try {
  execFileSync(candidate, process.argv.slice(2), { stdio: "inherit" });
} catch (err) {
  process.exit(err.status ?? 1);
}
