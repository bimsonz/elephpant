# Changelog

Notable changes between releases. Hand-maintained.

## Unreleased

- CI workflow (`cargo fmt --check`, `cargo test` matrix across ubuntu / macos / windows, end-to-end smoke test that builds `examples/multi-file` + `examples/traits-demo`, type-checks with `tsc --strict`, and runs the emitted JS in node).
- Release workflow triggered by `v*` tags: multi-platform binary build (darwin arm64 + x64, linux x64 + arm64, windows x64) with SHA256 checksums, GitHub Release upload, `cargo publish` for every workspace crate in dependency order, `npm publish elephpant`.
- `npm/psx/install.js` rewritten as a real postinstall — downloads the prebuilt binary for the running platform from the GitHub Release for the package's version, verifies its SHA256 against `SHASUMS256.txt`, and places it at `vendor/<platform>-<arch>/psx`.
- Per-crate `README.md` + `readme` field in each `Cargo.toml` so crates.io renders proper documentation pages.
- VS Code extension (`editors/vscode/`) — TextMate grammar with full keyword/operator/type/variable/interpolation coverage. `vsce publish` step added to the release workflow, gated on the `VSCE_PAT` secret so the job skips cleanly when the secret isn't set.
- Source maps: every emitted `.ts` now ships with a paired `.ts.map` and a trailing `sourceMappingURL` comment. New `psx-sourcemap` crate provides `LineMap`, base-64 VLQ encoding, and a `SourceMapBuilder` that serialises to source-map-v3 JSON. Spans threaded through every `Stmt` variant; the emitter records a `(ts_line, ts_col) -> (psx_line, psx_col)` mapping at the start of every statement in every flat scope. `psx build --no-source-maps` opts out. Stack traces in `node --enable-source-maps` resolve through `.js -> .ts -> .psx`.
- LSP Phase 1: new `psx-lsp` crate on `tower-lsp` exposes a stdio language server. Capabilities: text-document sync (open/change/save/close), parse-error diagnostics published on every reparse, hover with statement-level descriptions, goto-definition for cross-file `use App\Foo;` directives (resolves PSR-4 target file via `psx-resolver`), document symbols (classes/interfaces/enums/traits/functions with method children), and workspace symbols populated by walking every `.psx` under the workspace root at initialise time. Companion VS Code extension client lives at `editors/vscode/src/extension.ts` and spawns the `psx-lsp` binary. Release workflow builds both `psx` and `psx-lsp` binaries for every platform.
- LSP Phase 2 & 3: completions (keywords + workspace symbols + class members after `$this->`), signature help (triggered on `(` and `,` — returns the called function's params with type info), rename (file-local variable rename via AST walk + `WorkspaceEdit`), code actions (one quick fix: `array(...)` → `[...]`), document formatting (via the new `psx-printer` crate), inlay hints (`$x = <literal>` shows inferred primitive type).
- New `psx-printer` crate: AST → `.psx` pretty-printer. MVP coverage of common statements + expressions + class members. Used by the LSP formatting handler; unsupported AST patterns round-trip as best-effort placeholders so editors don't lose unfamiliar code.
- Sub-statement source maps: `Expr::Call` now carries a `Span`. The emitter records a mapping at every call-site boundary in addition to the per-statement mapping, giving stack traces column-level resolution within chained `a(b(c(...)))` lines.
- Trait conflict resolution: `use A, B { A::m insteadof B; B::m as legacyM; }` is supported. `insteadof` drops the loser trait's same-named member before expansion; `as` appends a renamed copy with an optional visibility override.
- Transitive trait expansion: a trait can `use` another trait; the flattener follows recursively. Cycle detection emits a marker comment-member rather than looping.

## v0.0.1 (unreleased)

Initial published state. Phases 0–5 complete:

- Lexer + parser + emitter for PHPScript's full PHP 8.5 surface.
- Multi-file projects via `psx build` and `psx.json` (PSR-4 resolution, npm escape hatch).
- OOP: classes, interfaces, abstracts, enums, inheritance, constructor property promotion, PHP 8.4 property hooks, asymmetric visibility, first-class callable.
- Control flow: `match`, ternary, elvis, `<=>` spaceship, `??` null-coalesce, `try`/`catch`/`finally`.
- Async/await with auto-`Promise<…>` return-type wrapping.
- Traits via compile-time inline expansion. Multi-file via a two-pass project compile.
- String interpolation lowers to TS template literals.

No prebuilt binaries published yet. Build from source.
