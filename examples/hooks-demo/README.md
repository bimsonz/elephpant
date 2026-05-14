# `hooks-demo` example

PHP 8.4 surface area: property hooks (`get =>` / `set(...) =>`), asymmetric visibility (`public private(set)`), and first-class callable (`$obj->method(...)`). The emitter lowers each to the tightest TS shape — backing fields for hooks, `readonly` for the common asym-vis case, and either a bare `.bind(...)` or a single-eval IIFE for FCC depending on whether the target is a pure expression.

Run:

```bash
../../target/release/psx build main.psx
```
