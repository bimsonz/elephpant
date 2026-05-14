# `async-demo` example

`async`/`await` on module-level functions and on instance methods. PHPScript adopts the JS keywords (PHP has fibers, not async/await), and the emitter auto-wraps the declared return type in `Promise<…>` when the function is async.

Run:

```bash
../../target/release/psx build main.psx
```
