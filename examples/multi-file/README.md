# `multi-file` example

A real multi-file project with `psx.json` driving PSR-4 resolution. `src/Main.psx` imports types from `src/Models/User.psx` and `src/Util/shout.psx`; the resolver computes the relative import paths and the emitter writes valid TS module imports.

Run:

```bash
../../target/release/psx build
# src/**/*.psx -> dist/**/*.ts
node run-demo.mjs    # after tsc has produced dist-js/
```
