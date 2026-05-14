//! End-to-end snapshot tests: `.psx` source -> TypeScript source.
//!
//! These define the language. When grammar or emit shape changes, snapshots
//! update here in lockstep with the lexer/parser/emitter unit tests.

use psx_cli::{compile_project, compile_project_with, compile_str, CompileOptions};
use psx_resolver::PsxConfig;

#[test]
fn integer_expression_statement() {
    let ts = compile_str("42;").unwrap();
    insta::assert_snapshot!(ts, @"42;");
}

#[test]
fn multiple_integer_statements() {
    let ts = compile_str("1; 2; 3;").unwrap();
    insta::assert_snapshot!(ts, @"
    1;
    2;
    3;
    ");
}

#[test]
fn whitespace_and_comments_in_source_do_not_appear_in_output() {
    let src = "// header
/* block */
42; // trailing
# also
7;";
    let ts = compile_str(src).unwrap();
    insta::assert_snapshot!(ts, @"
    42;
    7;
    ");
}

#[test]
fn empty_source_produces_empty_output() {
    let ts = compile_str("").unwrap();
    assert_eq!(ts, "");
}

#[test]
fn literal_expression_round_trip() {
    let src = r#"42;
3.14;
"hello";
true;
false;
null;
"#;
    let ts = compile_str(src).unwrap();
    insta::assert_snapshot!(ts, @r#"
    42;
    3.14;
    "hello";
    true;
    false;
    null;
    "#);
}

#[test]
fn integral_float_literal_keeps_dot_in_output() {
    // PHP-faithful lexing reads `42.` as Float(42.0). Emit must preserve the
    // float-ness so it's distinguishable from an integer in the TS output.
    let ts = compile_str("42.;").unwrap();
    insta::assert_snapshot!(ts, @"42.0;");
}

#[test]
fn string_with_internal_escapes_round_trips() {
    // Source: "hi\n\"x\"" — newline + escaped quotes. Round-trip should give
    // valid TS with the same escapes.
    let src = r#""hi\n\"x\"";"#;
    let ts = compile_str(src).unwrap();
    insta::assert_snapshot!(ts, @r#""hi\n\"x\"";"#);
}

#[test]
fn single_quoted_source_emits_double_quoted_ts() {
    // PHPScript single-quoted strings carry the same content as double in
    // most cases; the emitter normalizes to double-quoted TS.
    let ts = compile_str(r"'hello';").unwrap();
    insta::assert_snapshot!(ts, @r#""hello";"#);
}

#[test]
fn variable_reference_drops_dollar() {
    let ts = compile_str("$x; $userName; $_GET;").unwrap();
    insta::assert_snapshot!(ts, @r"
    x;
    userName;
    _GET;
    ");
}

#[test]
fn first_assignment_emits_let_declaration() {
    let ts = compile_str("$x = 42;").unwrap();
    insta::assert_snapshot!(ts, @"let x = 42;");
}

#[test]
fn reassignment_does_not_redeclare() {
    let ts = compile_str("$x = 1; $x = 2; $x = 3;").unwrap();
    insta::assert_snapshot!(ts, @r"
    let x = 1;
    x = 2;
    x = 3;
    ");
}

#[test]
fn each_variable_gets_its_own_let() {
    let ts = compile_str(
        r#"
$name = "world";
$count = 0;
$count = 1;
$flag = true;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let name = "world";
    let count = 0;
    count = 1;
    let flag = true;
    "#);
}

#[test]
fn arithmetic_round_trip() {
    let ts = compile_str("$x = 1 + 2 * 3 - 4 / 2;").unwrap();
    insta::assert_snapshot!(ts, @"let x = 1 + 2 * 3 - 4 / 2;");
}

#[test]
fn parentheses_preserved_when_meaningful() {
    let ts = compile_str("$x = (1 + 2) * 3;").unwrap();
    insta::assert_snapshot!(ts, @"let x = (1 + 2) * 3;");
}

#[test]
fn unary_negation_round_trip() {
    let ts = compile_str("$x = -42; $y = -$x;").unwrap();
    insta::assert_snapshot!(ts, @r"
    let x = -42;
    let y = -x;
    ");
}

#[test]
fn power_is_right_associative_in_output() {
    let ts = compile_str("$x = 2 ** 3 ** 2;").unwrap();
    insta::assert_snapshot!(ts, @"let x = 2 ** 3 ** 2;");
}

#[test]
fn string_concat_round_trip() {
    let ts = compile_str(r#"$greeting = "hello" . " " . "world";"#).unwrap();
    insta::assert_snapshot!(ts, @r#"let greeting = "hello" + " " + "world";"#);
}

/// PHP 8 lowered `.` below `+ -`. Since we parse with that precedence and
/// emit `.` as `+`, our output uses parens to preserve meaning when the
/// arithmetic was meant to bind tighter.
#[test]
fn arithmetic_then_concat_keeps_meaning() {
    // PHP-side: (1 + 2) . "x" -> "3x"
    // We emit `+` for both. Parens keep the addition together so JS reads
    // `(1 + 2) + "x"` -> "3x", same as PHP.
    let ts = compile_str(r#"$s = 1 + 2 . "x";"#).unwrap();
    insta::assert_snapshot!(ts, @r#"let s = 1 + 2 + "x";"#);
}

#[test]
fn comparison_round_trip() {
    let ts = compile_str("$ok = $a < $b;\n$same = $a === $b;\n$diff = $a !== $b;").unwrap();
    insta::assert_snapshot!(ts, @r"
    let ok = a < b;
    let same = a === b;
    let diff = a !== b;
    ");
}

#[test]
fn loose_equality_in_source_produces_parse_error() {
    let err = compile_str("$x = 1 == 2;").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("`==`") && msg.contains("`===`"), "got: {msg}");
}

#[test]
fn logical_and_coalesce_round_trip() {
    let ts = compile_str("$x = $a && $b || $c ?? $d;").unwrap();
    // Per design: && > || > ??. Source parses as `(a && b) || c) ?? d`.
    // Right operand of `||` is just `c`; inner `||` binds before `??`.
    insta::assert_snapshot!(ts, @"let x = a && b || c ?? d;");
}

#[test]
fn null_coalesce_with_assignment_pattern() {
    // PHP idiom: `$name = $input ?? "default"`.
    let ts = compile_str(r#"$name = $input ?? "default";"#).unwrap();
    insta::assert_snapshot!(ts, @r#"let name = input ?? "default";"#);
}

#[test]
fn compound_assignment_round_trip() {
    let ts = compile_str(
        "$x = 0;
$x += 1;
$x -= 1;
$x *= 2;
$x /= 2;
$x %= 2;
$x **= 2;
$x ??= 99;
",
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let x = 0;
    x += 1;
    x -= 1;
    x *= 2;
    x /= 2;
    x %= 2;
    x **= 2;
    x ??= 99;
    ");
}

#[test]
fn dot_equals_emits_plus_equals() {
    let ts = compile_str(
        r#"$s = "hi";
$s .= " there";
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let s = "hi";
    s += " there";
    "#);
}

/// `$msg` is assigned only inside `if` branches. The hoist pass lifts it to a
/// `let msg;` line at the top so PHP-style cross-block visibility holds in JS.
#[test]
fn if_else_round_trip() {
    let ts = compile_str(
        r#"$x = 1;
if ($x === 1) {
    $msg = "one";
} elseif ($x === 2) {
    $msg = "two";
} else {
    $msg = "other";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let msg;
    let x = 1;
    if (x === 1) {
      msg = "one";
    } else if (x === 2) {
      msg = "two";
    } else {
      msg = "other";
    }
    "#);
}

#[test]
fn nested_if_inside_block_round_trip() {
    let ts = compile_str(
        r#"
if ($a) {
    if ($b) {
        $z = 1;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let z;
    if (a) {
      if (b) {
        z = 1;
      }
    }
    ");
}

#[test]
fn if_with_unbraced_body_inline() {
    let ts = compile_str("if ($x) $count = 1;").unwrap();
    // `count` is hoisted because it's first-declared inside the if-body. The
    // result is valid JS where `count` is visible after the if (PHP semantic).
    insta::assert_snapshot!(ts, @r"
    let count;
    if (x) count = 1;
    ");
}

/// Variables assigned only at module level still get the compact inline-let
/// form. The hoist pre-pass only touches names assigned below module level.
#[test]
fn module_level_only_vars_keep_compact_let() {
    let ts = compile_str("$x = 1; $y = 2; $x = 3;").unwrap();
    insta::assert_snapshot!(ts, @r"
    let x = 1;
    let y = 2;
    x = 3;
    ");
}

/// A variable assigned at both module level and inside a block: the block
/// references the outer one (no hoist needed since module-level let already
/// declared it).
#[test]
fn variable_assigned_at_both_module_and_block_uses_outer_let() {
    let ts = compile_str(
        r#"$x = 1;
if (true) {
    $x = 2;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let x = 1;
    if (true) {
      x = 2;
    }
    ");
}

#[test]
fn return_round_trip() {
    let ts = compile_str("return 42;").unwrap();
    insta::assert_snapshot!(ts, @"return 42;");
}

#[test]
fn while_loop_round_trip() {
    let ts = compile_str(
        r#"$i = 0;
while ($i < 3) {
    $i += 1;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let i = 0;
    while (i < 3) {
      i += 1;
    }
    ");
}

#[test]
fn foreach_value_only_round_trip() {
    let ts = compile_str(
        r#"foreach ($items as $item) {
    $item;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    for (const item of items) {
      item;
    }
    ");
}

#[test]
fn foreach_key_value_round_trip() {
    let ts = compile_str(
        r#"foreach ($users as $id => $user) {
    $id;
    $user;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    for (const [id, user] of Object.entries(users)) {
      id;
      user;
    }
    ");
}

#[test]
fn function_call_round_trip() {
    let ts = compile_str(
        r#"
$result = add(1, 2);
greet($name, "world");
foo();
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let result = add(1, 2);
    greet(name, "world");
    foo();
    "#);
}

#[test]
fn nested_calls_round_trip() {
    let ts = compile_str("$x = max(min($a, $b), 0);").unwrap();
    insta::assert_snapshot!(ts, @"let x = max(min(a, b), 0);");
}

#[test]
fn function_declaration_round_trip() {
    let ts = compile_str(
        r#"function add(int $a, int $b): int {
    return $a + $b;
}

$result = add(1, 2);
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    function add(a: number, b: number): number {
      return a + b;
    }
    let result = add(1, 2);
    ");
}

#[test]
fn function_with_default_value_round_trip() {
    let ts = compile_str(
        r#"function greet(string $name = "world"): string {
    return "Hello, " . $name;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    function greet(name: string = "world"): string {
      return "Hello, " + name;
    }
    "#);
}

#[test]
fn function_with_complex_types_round_trip() {
    let ts = compile_str(
        r#"function find(array<User> $users, string $name): ?User {
    foreach ($users as $user) {
        if ($user === $name) {
            return $user;
        }
    }
    return null;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    function find(users: User[], name: string): User | null {
      for (const user of users) {
        if (user === name) {
          return user;
        }
      }
      return null;
    }
    ");
}

#[test]
fn function_local_scope_isolated_from_module() {
    let ts = compile_str(
        r#"$x = "module";
function shadow(): void {
    $x = "local";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let x = "module";
    function shadow(): void {
      let x = "local";
    }
    "#);
}

/// Variables hoisted inside a function body don't leak into the module-level
/// hoist set.
#[test]
fn function_inner_hoist_isolated() {
    let ts = compile_str(
        r#"function compute(int $n): string {
    if ($n > 0) {
        $msg = "positive";
    } else {
        $msg = "non-positive";
    }
    return $msg;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    function compute(n: number): string {
      let msg;
      if (n > 0) {
        msg = "positive";
      } else {
        msg = "non-positive";
      }
      return msg;
    }
    "#);
}

#[test]
fn array_literal_round_trip() {
    let ts = compile_str(
        r#"$nums = [1, 2, 3];
$user = ["name" => "Ada", "age" => 36];
$mixedKeys = ["first-name" => "Ada", "lastName" => "Lovelace"];
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let nums = [1, 2, 3];
    let user = { name: "Ada", age: 36 };
    let mixedKeys = { "first-name": "Ada", lastName: "Lovelace" };
    "#);
}

#[test]
fn array_index_round_trip() {
    let ts = compile_str(
        r#"$first = $nums[0];
$user["age"] = 37;
$grid[0][1] = "x";
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let first = nums[0];
    user["age"] = 37;
    grid[0][1] = "x";
    "#);
}

#[test]
fn foreach_over_array_literal() {
    let ts = compile_str(
        r#"foreach ([1, 2, 3] as $n) {
    $sum = 0;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let sum;
    for (const n of [1, 2, 3]) {
      sum = 0;
    }
    ");
}

#[test]
fn member_access_round_trip() {
    let ts = compile_str(
        r#"$name = $user->name;
$email = $user?->profile?->email;
$role = $user->role();
$max = Math::max(1, 2);
$pi = Math::$PI;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let name = user.name;
    let email = user?.profile?.email;
    let role = user.role();
    let max = Math.max(1, 2);
    let pi = Math.PI;
    ");
}

#[test]
fn property_write_round_trip() {
    let ts = compile_str(
        r#"$this->name = "Ada";
$this->age = $this->age + 1;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    this.name = "Ada";
    this.age = this.age + 1;
    "#);
}

#[test]
fn chained_method_calls_round_trip() {
    let ts = compile_str(r#"$result = $api->get("/users")->json()->users;"#).unwrap();
    insta::assert_snapshot!(ts, @r#"let result = api.get("/users").json().users;"#);
}

#[test]
fn new_expression_round_trip() {
    let ts = compile_str(
        r#"$user = new User("Ada", 36);
$empty = new Bag();
$bag = new Bag<int>(5);
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let user = new User("Ada", 36);
    let empty = new Bag();
    let bag = new Bag<number>(5);
    "#);
}

#[test]
fn class_skeleton_round_trip() {
    let ts = compile_str(
        r#"class User {
    public string $name;
    private int $age = 0;

    public function __construct(string $name, int $age) {
        $this->name = $name;
        $this->age = $age;
    }

    public function greet(): string {
        return "Hello, " . $this->name;
    }
}

$ada = new User("Ada", 36);
$msg = $ada->greet();
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class User {
      public name: string;
      private age: number = 0;
      public constructor(name: string, age: number) {
        this.name = name;
        this.age = age;
      }
      public greet(): string {
        return "Hello, " + this.name;
      }
    }
    let ada = new User("Ada", 36);
    let msg = ada.greet();
    "#);
}

#[test]
fn readonly_property_round_trip() {
    let ts = compile_str(
        r#"class User {
    public readonly string $id;
    public string $email;

    public function __construct(string $id, string $email) {
        $this->id = $id;
        $this->email = $email;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class User {
      public readonly id: string;
      public email: string;
      public constructor(id: string, email: string) {
        this.id = id;
        this.email = email;
      }
    }
    ");
}

/// PHP 8.2 `readonly class Foo {}` propagates `readonly` to every property.
#[test]
fn readonly_class_propagates_to_properties() {
    let ts = compile_str(
        r#"readonly class Point {
    public function __construct(
        public int $x,
        public int $y,
    ) {}
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Point {
      public constructor(public readonly x: number, public readonly y: number) {}
    }
    ");
}

#[test]
fn abstract_class_with_abstract_method() {
    let ts = compile_str(
        r#"abstract class Animal {
    public function __construct(public string $name) {}

    public abstract function speak(): string;

    public function announce(): string {
        return $this->name . " says " . $this->speak();
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    abstract class Animal {
      public constructor(public name: string) {}
      public abstract speak(): string;
      public announce(): string {
        return this.name + " says " + this.speak();
      }
    }
    "#);
}

/// `final` is parsed but dropped on emit (TS has no equivalent).
#[test]
fn final_class_drops_silently() {
    let ts = compile_str(
        r#"final class Sealed {
    public string $name;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Sealed {
      public name: string;
    }
    ");
}

#[test]
fn static_property_and_method_round_trip() {
    let ts = compile_str(
        r#"class Counter {
    public static int $count = 0;

    public static function bump(): void {
        Counter::$count = Counter::$count + 1;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Counter {
      public static count: number = 0;
      public static bump(): void {
        Counter.count = Counter.count + 1;
      }
    }
    ");
}

#[test]
fn class_extends_emits_ts_extends() {
    let ts = compile_str(
        r#"class Animal {
    public function __construct(public string $name) {}
}

class Dog extends Animal {
    public function bark(): string {
        return $this->name . ": woof";
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class Animal {
      public constructor(public name: string) {}
    }
    class Dog extends Animal {
      public bark(): string {
        return this.name + ": woof";
      }
    }
    "#);
}

#[test]
fn parent_call_emits_super() {
    let ts = compile_str(
        r#"class Animal {
    public function __construct(public string $name) {}
}

class Dog extends Animal {
    public function __construct(string $name, public string $breed) {
        parent::__construct($name);
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Animal {
      public constructor(public name: string) {}
    }
    class Dog extends Animal {
      public constructor(name: string, public breed: string) {
        super(name);
      }
    }
    ");
}

#[test]
fn self_call_resolves_to_class_name() {
    let ts = compile_str(
        r#"class Counter {
    public static int $count = 0;

    public static function bump(): void {
        self::$count = self::$count + 1;
    }

    public static function fresh(): self {
        return new self();
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Counter {
      public static count: number = 0;
      public static bump(): void {
        Counter.count = Counter.count + 1;
      }
      public static fresh(): Counter {
        return new Counter();
      }
    }
    ");
}

#[test]
fn interface_round_trip() {
    let ts = compile_str(
        r#"interface Auditable {
    public function audit(): void;
}

interface Loggable {
    public function log(string $message): void;
}

interface FullAudit extends Auditable, Loggable {
    public const string EVENT = "audit";
    public function summarize(): string;
}

class User implements FullAudit {
    public function audit(): void {}
    public function log(string $message): void {}
    public function summarize(): string {
        return "ok";
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    interface Auditable {
      audit(): void;
    }
    interface Loggable {
      log(message: string): void;
    }
    interface FullAudit extends Auditable, Loggable {
      // const EVENT: string = "audit";
      summarize(): string;
    }
    class User implements FullAudit {
      public audit(): void {}
      public log(message: string): void {}
      public summarize(): string {
        return "ok";
      }
    }
    "#);
}

#[test]
fn throw_and_catch_round_trip() {
    let ts = compile_str(
        r#"function fetch(string $url): string {
    if ($url === "") {
        throw new InvalidArgumentException("empty url");
    }
    return $url;
}

try {
    $page = fetch("");
} catch (InvalidArgumentException $e) {
    $page = "default";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let page;
    function fetch(url: string): string {
      if (url === "") {
        throw new InvalidArgumentException("empty url");
      }
      return url;
    }
    try {
      page = fetch("");
    } catch (__e: unknown) {
      if (__e instanceof InvalidArgumentException) {
        let e = __e;
        page = "default";
      } else {
        throw __e;
      }
    }
    "#);
}

#[test]
fn try_with_multi_type_catch() {
    let ts = compile_str(
        r#"try {
    $r = 1;
} catch (FooError | BarError $e) {
    $r = 0;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let r;
    try {
      r = 1;
    } catch (__e: unknown) {
      if (__e instanceof FooError || __e instanceof BarError) {
        let e = __e;
        r = 0;
      } else {
        throw __e;
      }
    }
    ");
}

#[test]
fn try_with_finally() {
    let ts = compile_str(
        r#"try {
    $r = 1;
} catch (Exception $e) {
    $r = 0;
} finally {
    $cleanup = true;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    let cleanup, r;
    try {
      r = 1;
    } catch (__e: unknown) {
      if (__e instanceof Exception) {
        let e = __e;
        r = 0;
      } else {
        throw __e;
      }
    } finally {
      cleanup = true;
    }
    ");
}

// ---------- Phase 5: traits ----------

#[test]
fn same_file_trait_inlines_into_using_class() {
    let ts = compile_str(
        r#"trait Greetable {
    public string $greeting = "Hello";

    public function greet(string $name): string {
        return $this->greeting . ", " . $name;
    }
}

class User {
    use Greetable;

    public string $email;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class User {
      public greeting: string = "Hello";
      public greet(name: string): string {
        return this.greeting + ", " + name;
      }
      public email: string;
    }
    "#);
}

#[test]
fn trait_with_multiple_traits_and_class_wins_on_conflict() {
    let ts = compile_str(
        r#"trait Greetable {
    public function greet(): string { return "hi"; }
}

trait Audible {
    public function shout(): string { return "OY"; }
}

class Mascot {
    use Greetable, Audible;

    // Class-defined `greet` overrides the trait's.
    public function greet(): string { return "hello!"; }
}
"#,
    )
    .unwrap();
    // Expansion happens in source order: UseTrait (which adds non-conflicting
    // trait members in the order traits are listed) then the class-defined
    // members. Greetable's `greet` is skipped since the class defines its own.
    insta::assert_snapshot!(ts, @r#"
    class Mascot {
      public shout(): string {
        return "OY";
      }
      public greet(): string {
        return "hello!";
      }
    }
    "#);
}

#[test]
fn trait_not_found_emits_marker() {
    // No trait declaration, just a `use Missing;` reference. The emitter
    // should leave a visible marker in the class body so tsc fails loudly.
    let ts = compile_str(
        r#"class C {
    use Missing;
}
"#,
    )
    .unwrap();
    assert!(ts.contains("trait `Missing` not found"));
}

#[test]
fn multi_file_trait_use_does_not_emit_import_and_inlines_members() {
    // A use-decl pointing at a trait in another file MUST be silently dropped
    // from the import header — the trait file emits nothing, so an import
    // would dangle. The trait body must still be inlined into the using class.
    let tmp = std::env::temp_dir().join(format!("psx-traits-multifile-{}", std::process::id(),));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src/Traits")).unwrap();
    std::fs::create_dir_all(tmp.join("src/Models")).unwrap();
    std::fs::write(
        tmp.join("src/Traits/Greetable.psx"),
        "namespace App\\Traits;\n\ntrait Greetable {\n    public function greet(): string { return \"hi\"; }\n}\n",
    ).unwrap();
    std::fs::write(
        tmp.join("src/Models/User.psx"),
        "namespace App\\Models;\n\nuse App\\Traits\\Greetable;\n\nclass User {\n    use Greetable;\n}\n",
    ).unwrap();
    let cfg = PsxConfig {
        namespace: "App".into(),
        src: "src".into(),
        out_dir: "dist".into(),
        npm_prefixes: Default::default(),
    };
    compile_project(&cfg, &tmp).unwrap();
    let user_ts = std::fs::read_to_string(tmp.join("dist/Models/User.ts")).unwrap();
    let trait_ts = std::fs::read_to_string(tmp.join("dist/Traits/Greetable.ts")).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !user_ts.contains("import"),
        "User.ts should not import from the erased trait file, got:\n{user_ts}"
    );
    assert!(
        user_ts.contains("greet()"),
        "trait method should be inlined into User, got:\n{user_ts}"
    );
    // The trait file emits only the trailing `sourceMappingURL` comment —
    // no class/interface/declarations of its own (the trait is erased).
    let trait_body: String = trait_ts
        .lines()
        .filter(|l| !l.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        trait_body.trim().is_empty(),
        "Greetable.ts should emit no declarations (trait is erased), got:\n{trait_ts}"
    );
}

#[test]
fn compile_project_with_source_maps_writes_ts_and_map_files() {
    // End-to-end: build a 1-file project with source maps on, then check
    // both .ts and .ts.map landed on disk and the .ts.map is valid v3 JSON
    // with at least one mapping entry that references the .psx source.
    let tmp = std::env::temp_dir().join(format!("psx-sm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("src/Main.psx"),
        "namespace App;\n\nfunction add(int $a, int $b): int {\n    return $a + $b;\n}\n",
    )
    .unwrap();
    let cfg = PsxConfig {
        namespace: "App".into(),
        src: "src".into(),
        out_dir: "dist".into(),
        npm_prefixes: Default::default(),
    };
    compile_project_with(&cfg, &tmp, &CompileOptions { source_maps: true }).unwrap();

    let ts = std::fs::read_to_string(tmp.join("dist/Main.ts")).unwrap();
    let map = std::fs::read_to_string(tmp.join("dist/Main.ts.map")).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        ts.contains("//# sourceMappingURL=Main.ts.map"),
        "TS output should have sourceMappingURL trailer, got:\n{ts}"
    );

    // Minimal V3 parse — `serde_json::Value` keeps this assertion small.
    let map_json: serde_json::Value = serde_json::from_str(&map).unwrap();
    assert_eq!(map_json["version"], 3);
    let sources = map_json["sources"].as_array().expect("sources is array");
    assert_eq!(sources.len(), 1);
    assert!(
        sources[0].as_str().unwrap().ends_with("Main.psx"),
        "expected source path ending in Main.psx, got {:?}",
        sources[0]
    );
    let mappings = map_json["mappings"].as_str().expect("mappings is string");
    assert!(
        !mappings.is_empty(),
        "mappings string should be non-empty for a non-empty source"
    );
}

#[test]
fn source_maps_have_sub_statement_granularity_for_chained_calls() {
    // A single statement `return foo(bar(baz()));` has three call sites.
    // Sub-statement maps mean we expect at least one mapping for the
    // statement plus three more for the nested calls — i.e., the mappings
    // string has more segments than there are top-level statements.
    let tmp = std::env::temp_dir().join(format!("psx-sm-substmt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("src/Main.psx"),
        "function baz(): int { return 1; }\nfunction bar(int $x): int { return $x; }\nfunction foo(int $x): int { return $x; }\nfunction chain(): int { return foo(bar(baz())); }\n",
    )
    .unwrap();
    let cfg = PsxConfig {
        namespace: "App".into(),
        src: "src".into(),
        out_dir: "dist".into(),
        npm_prefixes: Default::default(),
    };
    compile_project_with(&cfg, &tmp, &CompileOptions { source_maps: true }).unwrap();
    let map = std::fs::read_to_string(tmp.join("dist/Main.ts.map")).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let map_json: serde_json::Value = serde_json::from_str(&map).unwrap();
    let mappings = map_json["mappings"].as_str().unwrap();
    // Each `,` separates a segment within a generated line; each `;`
    // separates lines. The chain function alone should contribute 4+
    // segments (1 statement + 3 nested calls). Across the whole file we
    // expect plenty.
    let segment_count = mappings.matches(',').count() + mappings.matches(';').count() + 1;
    assert!(
        segment_count >= 7,
        "expected sub-statement granularity to add segments; got mappings:\n{mappings}"
    );
}

#[test]
fn compile_project_with_no_source_maps_skips_map_files() {
    let tmp = std::env::temp_dir().join(format!("psx-nosm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("src/Main.psx"),
        "namespace App;\n\nfunction add(int $a, int $b): int {\n    return $a + $b;\n}\n",
    )
    .unwrap();
    let cfg = PsxConfig {
        namespace: "App".into(),
        src: "src".into(),
        out_dir: "dist".into(),
        npm_prefixes: Default::default(),
    };
    compile_project_with(&cfg, &tmp, &CompileOptions { source_maps: false }).unwrap();

    let ts = std::fs::read_to_string(tmp.join("dist/Main.ts")).unwrap();
    let map_exists = tmp.join("dist/Main.ts.map").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !ts.contains("sourceMappingURL"),
        "no-source-maps mode should suppress the sourceMappingURL comment"
    );
    assert!(
        !map_exists,
        "no-source-maps mode should not write a .ts.map file"
    );
}

#[test]
fn use_trait_insteadof_resolves_conflict_without_marker() {
    // Without `insteadof`, Foo + Bar both contributing `greet` would emit
    // a conflict marker. With `Foo::greet insteadof Bar`, Bar's `greet`
    // is dropped silently, leaving a clean class body.
    let ts = compile_str(
        r#"trait Foo {
    public function greet(): string { return "foo"; }
}

trait Bar {
    public function greet(): string { return "bar"; }
    public function shout(): string { return "BAR"; }
}

class C {
    use Foo, Bar {
        Foo::greet insteadof Bar;
    }
}
"#,
    )
    .unwrap();
    assert!(
        !ts.contains("__PSX_NOTE_"),
        "insteadof should suppress the conflict marker, got:\n{ts}"
    );
    insta::assert_snapshot!(ts, @r#"
    class C {
      public greet(): string {
        return "foo";
      }
      public shout(): string {
        return "BAR";
      }
    }
    "#);
}

#[test]
fn use_trait_insteadof_with_multiple_losers() {
    let ts = compile_str(
        r#"trait Foo {
    public function greet(): string { return "foo"; }
}

trait Bar {
    public function greet(): string { return "bar"; }
}

trait Baz {
    public function greet(): string { return "baz"; }
}

class C {
    use Foo, Bar, Baz {
        Foo::greet insteadof Bar, Baz;
    }
}
"#,
    )
    .unwrap();
    assert!(!ts.contains("__PSX_NOTE_"));
    insta::assert_snapshot!(ts, @r#"
    class C {
      public greet(): string {
        return "foo";
      }
    }
    "#);
}

#[test]
fn use_trait_alias_emits_renamed_method() {
    // `Bar::greet as legacyGreet` keeps Bar::greet under its original name
    // (subject to other adaptations) AND emits a renamed copy under the alias.
    let ts = compile_str(
        r#"trait Bar {
    public function greet(): string { return "hi"; }
}

class C {
    use Bar {
        Bar::greet as legacyGreet;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class C {
      public greet(): string {
        return "hi";
      }
      public legacyGreet(): string {
        return "hi";
      }
    }
    "#);
}

#[test]
fn use_trait_alias_with_visibility_override() {
    let ts = compile_str(
        r#"trait Bar {
    public function shout(): string { return "OY"; }
}

class C {
    use Bar {
        Bar::shout as private quietShout;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class C {
      public shout(): string {
        return "OY";
      }
      private quietShout(): string {
        return "OY";
      }
    }
    "#);
}

#[test]
fn use_trait_insteadof_plus_as_combined() {
    // The classic PHP example: pick a winner via insteadof, then expose the
    // loser's version under a different name via `as`.
    let ts = compile_str(
        r#"trait Foo {
    public function greet(): string { return "foo"; }
}

trait Bar {
    public function greet(): string { return "bar"; }
}

class C {
    use Foo, Bar {
        Foo::greet insteadof Bar;
        Bar::greet as legacyGreet;
    }
}
"#,
    )
    .unwrap();
    assert!(!ts.contains("__PSX_NOTE_"));
    insta::assert_snapshot!(ts, @r#"
    class C {
      public greet(): string {
        return "foo";
      }
      public legacyGreet(): string {
        return "bar";
      }
    }
    "#);
}

#[test]
fn transitive_trait_use_inlines_into_class() {
    // Trait A uses trait B. Class C uses A. Members from both A and B
    // should end up directly on C.
    let ts = compile_str(
        r#"trait B {
    public function hello(): string { return "hello"; }
}

trait A {
    use B;
    public function world(): string { return "world"; }
}

class C {
    use A;
}
"#,
    )
    .unwrap();
    assert!(!ts.contains("transitive expansion not yet implemented"));
    insta::assert_snapshot!(ts, @r#"
    class C {
      public hello(): string {
        return "hello";
      }
      public world(): string {
        return "world";
      }
    }
    "#);
}

#[test]
fn trait_cycle_emits_marker_no_panic() {
    // A uses B, B uses A. The flattener must detect the cycle and emit a
    // marker rather than recursing forever.
    let ts = compile_str(
        r#"trait A {
    use B;
    public function a(): string { return "a"; }
}

trait B {
    use A;
    public function b(): string { return "b"; }
}

class C {
    use A;
}
"#,
    )
    .unwrap();
    assert!(
        ts.contains("trait cycle detected"),
        "expected cycle marker in output, got:\n{ts}"
    );
}

#[test]
fn trait_constant_inlines_too() {
    let ts = compile_str(
        r#"trait HasVersion {
    public const string VERSION = "1.0";
}

class Server {
    use HasVersion;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class Server {
      public static readonly VERSION: string = "1.0";
    }
    "#);
}

// ---------- Phase 5: property hooks ----------

#[test]
fn property_hook_short_get_emits_getter() {
    let ts = compile_str(
        r#"class User {
    public string $email {
        get => strtolower($this->email);
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @"
    class User {
      private _email!: string;
      public get email(): string {
        return strtolower(this._email);
      }
    }
    ");
}

#[test]
fn property_hook_short_set_assigns_backing() {
    let ts = compile_str(
        r#"class User {
    public string $email {
        set(string $v) => strtolower($v);
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @"
    class User {
      private _email!: string;
      public set email(v: string) {
        this._email = strtolower(v);
      }
    }
    ");
}

#[test]
fn property_hook_block_get_and_set() {
    let ts = compile_str(
        r#"class User {
    public string $email {
        get { return strtolower($this->email); }
        set(string $v) { $this->email = strtolower($v); }
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @"
    class User {
      private _email!: string;
      public get email(): string {
        return strtolower(this._email);
      }
      public set email(v: string) {
        this._email = strtolower(v);
      }
    }
    ");
}

#[test]
fn property_hook_with_default_value_carries_to_backing_field() {
    let ts = compile_str(
        r#"class Counter {
    public int $count = 0 {
        get => $this->count;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Counter {
      private _count: number = 0;
      public get count(): number {
        return this._count;
      }
    }
    ");
}

#[test]
fn property_hook_default_set_param_name_and_type() {
    // No `(param)` clause -> param name defaults to "value", type defaults
    // to the property's declared type. Short-form `set => <expr>` assigns
    // the expression's value to the backing field — no need to write the
    // assignment yourself.
    let ts = compile_str(
        r#"class C {
    public string $msg {
        set => $value;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @"
    class C {
      private _msg!: string;
      public set msg(value: string) {
        this._msg = value;
      }
    }
    ");
}

#[test]
fn property_hook_with_asym_visibility_setter_uses_set_vis() {
    let ts = compile_str(
        r#"class C {
    public private(set) string $msg {
        get => $this->msg;
        set => $value;
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @"
    class C {
      private _msg!: string;
      public get msg(): string {
        return this._msg;
      }
      private set msg(value: string) {
        this._msg = value;
      }
    }
    ");
}

#[test]
fn unhooked_property_path_unchanged_regression() {
    // The non-hooked property emits exactly as before — no backing field.
    let ts = compile_str(
        r#"class C {
    public string $name = "Ada";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class C {
      public name: string = "Ada";
    }
    "#);
}

// ---------- Phase 5: asymmetric visibility ----------

#[test]
fn asym_public_get_private_set_emits_readonly() {
    let ts = compile_str(
        r#"class User {
    public private(set) string $name;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class User {
      public readonly name: string;
    }
    ");
}

#[test]
fn asym_public_get_protected_set_emits_lossy_note() {
    let ts = compile_str(
        r#"class User {
    public protected(set) string $email;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class User {
      public readonly /* public protected(set) — TS approximates as readonly */ email: string;
    }
    ");
}

#[test]
fn asym_visibility_on_promoted_param() {
    let ts = compile_str(
        r#"class User {
    public function __construct(
        public private(set) string $id,
    ) {}
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class User {
      public constructor(public readonly id: string) {}
    }
    ");
}

#[test]
fn symmetric_visibility_unchanged() {
    // Regression: the existing single-vis path keeps emitting the same TS.
    let ts = compile_str(
        r#"class User {
    public string $name;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class User {
      public name: string;
    }
    ");
}

// ---------- Phase 5: first-class callable ----------

#[test]
fn fcc_function_emits_bare_reference() {
    let ts = compile_str("$f = strlen(...);").unwrap();
    insta::assert_snapshot!(ts, @"let f = strlen;");
}

#[test]
fn fcc_static_method_emits_bare() {
    let ts = compile_str("$f = Foo::bar(...);").unwrap();
    insta::assert_snapshot!(ts, @"let f = Foo.bar;");
}

#[test]
fn fcc_variable_callable_emits_bare() {
    let ts = compile_str("$f = $cb(...);").unwrap();
    insta::assert_snapshot!(ts, @"let f = cb;");
}

#[test]
fn fcc_instance_method_pure_target_uses_bare_bind() {
    let ts = compile_str("$f = $obj->method(...);").unwrap();
    insta::assert_snapshot!(ts, @"let f = obj.method.bind(obj);");
}

#[test]
fn fcc_instance_method_complex_target_uses_iife() {
    let ts = compile_str("$f = (new Foo())->doThing(...);").unwrap();
    insta::assert_snapshot!(ts, @"let f = ((__o) => __o.doThing.bind(__o))(new Foo());");
}

// ---------- Phase 5: spaceship `<=>` ----------

#[test]
fn spaceship_operator_emits_iife() {
    let ts = compile_str("$cmp = $a <=> $b;").unwrap();
    insta::assert_snapshot!(
        ts,
        @"let cmp = ((__l, __r) => __l < __r ? -1 : __l > __r ? 1 : 0)(a, b);"
    );
}

#[test]
fn spaceship_with_side_effecting_operands_evaluates_once() {
    let ts = compile_str("$cmp = foo() <=> bar();").unwrap();
    insta::assert_snapshot!(
        ts,
        @"let cmp = ((__l, __r) => __l < __r ? -1 : __l > __r ? 1 : 0)(foo(), bar());"
    );
}

#[test]
fn spaceship_in_compound_expression() {
    // `<=>` is at PHP precedence level 6 — same as ===/!==. The IIFE keeps
    // its operands tight so the surrounding `+ 1` reads cleanly.
    let ts = compile_str("$x = ($a <=> $b) + 1;").unwrap();
    insta::assert_snapshot!(
        ts,
        @"let x = ((__l, __r) => __l < __r ? -1 : __l > __r ? 1 : 0)(a, b) + 1;"
    );
}

// ---------- Phase 5: async / await ----------

#[test]
fn async_function_auto_wraps_return_type() {
    let ts = compile_str(
        r#"async function fetchUser(string $id): User {
    return new User($id, "x@y");
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    async function fetchUser(id: string): Promise<User> {
      return new User(id, "x@y");
    }
    "#);
}

#[test]
fn async_function_explicit_promise_not_double_wrapped() {
    let ts = compile_str(
        r#"async function fetchAll(): Promise<array<User>> {
    return [];
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    async function fetchAll(): Promise<User[]> {
      return [];
    }
    ");
}

#[test]
fn async_function_no_return_type_implies_promise_void() {
    let ts = compile_str(
        r#"async function tick() {
    $i = 0;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    async function tick(): Promise<void> {
      let i = 0;
    }
    ");
}

#[test]
fn await_expression_round_trip() {
    let ts = compile_str(
        r#"async function go(): string {
    $resp = await fetch("/api");
    return await $resp->text();
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    async function go(): Promise<string> {
      let resp = await fetch("/api");
      return await resp.text();
    }
    "#);
}

#[test]
fn async_method_round_trip() {
    let ts = compile_str(
        r#"class Api {
    public async function get(string $url): string {
        $resp = await fetch($url);
        return await $resp->text();
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Api {
      public async get(url: string): Promise<string> {
        let resp = await fetch(url);
        return await resp.text();
      }
    }
    ");
}

// ---------- Phase 5: string interpolation ----------

#[test]
fn simple_dollar_interpolation() {
    let ts = compile_str(
        r#"$name = "Ada";
$msg = "Hello, $name";
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let name = "Ada";
    let msg = `Hello, ${name}`;
    "#);
}

#[test]
fn brace_dollar_member_interpolation() {
    let ts = compile_str(r#"$msg = "user: {$user->email}";"#).unwrap();
    insta::assert_snapshot!(ts, @r"let msg = `user: ${user.email}`;");
}

#[test]
fn brace_dollar_index_interpolation() {
    let ts = compile_str(r#"$first = "first: {$arr[0]}";"#).unwrap();
    insta::assert_snapshot!(ts, @r"let first = `first: ${arr[0]}`;");
}

#[test]
fn multiple_interpolations_in_one_string() {
    let ts = compile_str(r#"$line = "$first $last (age $age)";"#).unwrap();
    insta::assert_snapshot!(ts, @r"let line = `${first} ${last} (age ${age})`;");
}

#[test]
fn escaped_dollar_does_not_interpolate_in_e2e() {
    let ts = compile_str(r#"$price = "Price: \$5.00";"#).unwrap();
    insta::assert_snapshot!(ts, @r#"let price = "Price: $5.00";"#);
}

#[test]
fn single_quoted_strings_never_interpolate_e2e() {
    let ts = compile_str(r#"$msg = 'Hello, $name';"#).unwrap();
    insta::assert_snapshot!(ts, @r#"let msg = "Hello, $name";"#);
}

#[test]
fn template_literal_text_escapes_backticks() {
    // A user's literal text containing a backtick must be escaped in the
    // template literal output so it doesn't terminate the literal.
    let ts = compile_str(r#"$msg = "code: `$name`";"#).unwrap();
    insta::assert_snapshot!(ts, @r"let msg = `code: \`${name}\``;");
}

// ---------- Phase 4: namespaces + use ----------

#[test]
fn namespaced_class_emits_export() {
    let ts = compile_str(
        r#"namespace App\Models;

class User {
    public function __construct(public string $name) {}
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    export class User {
      public constructor(public name: string) {}
    }
    ");
}

#[test]
fn use_emits_import_at_top() {
    let ts = compile_str(
        r#"namespace App;

use App\Models\User;

class Service {
    public function makeUser(string $name): User {
        return new User($name);
    }
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    import { User } from "App/Models";
    export class Service {
      public makeUser(name: string): User {
        return new User(name);
      }
    }
    "#);
}

#[test]
fn aliased_use_emits_aliased_import() {
    let ts = compile_str(
        r#"namespace App;

use App\Models\Profile as P;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"import { Profile as P } from "App/Models";"#);
}

#[test]
fn grouped_use_collapses_to_single_import() {
    let ts = compile_str(
        r#"namespace App;

use App\Models\{User, Profile, Address as Addr};
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"import { User, Profile, Address as Addr } from "App/Models";"#);
}

#[test]
fn use_function_and_const_round_trip() {
    let ts = compile_str(
        r#"namespace App;

use function App\Util\format;
use const App\Constants\PI;
"#,
    )
    .unwrap();
    // Both function and const uses emit identical TS imports — JS doesn't
    // differentiate.
    insta::assert_snapshot!(ts, @r#"
    import { format } from "App/Util";
    import { PI } from "App/Constants";
    "#);
}

#[test]
fn unnamespaced_file_does_not_emit_export() {
    // Existing single-file scripts (no `namespace`) still emit identical output.
    let ts = compile_str(
        r#"class User {
    public function __construct(public string $name) {}
}

$ada = new User("Ada");
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class User {
      public constructor(public name: string) {}
    }
    let ada = new User("Ada");
    "#);
}

#[test]
fn imports_hoist_above_other_statements_regardless_of_source_order() {
    let ts = compile_str(
        r#"namespace App;

class Foo {}

use App\Bar\Baz;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    import { Baz } from "App/Bar";
    export class Foo {}
    "#);
}

#[test]
fn instanceof_round_trip() {
    let ts = compile_str(
        r#"$is_user = $obj instanceof User;
if ($e instanceof InvalidArgumentException) {
    $msg = "bad arg";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let msg;
    let is_user = obj instanceof User;
    if (e instanceof InvalidArgumentException) {
      msg = "bad arg";
    }
    "#);
}

#[test]
fn match_with_default_round_trip() {
    let ts = compile_str(
        r#"$result = match ($code) {
    200, 201 => "ok",
    404 => "not found",
    default => "unknown",
};
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"let result = ((__m) => { if (__m === 200 || __m === 201) return "ok"; if (__m === 404) return "not found"; return "unknown"; })(code);"#);
}

#[test]
fn match_without_default_throws_on_miss() {
    let ts = compile_str(
        r#"$result = match ($x) {
    1 => "one",
    2 => "two",
};
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"let result = ((__m) => { if (__m === 1) return "one"; if (__m === 2) return "two"; throw new Error("Unhandled match value: " + String(__m)); })(x);"#);
}

#[test]
fn ternary_round_trip() {
    let ts = compile_str(
        r#"$msg = $age >= 18 ? "adult" : "minor";
$display = $name ?: "anonymous";
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    let msg = age >= 18 ? "adult" : "minor";
    let display = name || "anonymous";
    "#);
}

#[test]
fn nested_ternary_parens() {
    let ts = compile_str(r#"$x = $a ? ($b ? 1 : 2) : 3;"#).unwrap();
    insta::assert_snapshot!(ts, @"let x = a ? (b ? 1 : 2) : 3;");
}

#[test]
fn backed_string_enum_round_trip() {
    let ts = compile_str(
        r#"enum Role: string {
    case Admin = "admin";
    case Member = "member";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    enum Role {
      Admin = "admin",
      Member = "member"
    }
    "#);
}

#[test]
fn backed_int_enum_round_trip() {
    let ts = compile_str(
        r#"enum Priority: int {
    case Low = 1;
    case High = 10;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    enum Priority {
      Low = 1,
      High = 10
    }
    ");
}

#[test]
fn pure_enum_round_trip() {
    let ts = compile_str(
        r#"enum Status {
    case Open;
    case Closed;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    enum Status {
      Open,
      Closed
    }
    ");
}

#[test]
fn enum_case_access_round_trip() {
    let ts = compile_str(
        r#"enum Role: string {
    case Admin = "admin";
}

$r = Role::Admin;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    enum Role {
      Admin = "admin"
    }
    let r = Role.Admin;
    "#);
}

#[test]
fn pure_enum_with_value_errors() {
    let err = compile_str("enum X { case A = 1; }").unwrap_err();
    assert!(err.to_string().contains("pure enum"));
}

#[test]
fn backed_enum_without_value_errors() {
    let err = compile_str("enum X: string { case A; }").unwrap_err();
    assert!(err.to_string().contains("backed enum"));
}

#[test]
fn class_constants_round_trip() {
    let ts = compile_str(
        r#"class Config {
    public const string VERSION = "1.0";
    public const int MAX = 100;
    private const string SECRET = "shh";
    public final const string KIND = "config";
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class Config {
      public static readonly VERSION: string = "1.0";
      public static readonly MAX: number = 100;
      private static readonly SECRET: string = "shh";
      public static readonly KIND: string = "config";
    }
    "#);
}

#[test]
fn class_constant_without_type_round_trip() {
    let ts = compile_str(
        r#"class C {
    public const PI = 3.14;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @"
    class C {
      public static readonly PI = 3.14;
    }
    ");
}

#[test]
fn class_constant_access_round_trip() {
    // Read-side: `Foo::CONST` was already covered by member-access tests.
    // This locks the shape together with declarations.
    let ts = compile_str(
        r#"class Config {
    public const int MAX = 100;
}

$cap = Config::MAX;
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Config {
      public static readonly MAX: number = 100;
    }
    let cap = Config.MAX;
    ");
}

#[test]
fn implements_clause_emits_ts_implements() {
    let ts = compile_str(
        r#"class Bus implements Vehicle, Trackable {
    public string $model;
}
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r"
    class Bus implements Vehicle, Trackable {
      public model: string;
    }
    ");
}

#[test]
fn class_with_promoted_constructor_round_trip() {
    let ts = compile_str(
        r#"class User {
    public function __construct(
        public readonly string $id,
        public string $email,
        private ?int $age = null,
    ) {}

    public function info(): string {
        return $this->id . " <" . $this->email . ">";
    }
}

$ada = new User("u1", "ada@example.com", 36);
$line = $ada->info();
"#,
    )
    .unwrap();
    insta::assert_snapshot!(ts, @r#"
    class User {
      public constructor(public readonly id: string, public email: string, private age: number | null = null) {}
      public info(): string {
        return this.id + " <" + this.email + ">";
      }
    }
    let ada = new User("u1", "ada@example.com", 36);
    let line = ada.info();
    "#);
}
