# PHPScript — Language + Compiler Design

## Context

PHPScript is a modern PHP-inspired language for the JavaScript ecosystem. Files use the `.psx` extension; the **elephpant** compiler (this repo) lowers them to clean, type-checked TypeScript that runs anywhere JS does. No PHP runtime — every construct compiles to native JS/TS at build time.

**Framing.** PHPScript is its own language. It borrows syntax from modern PHP — `$` sigils, `->`, `::`, `use`, `foreach`, namespaces, PHP-style type annotations — but defines its own semantics where they need to differ to play well with the JS runtime. Arrays are JS arrays. `==` is rejected; only `===` is accepted. `async`/`await` is first-class (PHP has fibers; we don't).

**Why compile to TypeScript instead of forking a JS compiler.** Forking `microsoft/typescript-go` was the original idea and got rejected — its AST is hardcoded JS-shaped throughout a 1.4MB `checker.go` with no plugin seam, and Microsoft is folding it back into `microsoft/TypeScript` long-term. Instead, elephpant follows Civet's playbook: compile `.psx` → TypeScript source, hand off to `tsc` / `esbuild` / the wider TS ecosystem for type checking, source maps, IDE integration, npm interop. We inherit the entire toolchain instead of rebuilding it.

**Language version reference (no legacy baggage).** PHP **8.5 is the syntactic reference**; PHP 9 features (notably the experimental generics RFC) are tracked actively. PHPScript does **not** support deprecated PHP constructs — `var` keyword, `array()` long-form, `each()`, `create_function`, dynamic class properties without declarations, alternative `if/endif` syntax. Style is forward-only: constructor property promotion is the default, `readonly` is supported, enums use the 8.1 backed-enum form, `match` is preferred over `switch`, first-class callable syntax (`strlen(...)`), named arguments, `never`/`true`/`false` types, asymmetric visibility (8.4), property hooks (8.4) — all in scope. When PHP 9 ships generics, we'll align the surface syntax to it; until then, generic types on classes and functions are deferred.

## Architecture

```
.psx source files
       │
       ▼
┌────────────────────┐
│  elephpant (Rust)  │   single binary; also a workspace of crates
│  ────────────────  │
│  Lexer             │   psx-lexer
│  Parser (RD)       │   psx-parser     → psx-ast
│  PSR-4 resolver    │   psx-resolver
│  TS emitter        │   psx-emitter    → .ts + .ts.map
└────────────────────┘
       │
       ▼
.ts files (with type annotations + chained source map)
       │
       ▼
tsc / esbuild  ──►  .js / .js.map  (debuggers chain back to .psx)
```

- **Implementation language:** Rust. Single binary + per-component library crates.
- **Parser:** hand-rolled recursive descent (swc/oxc-style). Pratt-style operator precedence for binary expressions.
- **Distribution:** prebuilt binaries via GitHub Releases; Rust crates via crates.io; thin npm wrapper (`elephpant`) that exec's the binary for Node-tooling integration.
- **No runtime library.** Every PHPScript construct lowers to native JS/TS at compile time. No `PHPArray` wrapper, no coercion shim. Compiled output is the same size as hand-written TypeScript.

## Language Spec (MVP target)

### File extension: `.psx`
No `<?php` opener — extension implies it.

### Variables & types
```
$name: string = "World";
$count: int = 0;
$items: array<int> = [1, 2, 3];
$user: ?User = null;
```
Maps to TS: `let name: string = "World";` etc. The `$` is dropped at emit.

**Type mappings:**
| PHPScript | TypeScript |
|---|---|
| `int`, `float` | `number` |
| `string` | `string` |
| `bool` | `boolean` |
| `void`, `never`, `null`, `mixed` | identical |
| `array<T>` | `T[]` |
| `array<K, V>` | `Record<K, V>` |
| `Foo\|Bar` | `Foo \| Bar` |
| `?Foo` | `Foo \| null` |
| Class names | identical |
| `Promise<T>` | `Promise<T>` |

### Arrays
- `[1, 2, 3]` → JS array.
- `["k" => "v"]` → JS object literal `{ k: "v" }`.
- Mixed/dynamic keys → object with numeric-string keys (documented gotcha).
- We deliberately do **not** ship PHP's ordered-hashmap semantics. Trade-off: full JS interop > PHP authenticity.

### Functions
```
function add(int $a, int $b): int {
    return $a + $b;
}

$double = fn(int $x): int => $x * 2;
```

### Classes, interfaces, abstracts, traits-as-mixins

Modern PHP only — constructor property promotion is the default, `readonly` is supported, enums use the 8.1 backed-enum form, asymmetric visibility (8.4) and property hooks (8.4) are first-class.

```
enum Role: string {
    case Admin = "admin";
    case Member = "member";
}
```

```
namespace App\Models;

use App\Contracts\Auditable;

interface Auditable {
    public function audit(): void;
}

abstract class Entity {
    public function __construct(public string $id) {}
}

class User extends Entity implements Auditable {
    public function __construct(
        string $id,
        public string $email,
        private ?string $passwordHash = null,
    ) {
        parent::__construct($id);
    }

    public function audit(): void {
        console::log("audit {$this->id}");
    }
}
```

### Generics
PHP 9's experimental generics RFC is the **canonical source** once stable. Until then, MVP uses a TS-influenced syntax that should map cleanly to whatever PHP 9 finalizes:
```
function map<T, U>(array<T> $xs, fn(T): U $f): array<U> { /* ... */ }
class Box<T> { public function __construct(public readonly T $value) {} }
```
Action item during Phase 5: re-read the active PHP 9 generics RFC and align surface syntax exactly. If PHP 9 ships generics differently (e.g., `@template` doc-style or sigil-based), we adopt that and rev the parser.

### Async/await
Extension of PHP syntax (PHP has fibers, not async/await — we just adopt JS keywords):
```
async function fetchUser(string $id): Promise<User> {
    $res = await fetch("/api/users/{$id}");
    return await $res->json();
}
```

### Modules & namespaces (PSR-4)
- Project root has `psx.json` declaring base namespace + src directory.
  ```json
  { "namespace": "App", "src": "src" }
  ```
- `namespace App\Foo;` at top of `src/Foo/Bar.psx` declares `App\Foo\Bar`.
- `use App\Foo\Bar;` → resolved to `import { Bar } from "../Foo/Bar";` at emit time (relative path computed by resolver).
- `use App\Foo\Bar as Baz;` → `import { Bar as Baz } from ...`.
- **npm escape hatch** (pragmatic addition — strict PSR-4 doesn't address external packages): `use \Npm\React\useState;` resolves to `import { useState } from "react";` via a configurable namespace prefix mapping in `psx.json`. Without this, calling npm packages is impossible.

### Control flow
`if/elseif/else`, `foreach ($xs as $x)`, `foreach ($xs as $k => $v)`, `for`, `while`, `do/while`, `match` (lowered to TS `switch` or chained ternaries depending on shape), `break`, `continue`.

### Operators
`+`, `-`, `*`, `/`, `%`, `**`, `.` (string concat → template literal or `+`), `??`, `<=>`, `?:`, `===`/`!==`, `&&`/`||`, bitwise. **`==` is NOT polyfilled** — emits to JS `===` always (documented; PHP's loose equality is too weird to inherit).

### String interpolation
`"hello $name"` and `"hello {$user->email}"` → TS template literals.

### Errors
```
try {
    throw new InvalidArgumentException("nope");
} catch (InvalidArgumentException $e) {
    console::log($e->getMessage());
} finally { /* ... */ }
```
`Exception` and friends map to a tiny set of error classes that extend JS `Error`.

### Builtins / stdlib
Rather than reimplement PHP's stdlib, we expose the **JS API directly under PHP-styled aliases**:
- `console::log(...)`, `console::error(...)` (statics on a virtual `console` namespace)
- `JSON::stringify`, `JSON::parse`
- `fetch(...)`, `Date`, `Math::*`, etc.

This is the "map to JavaScript API" intent the user described — no PHP `array_map`, just `array<T>` with chainable methods or use JS `Array.prototype`-style.

## Components / Crates

| Crate | Responsibility |
|---|---|
| `psx-lexer` | Tokenize `.psx` source. |
| `psx-ast` | AST node definitions (Rust enums + structs). |
| `psx-parser` | Recursive-descent parser; produces `psx-ast`. |
| `psx-resolver` | PSR-4 namespace → file path resolution; reads `psx.json`. |
| `psx-emitter` | Walks AST, emits TS source as `String`. |
| `psx-cli` | `psx build`, `psx check`, `psx fmt` commands; uses clap. |
| `psx-wasm` (later) | WASM build for in-browser playground. |
| `npm/psx` | Thin npm wrapper that downloads & invokes the prebuilt binary; exposes a programmatic Node API for build-tool integration. |

Each crate stays focused; the AST is the seam between parser and emitter.

## Phased Implementation

| Phase | Scope | Status |
|---|---|---|
| **0** | Cargo workspace, CLI skeleton, `.psx` fixture files, snapshot-test harness (`insta`). | ✅ shipped. |
| **1** | Lexer + parser for: variables, primitive types, arithmetic/string ops, `if/elseif/else`, `foreach`, function declarations, arrow fns, arrays. | ✅ shipped. |
| **2** | TS emitter for Phase 1 features. | ✅ shipped — `examples/hello` round-trips through `tsc --strict` + `node`. |
| **3** | Classes, interfaces, abstract, inheritance, `$this`, visibility modifiers, constructor property promotion, enums, `readonly`, class constants. | ✅ shipped — `examples/oop`. |
| **4** | `psx.json` config, namespaces, `use` statements, PSR-4 resolver, npm escape hatch. | ✅ shipped — `examples/multi-file`. |
| **5** | Type unions, nullable types, async/await, try/catch/throw, `match`, string interpolation, `??`/`<=>`/`?:`, PHP 8.4 property hooks, asymmetric visibility, first-class callable, traits (incl. `insteadof`/`as` + transitive `use`). | ✅ shipped — `examples/async-demo`, `examples/error-handling`, `examples/hooks-demo`, `examples/traits-demo`. Generics tracked upstream (PHP 9 RFC). |
| **6** | Source maps: `.psx → .ts → .js` chain via `psx-sourcemap` crate. Spans on every `Stmt` and `Expr::Call`. CLI writes `.ts.map` alongside `.ts`. | ✅ shipped. |
| **7** | Editor tooling — VS Code TextMate grammar; `psx-lsp` Language Server with diagnostics, hover, goto, document/workspace symbols, completions, signature help, rename, code actions, formatting, inlay hints. `psx-printer` AST → `.psx` pretty-printer. | ✅ shipped. |
| **8** | CI + release pipeline — GitHub Actions matrix build, multi-platform binary release, crates.io + npm + VS Code Marketplace publish on `v*` tags. | ✅ wired, awaits first tag. |

Each phase ends with: snapshot tests pass, example file compiles and runs end-to-end, README updated.

### Shipped emit decisions

Choices made during implementation that aren't obvious from the surface syntax:

- **Spaceship `<=>`**: emitted as a single-eval IIFE — `((__l, __r) => __l < __r ? -1 : __l > __r ? 1 : 0)(a, b)` — so it never re-evaluates side-effectful operands. No runtime helper.
- **`match` expressions**: comma-separated arms lower to OR chains; `match (true) { …conds }` lowers to a nested ternary; the general case lowers to an IIFE-wrapped `switch`. Pick the simplest shape that captures the semantics.
- **String interpolation**: `"hello $name"` and `"hello {$expr}"` → TS template literals. Concatenation chains stay as `+` unless they include an interpolated string.
- **First-class callable `(...)`**: when the target is a pure expression (`Var`, `Ident`, `Foo::bar(...)`) emit a bare `.bind(obj)` or unbound reference; when the target is a complex expression like `(new Foo())->m(...)`, wrap in a single-eval IIFE so the receiver is only constructed once.
- **Property hooks (PHP 8.4)**: `get =>` and `set(…) =>` lower to TS getters/setters over a private backing field (`_<prop>!`). The `$this->prop` reference inside the hook body rewrites to `this._<prop>` at emit time, no AST pass needed.
- **Asymmetric visibility (PHP 8.4)**: `public private(set)` lowers to `public readonly` when the get-side is public and the set-side is private (the common case); other combinations fall through to a getter/setter pair.
- **Traits**: compile-time inline expansion. `Stmt::Trait` emits nothing; trait members are spliced into using classes ahead of the class's own members. Class-defined members win silently on conflict (PHP semantics); trait-vs-trait conflict on the same name emits a marker constant so `tsc` fails loudly. A namespace `use App\Traits\X;` directive pointing at a trait is dropped from the import header (the trait file emits nothing, so the import would dangle).
- **Trait `insteadof` / `as`**: `use A, B { A::m insteadof B; B::m as legacyM; }` is supported. `insteadof` drops the loser trait's same-named member before expansion (so no conflict marker is emitted); `as` appends a renamed copy of the source method with an optional visibility override. Adaptations apply to methods, properties, and class constants alike — the loser-name set is keyed by member name, not method-specifically.
- **Transitive trait expansion**: a trait can `use OtherTrait;` in its body. `flatten_trait` recursively follows nested `UseTrait` directives. Cycles (A uses B uses A) emit a marker constant rather than looping forever — the `visited` set is per top-level expansion chain.
- **Auto-export**: top-level declarations in a file with a `namespace` declaration get `export`. Files without a namespace stay script-shaped.
- **Hoisting**: locals assigned in nested blocks but read at top-level get a leading `let x, y;` hoist line so the assignment can stay `=` rather than become `let x =`. Function/class/interface/enum/trait declarations don't contribute hoist names.

- **Source maps**: `Stmt` variants carry a `Span` (byte range into the source) populated by the parser. The emitter wraps its output in a `SourceMapBuilder` (from the new `psx-sourcemap` crate) and records a `(generated_line, generated_col) -> (source_line, source_col)` mapping at the start of every statement in every flat scope (module body, function body, method body, hook body). CLI writes `<name>.ts.map` alongside `<name>.ts` and appends `//# sourceMappingURL=<name>.ts.map` to the TS output. `psx build --no-source-maps` opts out. Granularity is statement-level (not token-level) — enough to give `node --enable-source-maps` a useful chained stack trace through `.js -> .ts -> .psx`.
- **LSP**: `crates/psx-lsp/` ships a `tower-lsp`-based language server. Capabilities: full text-document sync, parse-error diagnostics on every reparse, hover with statement-level descriptions, goto-definition for cross-file `use` directives (PSR-4 via `psx-resolver`), document symbols (classes/interfaces/enums/traits/functions with method children), workspace symbols, **completions** (keyword + workspace-symbol + `$this->` member-after-arrow), **signature help** triggered on `(` / `,` (returns the called function's params), **rename** (file-local variables), **code actions** (one quick fix: `array(...)` → `[...]`), **formatting** via the new `psx-printer` crate, and **inlay hints** for `let $name = <literal>` where the type is inferable. VS Code client at `editors/vscode/src/extension.ts` spawns the `psx-lsp` binary over stdio.
- **`psx-printer`**: a small AST → `.psx` pretty-printer used by the LSP formatting handler. MVP coverage — common statements + expressions + class members. Patterns the printer doesn't cover fall back to best-effort placeholders; the LSP treats unchanged output as a no-op so unsupported files stay editable.
- **Sub-statement source maps**: `Expr::Call` carries a `Span`; the emitter records a mapping at each call-site boundary on top of the per-statement mapping. Chained `a(b(c()))` lines produce 3 + 1 = 4 segments, giving debuggers column-level resolution within multi-call lines.

### Deferred (tracked, not blocking)

- **Generics** — waiting on the PHP 9 RFC.
- **Token-level source maps** — current sub-statement granularity covers call sites; expression-level (binary operators, member access) would be next if debugging UX feels coarse.
- **LSP cross-file analysis** — diagnostics that need to know about every Class in the workspace (e.g. unused-import warnings) are deferred until the index gains a semantic-model layer.
- **LSP type inference** — inlay hints currently fire only on `$x = <literal>`. Real type inference (across assignments + function signatures) is its own multi-week effort.
- **`.psx` formatter completeness** — the printer covers the common AST surface; patterns it doesn't know about (property hooks, try/catch detail, complex match) round-trip as placeholders. Filling these in is incremental work as each shape arrives in real code.

## Critical Files / Locations

- `Cargo.toml` — workspace root.
- `crates/psx-lexer/src/lib.rs` — token types + lexer.
- `crates/psx-ast/src/lib.rs` — AST definitions.
- `crates/psx-parser/src/lib.rs` — parser entry; modules per construct (`expr.rs`, `stmt.rs`, `class.rs`, `module.rs`).
- `crates/psx-resolver/src/lib.rs` — PSR-4 logic; `psx.json` schema.
- `crates/psx-emitter/src/lib.rs` — emitter entry; modules per construct.
- `crates/psx-cli/src/main.rs` — clap CLI.
- `crates/psx-sourcemap/src/lib.rs` — `LineMap`, base-64 VLQ, source-map-v3 builder.
- `crates/psx-printer/src/lib.rs` — AST → `.psx` pretty-printer.
- `crates/psx-lsp/src/server.rs` — `tower-lsp` backend.
- `npm/psx/` — npm wrapper (`package.json`, postinstall download script).
- `editors/vscode/` — VS Code extension (TextMate grammar + LSP client).
- `examples/<name>/main.psx` or `src/` — working `.psx` demos.
- `tests/snapshots/` — `insta` snapshot fixtures (`.psx` → `.ts`).

## Reusable / External Pieces

- **`insta`** — Rust snapshot-testing crate; standard for compiler test suites.
- **`clap`** — Rust CLI argparser.
- **`tower-lsp`** — Rust LSP framework used by `psx-lsp`.
- **`oxc` / `swc` source** — read for parser-architecture inspiration; we don't depend on them (different target language).

## Verification

Each phase has a corresponding test set. The project is "working" when all of these pass:

1. **Unit tests** (`cargo test --workspace` — currently 412 across 8 crates).
2. **Snapshot tests** — every `.psx` fixture in `crates/psx-cli/tests/snapshots.rs` produces an exact `.ts` snapshot via `insta`.
3. **End-to-end compile-and-run** — `psx build examples/hello/main.psx && node …` produces the expected output.
4. **TypeScript validation** — emitted `.ts` passes `tsc --strict` for every example.
5. **Source map chain** — `node --enable-source-maps` resolves stack traces from `.js` through `.ts` back to the originating `.psx` line.
6. **CI matrix** — GitHub Actions runs `cargo fmt --check`, `cargo test --workspace`, and the end-to-end smoke test on ubuntu / macos / windows.
- **PHP 8.x attributes (`#[Attribute]`)** — not in MVP; could map to TypeScript decorators later.
- **PHP 9 generics syntax** — track the RFC; re-align Phase 5 syntax once stable. If PHP 9 generics ship before MVP completes, adopt that syntax directly and skip the TS-style interim form.
- **Property hooks / asymmetric visibility (PHP 8.4)** — in scope as language features; emit-side mapping (do hooks become TS getters/setters? does asymmetric visibility map to `readonly` + private setter?) decided during Phase 3.
- **`==` semantics** — explicit decision: emit as `===`. Documented; revisit only if user demand surfaces.
- **`echo`** — alias to `console.log`? Or omit entirely in favor of `console::log`? Lean toward omit (consistency with "map to JS API").
