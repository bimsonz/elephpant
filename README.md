# elephpant

**A modern PHP-inspired language for the JavaScript ecosystem.** Write `.psx` files that feel like PHP 8.5+; elephpant compiles them to clean, type-checked TypeScript that runs anywhere JS does. No PHP runtime — every construct lowers to native JS/TS at compile time.

The name is a tip of the hat to the elePHPant — every PHP dev knows it. The toolchain is built in Rust and ships as a single binary.

```php
namespace App\Models;

class User {
    public private(set) string $id;

    public function __construct(
        string $id,
        public string $email,
    ) {
        $this->id = $id;
    }

    public function describe(): string {
        return "{$this->email} (id={$this->id})";
    }
}
```

…compiles to:

```ts
export class User {
  public readonly id: string;
  public constructor(id: string, public email: string) {
    this.id = id;
  }
  public describe(): string {
    return `${this.email} (id=${this.id})`;
  }
}
```

(`public private(set)` lowers to `public readonly` — the simplest TS shape that captures the same external contract. Files declare an explicit `namespace` and get an `export` automatically; the readonly assignment in the constructor is rewritten in-place.)

## What's done

PHPScript is a real language with a working compiler. The full PHP 8.5 surface area is implemented; the toolchain ships diagnostics, source maps, and a language server.

### Language

| Feature | Status |
|---|---|
| Type-annotated locals, params, return types | ✅ |
| `int` / `float` / `string` / `bool` / `void` / `never` / `mixed` / `null` / `true` / `false` types | ✅ |
| Union types (`A \| B`) and nullables (`?T`) | ✅ |
| Generic type annotations (`array<T>`, `Promise<T>`, …) | ✅ |
| Control flow: `if`/`elseif`/`else`, `for`, `foreach`, `while`, `do`/`while`, `break`, `continue`, `return` | ✅ |
| `match` expressions | ✅ |
| `try`/`catch`/`finally`, `throw`, typed catches | ✅ |
| Classes, abstract classes, `final`, `readonly class` | ✅ |
| Interfaces with constants and method signatures | ✅ |
| Enums (pure + 8.1 backed enums) | ✅ |
| Traits (with multi-file expansion, `insteadof`/`as`, transitive `use`) | ✅ |
| Constructor property promotion | ✅ |
| Property hooks (PHP 8.4 `get =>` / `set(…) =>`) | ✅ |
| Asymmetric visibility (PHP 8.4 `public private(set)`) | ✅ |
| First-class callable syntax (PHP 8.1 `obj->method(...)`) | ✅ |
| `<=>` spaceship, `??` null-coalesce, `?:` elvis | ✅ |
| String interpolation (`"$var"`, `"{$expr}"`) | ✅ |
| `async` / `await` (PHPScript addition — auto-wraps return as `Promise<…>`) | ✅ |
| Namespaces, `use`, PSR-4 multi-file projects | ✅ |
| npm escape hatch (configurable namespace prefixes → npm packages) | ✅ |
| Generics on classes / functions (PHP 9 RFC) | ⏳ tracked upstream |

### Toolchain

| Component | Status |
|---|---|
| `psx` CLI (single-file and project `build`) | ✅ |
| Source maps — `.psx → .ts → .js` chain with column-level resolution | ✅ |
| `psx-lsp` language server: diagnostics, hover, goto-definition, document + workspace symbols, completions, signature help, rename, code actions, formatting, inlay hints | ✅ |
| VS Code extension (syntax highlighting + LSP client) | ✅ |
| `psx-printer` AST → `.psx` pretty-printer (LSP formatter backend) | ✅ |
| CI workflow (fmt + matrix tests + end-to-end smoke) | ✅ |
| Release workflow (multi-platform binary builds, npm publish, crates.io publish, VS Code Marketplace) | ✅ wired, awaiting first tag |

## Quickstart

```bash
git clone https://github.com/bimsonz/elephpant
cd elephpant
cargo build --release
```

Single file:

```bash
./target/release/psx build examples/hello/main.psx
# writes examples/hello/dist/main.ts (+ main.ts.map)
```

Multi-file project (uses `psx.json` for namespace + paths):

```bash
cd examples/multi-file
../../target/release/psx build
# src/**/*.psx -> dist/**/*.ts (+ .ts.map files)
```

Type-check and run the emitted TS:

```bash
npx tsc --strict --target es2022 --module esnext --moduleResolution bundler \
  --rootDir dist --outDir dist-js dist/**/*.ts
echo '{"type":"module"}' > dist-js/package.json
node dist-js/Main.js
```

Once published, the same flow via npm:

```bash
npm install --save-dev elephpant
npx psx build src/
```

## Examples

Every example below is a real working `.psx` project that compiles and runs.

| Example | Demonstrates |
|---|---|
| [`examples/hello`](examples/hello) | Functions, control flow, `foreach`, optional params |
| [`examples/oop`](examples/oop) | Classes, interfaces, enums, abstracts, `readonly`, statics |
| [`examples/multi-file`](examples/multi-file) | `psx.json`, namespaces, `use`, PSR-4 imports |
| [`examples/async-demo`](examples/async-demo) | `async`/`await`, instance + module-level async functions |
| [`examples/error-handling`](examples/error-handling) | `match`, ternary, `throw`/`try`/`catch`, `instanceof` |
| [`examples/hooks-demo`](examples/hooks-demo) | PHP 8.4 property hooks, asymmetric visibility, FCC `(...)` |
| [`examples/traits-demo`](examples/traits-demo) | Multi-file traits + `insteadof`/`as` resolution |

## What the language looks like

```php
// Type-annotated variables and functions.
function classify(int $n): string {
    return match (true) {
        $n > 0  => "positive",
        $n < 0  => "negative",
        default => "zero",
    };
}

// Constructor property promotion + readonly + asymmetric visibility.
class Entity {
    public function __construct(
        public private(set) readonly string $id,
    ) {}
}

// String interpolation lowers to TS template literals.
function greet(User $u): string {
    return "Hello, {$u->email} (id={$u->id})";
}

// Traits inline at compile time. No runtime mixin shim.
trait Timestamps {
    public int $createdAt = 0;
    public function touch(): void { $this->createdAt = time(); }
}
class Post { use Timestamps; }

// Async/await with PHP-flavored syntax.
async function loadJSON(string $url): Promise<mixed> {
    $res = await fetch($url);
    return await $res->json();
}

// First-class callable (PHP 8.1).
$fn = $user->emailGetter(...);
```

The full language specification lives in [`docs/design.md`](./docs/design.md). For the explicit mapping of every PHPScript construct to its real-PHP source (RFC, manual page, deliberate deviations), see [`docs/php-fidelity.md`](./docs/php-fidelity.md) — that document is the canonical reference for "is this real PHP?" and gets updated whenever upstream PHP changes.

## Project structure

```
crates/
  psx-lexer/      Tokenizer
  psx-ast/        AST node definitions
  psx-parser/     Hand-rolled recursive-descent parser
  psx-resolver/   psx.json + PSR-4 namespace → file path
  psx-sourcemap/  Source-map-v3 builder (LineMap, VLQ, builder)
  psx-emitter/    AST → TypeScript source
  psx-printer/    AST → .psx pretty-printer (used by the LSP formatter)
  psx-cli/        `psx` binary
  psx-lsp/        `psx-lsp` Language Server Protocol implementation
editors/vscode/   VS Code extension (TextMate grammar + LSP client)
examples/         Working .psx projects (see table above)
docs/             Language + architecture spec
npm/psx/          npm wrapper (postinstall downloads the prebuilt binary)
```

## Design principles

- **PHPScript is its own language.** Modern PHP-inspired syntax, with PHP's deprecated constructs deliberately out: no `var` keyword, no `array()` long-form, no alternative-syntax `if/endif`. Constructor property promotion is the default. `==` always emits as `===`.
- **No runtime library.** Every construct lowers to native JS/TS at compile time. Output is the same size as hand-written TypeScript — no coercion shim, no `PHPArray` wrapper.
- **TypeScript is the type checker.** We emit valid TS and hand off to `tsc --strict`. The PHPScript compiler doesn't reimplement type checking; it produces TS that does.
- **PHP semantics where they match JS, JS semantics elsewhere.** Arrays are JS arrays. Hash-map literals (`["k" => "v"]`) lower to JS object literals — full JS interop wins over PHP authenticity.
- **Civet's playbook.** Compile to TypeScript source, hand off to the TS ecosystem. We inherit `tsc`, `esbuild`, source maps, IDE support, npm interop — all of it, for free.

## Editor support

A VS Code extension ships syntax highlighting + language-server support for `.psx` files:

```bash
code --install-extension bimsonz.psx-vscode
```

Behind the scenes it talks to the `psx-lsp` binary, which provides:

- Live parse-error diagnostics.
- Hover with statement-level context.
- Goto-definition for cross-file `use App\Foo;` directives.
- Document symbols (classes / interfaces / enums / traits / functions in the outline).
- Workspace symbols (search any declaration project-wide).
- Completions (keywords, workspace symbols, `$this->` members inside method bodies).
- Signature help inside function calls.
- File-local variable rename.
- A code-action quick-fix for `array(...)` → `[...]`.
- Document formatting via the `psx-printer` crate.
- Inlay hints for inferred literal types.

Build it locally from [`editors/vscode/`](./editors/vscode/) — `F5` in that directory opens a dev host.

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md). The short version:

- TDD-first. Every behavior change lands with a test.
- `cargo test` before every commit.
- New language features ship with an end-to-end snapshot test and a working `.psx` example.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE))
- MIT License ([LICENSE-MIT](./LICENSE-MIT))

at your option.
