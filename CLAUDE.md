# PHPScript (`.psx`) — Working Rules

Project-scoped instructions. These take precedence over default tool behavior.

## Attribution

**Never** include AI attribution, co-author lines, or "Generated with Claude" footers in any project artifact. This applies to:

- Commit messages — omit the `Co-Authored-By: Claude...` trailer entirely.
- Pull requests — no "🤖 Generated with Claude Code" footer.
- Release notes, changelogs, GitHub releases.
- Code comments, docs, generated outputs.

If the harness default would add such a line, override it.

## Modern PHP only

Reference floor is **PHP 8.5**. PHP 9 RFCs (notably the experimental generics RFC) are tracked actively. Reject legacy/deprecated constructs:

- `var` keyword — out (use `public`/`private`/`protected`).
- `array()` long-form, `each()`, `create_function`, alternative `if/endif` syntax — out.
- Constructor property promotion is the default constructor style, not optional.
- Enums use the 8.1 backed-enum form.
- `readonly`, asymmetric visibility (8.4), property hooks (8.4), `match` over `switch`, named arguments, first-class callable syntax — all in scope.

When in doubt, prefer the form a PHP 9 codebase would write.

## Test-driven development

Every feature lands **test-first**.

- New lexer/parser/emitter behavior: failing test before production code, every time.
- `insta` snapshot tests for emitter/E2E output. `cargo test` unit tests elsewhere.
- "MVP" scopes *features*, never *quality*. Don't accept "we'll add tests later".
- Snapshot fixtures double as language documentation — keep them clean and meaningful.
- Follow the `superpowers:test-driven-development` skill rigorously.

## Architecture (one-paragraph reminder)

Rust workspace. `psx-lexer` → `psx-parser` → `psx-ast` → `psx-emitter` → TypeScript source. CLI in `psx-cli`. Source maps via `psx-sourcemap`. Language server via `psx-lsp`. `.psx` pretty-printer via `psx-printer`. The emitted TypeScript is consumed by `tsc` / `esbuild` / any JS toolchain. Full design lives at [`docs/design.md`](./docs/design.md).
