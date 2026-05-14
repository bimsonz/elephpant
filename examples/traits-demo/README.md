# `traits-demo` example

Multi-file traits with the full PHP feature set:

- `src/Traits/Greetable.psx` + `src/Traits/Shouty.psx` — two traits that both define `greet` (deliberate conflict).
- `src/Models/User.psx` — uses just `Greetable` (`use Greetable;`).
- `src/Models/Admin.psx` — uses both, resolves the conflict via `insteadof`, and exposes the loser's version under a new name via `as`:
  ```php
  use Greetable, Shouty {
      Greetable::greet insteadof Shouty;
      Greetable::greeting insteadof Shouty;
      Shouty::greet as shoutyGreet;
  }
  ```

The compiler builds a project-wide trait map in pass 1, then inlines each trait's members directly into the using class at emit time, applying any `insteadof` / `as` adaptations. Transitive expansion (a trait `use`-ing another trait) is supported too, with cycle detection.

Run:

```bash
../../target/release/psx build
# src/**/*.psx -> dist/**/*.ts
node run-demo.mjs    # after tsc has produced dist-js/
```

Expected output:

```
greeting: Hello, Ada
admin:    Hello, root | Hello! root
```
