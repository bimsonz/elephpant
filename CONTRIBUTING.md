# Contributing to PHPScript

PHPScript is in active development. Contributions of all sizes are welcome — language features, bug fixes, examples, documentation, tooling.

## Ground rules

**Test-driven development.** Every behavior change lands with a failing test first, then the production code that makes it pass. The point isn't ritual — it's proof that the test actually catches what you intended.

- New lexer/parser/emitter behavior: write the failing test before the production code.
- `insta` snapshot tests for emitter and end-to-end output. `cargo test` unit tests elsewhere.
- "MVP" scopes features, never quality. No "we'll add tests later".
- Snapshot fixtures double as language documentation. Keep them clean and meaningful.

**Modern PHP only.** Reference floor is PHP 8.5; PHP 9 RFCs (especially generics) are tracked actively. Reject legacy/deprecated constructs:

- No `var` keyword (use `public`/`private`/`protected`).
- No `array()` long-form, `each()`, `create_function`, alternative `if/endif` syntax.
- Constructor property promotion is the default constructor style.
- Enums use the 8.1 backed-enum form.
- `readonly`, asymmetric visibility (8.4), property hooks (8.4), `match`, named arguments, first-class callable syntax — all in scope.

When in doubt, prefer the form a fresh PHP 9 codebase would write.

**PHP fidelity is enforced via [`docs/php-fidelity.md`](./docs/php-fidelity.md).** Every PHPScript construct maps to a specific PHP RFC, manual page, or explicit "deviation" entry. Before adding or modifying syntax:

- Confirm the real-PHP form by reading the linked RFC.
- Add a row to the fidelity table (or update an existing one).
- Add a parser test using a copy-pasted snippet from the RFC's examples.
- If PHP rejects a form, our parser must reject it too — add the negative test as well.

Inventing syntax PHP doesn't actually have — or quietly accepting deprecated forms — is the failure mode this discipline prevents. When in doubt: copy from the PHP manual or RFC, not from memory.

**No attribution noise.** No "Co-Authored-By", no "Generated with X" footers, no AI-tool watermarks in commits, PRs, release notes, comments, or docs. Commit messages should describe what changed and why, in your own voice.

## Dev loop

```bash
cargo test                  # unit + snapshot tests for every crate
cargo build --release       # binary at ./target/release/psx
```

When you change emitter output and the existing snapshots need to update:

```bash
cargo insta pending-snapshots    # see what changed
cargo insta accept               # accept all (after review)
```

End-to-end validation for a feature usually means:

1. Snapshot test in `crates/psx-cli/tests/snapshots.rs` for the new emit shape.
2. Working example under `examples/<name>/`.
3. `psx build` + `npx tsc --strict` + `node` on the emitted output prints what you expect.

## Adding a new language feature

The path is usually:

1. **AST.** Add the new variant/struct in `crates/psx-ast/src/lib.rs`. Existing variants that gain fields go via Python-style sed batches if there are many literal-construction sites (see commit history for prior patterns).
2. **Lexer.** Almost never needs changes — most PHP keywords are already tokenized.
3. **Parser.** New parser function in `crates/psx-parser/src/lib.rs`; wire it into the dispatch site (`parse_stmt`, `parse_class_member`, `parse_primary`, etc.).
4. **Emitter.** New arm in the relevant `emit_*` function in `crates/psx-emitter/src/lib.rs`. Prefer compile-time lowering over a runtime helper — every PHPScript construct should emit native JS/TS.
5. **Tests.**
   - Parser unit tests in `crates/psx-parser/src/lib.rs` (`#[cfg(test)]`).
   - Emitter snapshots in `crates/psx-cli/tests/snapshots.rs`.
   - If multi-file behavior matters, add a `compile_project` E2E test (see `multi_file_trait_use_does_not_emit_import_and_inlines_members` for a template).
6. **Example.** Add or extend an example under `examples/` so the README table can point at a working demo.
7. **Docs.** Update `docs/design.md` if the feature affects the published spec.

## Project structure

```
crates/
  psx-lexer/      Tokenizer
  psx-ast/        AST node definitions
  psx-parser/     Hand-rolled recursive-descent parser (Pratt-style binary ops)
  psx-resolver/   psx.json + PSR-4 namespace -> file path
  psx-emitter/    AST -> TypeScript source
  psx-cli/        psx binary
examples/         Working .psx projects, one per feature area
docs/             Language + architecture spec
tests/            Cross-crate fixtures
npm/psx/          npm wrapper scaffold
```

## Filing issues

If you hit a bug, a minimal reproducing `.psx` snippet plus the expected vs. actual TS output is the most useful thing you can include. The emitter is deterministic — a small input is almost always enough.

## License

Contributions are dual-licensed under MIT and Apache 2.0, matching the project. By submitting a contribution you agree to license it under both.
