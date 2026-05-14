# `elephpant` — npm wrapper for the PHPScript compiler

Thin npm wrapper that exposes the `psx` binary on `node_modules/.bin`. Use it when PHPScript is one piece of a JS toolchain (build scripts, monorepo tasks, bundler integrations).

## Install

```bash
npm install --save-dev elephpant
```

`postinstall` downloads the prebuilt binary for your platform from the GitHub Release for the installed version and verifies its SHA256 against `SHASUMS256.txt`. Supported platforms:

| platform | arch  | asset |
|----------|-------|-------|
| darwin   | arm64 | `psx-darwin-arm64` |
| darwin   | x64   | `psx-darwin-x64`   |
| linux    | x64   | `psx-linux-x64`    |
| linux    | arm64 | `psx-linux-arm64`  |
| win32    | x64   | `psx-windows-x64.exe` |

If you're on an unsupported platform or want to bring your own binary, set `PSX_SKIP_DOWNLOAD=1` and place a binary at `node_modules/elephpant/vendor/<platform>-<arch>/psx`.

## Use

```bash
npx psx build src/                   # compile a directory
npx psx build src/Main.psx           # compile one file
npx psx build                        # walk up to the nearest psx.json
```

`bin/psx.js` is a thin Node shim that exec's the vendored binary and forwards stdio.

## Build from source

If you'd rather skip the prebuilt download:

```bash
git clone https://github.com/bimsonz/elephpant
cd elephpant
cargo build --release
./target/release/psx --help
```

## License

MIT OR Apache-2.0 (matches the parent project).
