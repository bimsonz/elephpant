# PHP Fidelity

PHPScript borrows syntax from real PHP. This document is the canonical map between every PHPScript construct and its PHP source (RFC, manual page, or version), so future changes can be checked against the actual spec before they ship.

The risk this doc exists to prevent: improvising syntax that *looks* like PHP but isn't, or silently accepting forms PHP itself deprecated. Every row below either points at an RFC / manual page we follow exactly, or flags a deliberate deviation with its rationale.

## Rules

1. **Default to real PHP syntax.** If PHP has a syntax for the feature, ours must match it byte-for-byte. Don't invent qualifiers or extra keywords just because they "feel symmetric".
2. **Deliberate deviations are flagged in this table** under "Deviation". Without an entry here, our parser MUST reject syntax that PHP rejects and accept syntax PHP accepts.
3. **Every new language feature ships with a row here** plus a "spec test" — a snippet from real PHP that should parse, and (where applicable) a snippet PHP rejects that we also reject.
4. **When PHP changes, we change.** If the upstream RFC adjusts before the feature reaches stable PHP, we follow.

## Feature table

| Feature | PHP version | PHP source | PHPScript syntax | Status |
|---|---|---|---|---|
| `<?php` opener | always | [Manual: PHP tags](https://www.php.net/manual/en/language.basic-syntax.phptags.php) | **Not required** — `.psx` files are bare code. | **Deviation** (intentional) |
| Variables with `$` | always | [Manual: variables](https://www.php.net/manual/en/language.variables.basics.php) | `$name` | Faithful |
| Local-variable type annotation | not in PHP | — | **Not supported** (`$x: int = 5` is a parse error). | Deviation (rejected) |
| `function name(T $a): R { … }` | 7.0 (types), 7.4 (param defaults) | [Manual: functions](https://www.php.net/manual/en/language.functions.php) | Same | Faithful |
| Arrow functions `fn(T $x): R => expr` | 7.4 | [RFC: Arrow functions 2.0](https://wiki.php.net/rfc/arrow_functions_v2) | Same. Body is a single expression — multi-statement closures use the full `function(){}` form (deferred). | Faithful |
| `++` / `--` (pre + post fix) | always | [Manual: Incrementing/Decrementing](https://www.php.net/manual/en/language.operators.increment.php) | Same syntax. Emits identical TS. | Faithful |
| `for (init; cond; step)` | always | [Manual: for](https://www.php.net/manual/en/control-structures.for.php) | Same. Any of init/cond/step may be empty. MVP: single expression per slot (PHP's comma-separated list deferred). | Faithful |
| `do { … } while (cond);` | always | [Manual: do-while](https://www.php.net/manual/en/control-structures.do.while.php) | Same | Faithful |
| `break [N];` / `continue [N];` | always | [Manual: break](https://www.php.net/manual/en/control-structures.break.php) | Optional integer level supported. Levels > 1 emit a comment marker (TS has no multi-level break — refactor needed). | Faithful (with TS-side caveat) |
| `match` expression | 8.0 | [RFC: match](https://wiki.php.net/rfc/match_expression_v2) | Same | Faithful |
| Constructor property promotion | 8.0 | [RFC: constructor promotion](https://wiki.php.net/rfc/constructor_promotion) | Same | Faithful |
| Nullable types `?T` | 7.1 | [RFC: nullable types](https://wiki.php.net/rfc/nullable_types) | Same | Faithful |
| Union types `A\|B` | 8.0 | [RFC: union types v2](https://wiki.php.net/rfc/union_types_v2) | Same | Faithful |
| `never` return type | 8.1 | [RFC: never](https://wiki.php.net/rfc/noreturn_type) | Same | Faithful |
| `?->` null-safe operator | 8.0 | [RFC: nullsafe operator](https://wiki.php.net/rfc/nullsafe_operator) | Same | Faithful |
| Enums (pure + backed) | 8.1 | [RFC: enumerations](https://wiki.php.net/rfc/enumerations) | Same | Faithful |
| `readonly` properties | 8.1 | [RFC: readonly properties v2](https://wiki.php.net/rfc/readonly_properties_v2) | Same | Faithful |
| `readonly class Foo` | 8.2 | [RFC: readonly classes](https://wiki.php.net/rfc/readonly_classes) | Same | Faithful |
| First-class callable `target(...)` | 8.1 | [RFC: first-class callable syntax](https://wiki.php.net/rfc/first_class_callable_syntax) | Same | Faithful |
| Property hooks `get =>` / `set(…) =>` | 8.4 | [RFC: property hooks](https://wiki.php.net/rfc/property-hooks) | Same | Faithful |
| Asymmetric visibility `public private(set)` | 8.4 | [RFC: asymmetric visibility](https://wiki.php.net/rfc/asymmetric-visibility) | **Same** — read side bare, write side `(set)`. `(get)` is NOT a real PHP qualifier and is rejected. Write visibility must be ≤ read visibility (e.g., `private public(set)` is invalid). | Faithful |
| Traits + `insteadof` / `as` | 5.4 | [Manual: traits](https://www.php.net/manual/en/language.oop5.traits.php) | Same | Faithful |
| Spaceship `<=>` | 7.0 | [RFC: combined comparison operator](https://wiki.php.net/rfc/combined-comparison-operator) | Same | Faithful |
| `??` null-coalesce | 7.0 | [RFC: null coalesce operator](https://wiki.php.net/rfc/isset_ternary) | Same | Faithful |
| `?:` short ternary (elvis) | 5.3 | Manual | Same | Faithful |
| `==` / `!=` loose equality | always | Manual | **Rejected at parse time** — use `===` / `!==`. | Deviation (intentional) |
| `var $x` property declaration | deprecated since 5.0 | Manual | **Rejected at parse time** — use `public` / `protected` / `private`. | Deviation (intentional) |
| `array(…)` long-form constructor | always (deprecated style) | [Manual: arrays](https://www.php.net/manual/en/language.types.array.php) | **Rejected at parse time** — use the short `[…]` literal. | Deviation (intentional) |
| `if (…): … endif;` alt syntax | always | Manual | **Not parsed** — rejected via "unexpected `:`" error. | Deviation (intentional) |
| String interpolation `"$var"`, `"{$expr}"` | always | [Manual: strings](https://www.php.net/manual/en/language.types.string.php) | Same | Faithful |
| Namespaces, `use`, PSR-4 layout | 5.3 (namespaces), PSR-4 | [PSR-4](https://www.php-fig.org/psr/psr-4/) | Same syntax; PSR-4 file layout enforced by the project resolver. | Faithful |
| `async` / `await` | not in PHP | JS-borrowed (PHP has fibers, not async/await) | `async function foo(): Promise<T>`, `await $expr`. Return type auto-wrapped as `Promise<…>`. | **Deviation (PHPScript addition)** |
| Generic type annotations `array<T>`, `Promise<T>` | not in PHP yet | PHPDoc + PHPStan/Psalm convention; PHP 9 RFC tracked | Accepted as real syntax. May be re-shaped once the PHP 9 generics RFC lands. | **Deviation (interim)** |
| Generics on class / function declarations (`class Box<T>`, `function map<T, U>`) | PHP 9 RFC (draft) | [RFC: generic types](https://wiki.php.net/rfc/generics) | **Not yet supported**. Will follow whatever syntax PHP 9 ships. | Tracked upstream |

## Spec-test convention

For each row in the table marked "Faithful", we keep at least one parser unit test asserting a real-PHP snippet parses with the expected AST. For rows marked "Deviation (rejected)", we keep a test asserting our parser produces a clear error.

When a real PHP RFC lands or changes:

1. Re-read the RFC.
2. Check this table — update any row whose syntax shifted.
3. Update the parser, add a snapshot/spec test for the new shape, mark the old syntax as rejected if it was retired.
4. Bump the relevant entry in the README's "What's done" table only after the parser + emitter are in sync with the RFC text.

## Reviewers checklist

Before merging a change that touches syntax:

- [ ] If the change adds a syntactic feature, this table has a new row pointing at the PHP source.
- [ ] If the change modifies an existing feature, the row is updated and the parser tests reflect the new behaviour.
- [ ] If the change is a deliberate PHPScript deviation, it's marked "Deviation" with the rationale in the entry.
- [ ] At least one parser test exercises a copy-pasted snippet from the PHP manual / RFC examples.
- [ ] At least one parser test asserts the invalid form is rejected (PHP rejects it → we must reject it).
