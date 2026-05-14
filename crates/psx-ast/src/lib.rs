//! PHPScript AST.
//!
//! Grows phase by phase. Phase 1 starts with expression statements and integer
//! literals, just enough to take a `.psx` source file through the full pipeline
//! end-to-end.

/// Byte range into the source file an AST node was parsed from. Mirrors
/// `psx_lexer::Span` but lives in this crate so consumers of the AST can
/// read source positions without taking a dependency on the lexer.
#[derive(Debug, Clone, Copy, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const DUMMY: Span = Span { start: 0, end: 0 };
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

/// Spans intentionally do NOT participate in `PartialEq` — two AST nodes are
/// "the same" if their structure matches, regardless of where they were
/// parsed from. Lets unit tests assert on the AST shape without baking in
/// source positions.
impl PartialEq for Span {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}
impl Eq for Span {}

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub stmts: Vec<Stmt>,
}

impl Module {
    pub fn empty() -> Self {
        Self { stmts: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `<expr>;`
    Expr(Expr, Span),
    /// `{ stmt* }` — block of zero or more statements.
    Block(Vec<Stmt>, Span),
    /// `if (cond) <then> (else <else_>)?`. `elseif` is parsed as a nested
    /// `if` in the else branch — there's no separate AST node for it.
    If {
        cond: Expr,
        then: Box<Stmt>,
        else_: Option<Box<Stmt>>,
        span: Span,
    },
    /// `return;` (None) or `return <expr>;` (Some).
    Return(Option<Expr>, Span),
    /// `while (cond) <body>`.
    While {
        cond: Expr,
        body: Box<Stmt>,
        span: Span,
    },
    /// `do <body> while (cond);`. The body runs at least once.
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
        span: Span,
    },
    /// `for (init; cond; step) <body>`. Any of init / cond / step may be
    /// absent — `for (;;)` is the infinite loop. MVP supports a single
    /// expression per slot (PHP allows comma-separated lists; defer that).
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Expr>,
        body: Box<Stmt>,
        span: Span,
    },
    /// `foreach (iter as $value) <body>` or
    /// `foreach (iter as $key => $value) <body>`. Names are stored without
    /// the `$` sigil.
    Foreach {
        iter: Expr,
        key: Option<String>,
        value: String,
        body: Box<Stmt>,
        span: Span,
    },
    /// `function name(typed params): RetType { body }`.
    Function(FunctionDecl),
    /// `namespace App\Foo;` — file-level namespace declaration. Stored as
    /// path segments without leading backslash.
    Namespace(Vec<String>, Span),
    /// `use App\Foo\Bar;` (and the function/const, group, and aliased forms).
    /// Group `use App\Foo\{A, B as B2}` is flattened to multiple `UseItem`s
    /// at parse time so the resolver and emitter only ever see flat lists.
    Use(UseStmt),
    /// `throw <expr>;`.
    Throw(Expr, Span),
    /// `break;` or `break <level>;`. The level (PHP 5.0+) is the number of
    /// nested loops/switches to break out of; defaults to 1 when omitted.
    Break(Option<u32>, Span),
    /// `continue;` or `continue <level>;`. Same level semantics as `break`.
    Continue(Option<u32>, Span),
    /// `try { body } catch (...) { ... } [catch ...] [finally { ... }]`.
    Try {
        body: Vec<Stmt>,
        catches: Vec<Catch>,
        finally: Option<Vec<Stmt>>,
        span: Span,
    },
    /// `class Name extends Base implements I1, I2 { members... }`.
    Class(Class),
    /// `interface Name extends I1, I2 { signatures... }`.
    Interface(Interface),
    /// `enum Name { case A; case B; }` (pure) or
    /// `enum Name: BackedType { case A = ...; ... }` (backed).
    Enum(EnumDecl),
    /// `trait Name { ... }`. Traits are erased at compile time — their
    /// members are inlined into each class that `use`s them.
    Trait(TraitDecl),
}

impl Stmt {
    /// The byte-range span this statement occupies in its source file.
    pub fn span(&self) -> Span {
        match self {
            Stmt::Expr(_, s)
            | Stmt::Block(_, s)
            | Stmt::Return(_, s)
            | Stmt::Throw(_, s)
            | Stmt::Break(_, s)
            | Stmt::Continue(_, s)
            | Stmt::Namespace(_, s) => *s,
            Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::DoWhile { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Foreach { span, .. }
            | Stmt::Try { span, .. } => *span,
            Stmt::Function(f) => f.span,
            Stmt::Use(u) => u.span,
            Stmt::Class(c) => c.span,
            Stmt::Interface(i) => i.span,
            Stmt::Enum(e) => e.span,
            Stmt::Trait(t) => t.span,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraitDecl {
    pub name: String,
    pub members: Vec<ClassMember>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub name: String,
    /// `None` for pure enums; `Some(_)` for backed enums (PHP 8.1).
    pub backed_type: Option<TypeAnn>,
    pub implements: Vec<TypeAnn>,
    pub cases: Vec<EnumCase>,
    pub constants: Vec<ClassConstant>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumCase {
    pub name: String,
    /// Required for backed enums, forbidden for pure enums (parser enforces).
    pub value: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub name: String,
    pub extends: Vec<TypeAnn>,
    pub members: Vec<InterfaceMember>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterfaceMember {
    /// Method signature (body must be `None`).
    Method(Method),
    Constant(ClassConstant),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    pub name: String,
    /// `abstract class Foo` — populated by slice 7.
    pub abstract_: bool,
    /// `final class Foo` — populated by slice 7. Dropped silently on emit
    /// (TS has no `final`).
    pub final_: bool,
    /// `readonly class Foo` (PHP 8.2) — slice 5. When true, every property
    /// is treated as readonly at emit time.
    pub readonly: bool,
    /// `extends Base` — slice 6.
    pub extends: Option<TypeAnn>,
    /// `implements I1, I2` — slice 6.
    pub implements: Vec<TypeAnn>,
    pub members: Vec<ClassMember>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClassMember {
    Property(Property),
    Method(Method),
    Constant(ClassConstant),
    /// `use TraitA, TraitB;` (or `use TraitA, TraitB { adaptations }`)
    /// inside a class body. The emitter resolves each `TypeAnn` to a
    /// `TraitDecl` (via the trait map) and inlines the trait's members
    /// into the using class, applying any `insteadof` / `as` adaptations
    /// listed in the block.
    UseTrait(UseTraitBlock),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseTraitBlock {
    pub traits: Vec<TypeAnn>,
    pub adaptations: Vec<TraitAdaptation>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TraitAdaptation {
    /// `Foo::greet insteadof Bar[, Baz...];` — when expanding the trait
    /// list, the loser traits' member with this name is dropped. The
    /// winner trait's member is kept (and may itself collide silently
    /// with a class-defined member, in which case the class still wins).
    InsteadOf {
        winner_trait: String,
        method: String,
        losers: Vec<String>,
    },
    /// `Bar::greet as [private|public|protected] legacyGreet;` — after
    /// the standard expansion, emit a renamed copy of the source trait's
    /// member with an optional visibility override.
    Alias {
        source_trait: String,
        source_method: String,
        new_name: String,
        new_visibility: Option<Visibility>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClassConstant {
    pub visibility: Visibility,
    /// PHP 8.1+: `final const`.
    pub final_: bool,
    /// PHP 8.3+: typed class constants.
    pub ty: Option<TypeAnn>,
    pub name: String,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub visibility: Visibility,
    /// PHP 8.4 asymmetric visibility: when `Some`, this is the write-side
    /// visibility, distinct from `visibility` (the read-side). When `None`,
    /// the property is symmetrically visible.
    pub set_visibility: Option<Visibility>,
    pub readonly: bool,
    pub static_: bool,
    pub ty: Option<TypeAnn>,
    pub name: String,
    pub default: Option<Expr>,
    /// PHP 8.4 property hooks. When `Some`, the property lowers to a
    /// backing field plus TS getter/setter rather than a plain field.
    pub hooks: Option<PropertyHooks>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PropertyHooks {
    pub get: Option<HookBody>,
    pub set: Option<SetHook>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookBody {
    /// `get => <expr>;` / `set => <expr>;`. For `get`, the expression's
    /// value is returned. For `set`, it's assigned to the backing field.
    Expr(Expr),
    /// `get { <stmts> }` / `set { <stmts> }`.
    Block(Vec<Stmt>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetHook {
    /// Defaults to `"value"` when the source omits the `(param)` clause.
    pub param_name: String,
    /// Defaults to the property's declared type when the source omits it.
    pub param_type: Option<TypeAnn>,
    pub body: HookBody,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Method {
    pub visibility: Visibility,
    pub static_: bool,
    pub abstract_: bool,
    pub final_: bool,
    /// `async` modifier (PHPScript extension).
    pub async_: bool,
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeAnn>,
    /// `None` means abstract or interface declaration; `Some([])` is an
    /// explicit empty body.
    pub body: Option<Vec<Stmt>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseStmt {
    pub kind: UseKind,
    pub items: Vec<UseItem>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseKind {
    /// Default form — `use App\Foo\Bar;` (covers classes, interfaces, enums,
    /// traits — anything in PHP class-namespace).
    Class,
    /// `use function App\Util\format;`.
    Function,
    /// `use const App\Constants\PI;`.
    Const,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UseItem {
    /// Path segments of the imported symbol, without leading backslash.
    pub path: Vec<String>,
    /// Optional `as <alias>`.
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Catch {
    /// One or more exception types — PHP 8 supports `catch (E1|E2 $e)`.
    pub types: Vec<TypeAnn>,
    /// Optional binding (PHP 8 allows nameless catches).
    pub var: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeAnn>,
    pub body: Vec<Stmt>,
    /// `async` modifier (PHPScript extension — PHP doesn't have async).
    pub async_: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeAnn>,
    pub default: Option<Expr>,
    /// `Some(_)` => constructor property promotion (PHP 8.0). The parameter
    /// is *also* a class property with the recorded modifiers.
    pub promotion: Option<Promotion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Promotion {
    pub visibility: Visibility,
    /// Asymmetric write-side visibility (PHP 8.4). See `Property.set_visibility`.
    pub set_visibility: Option<Visibility>,
    pub readonly: bool,
}

/// Type annotation tree. Maps to TypeScript at emit time per the design
/// doc's mapping table.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeAnn {
    /// Named type — `int`, `string`, `MyClass`, etc.
    Named(String),
    /// `?Foo` — short nullable form. Equivalent to `Foo|null`.
    Nullable(Box<TypeAnn>),
    /// `Foo|Bar|...` — union of types.
    Union(Vec<TypeAnn>),
    /// `array<T>` or `array<K, V>`. Generic in name; the emitter has
    /// special-cased mappings for `array<>`.
    Generic(String, Vec<TypeAnn>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Decimal integer literal.
    Int(i64),
    /// Decimal float literal.
    Float(f64),
    /// String literal (escapes already resolved).
    Str(String),
    /// Double-quoted string with interpolation. Each part is either a
    /// literal text run or a sub-expression (`$name` or `{$expr}`). The
    /// emitter emits this as a TS template literal.
    InterpolatedStr(Vec<InterpolatedPart>),
    /// Boolean literal — `true` / `false`.
    Bool(bool),
    /// `null`.
    Null,
    /// Variable reference. The name is stored without the `$` sigil.
    Var(String),
    /// Bare identifier — function name, class name, constant, enum case
    /// reference, etc. The parser produces this for any unqualified
    /// non-keyword name; the emitter emits it verbatim.
    Ident(String),
    /// Function call (or any callable invocation). The callee is an
    /// expression so future additions like `$f()` (variable callee) and
    /// `Foo::bar()` (static method) slot in without changing the call shape.
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        /// Byte span of the whole call expression (including parens). Used
        /// by the emitter to record sub-statement source-map entries so
        /// stack traces resolve to the specific call in a chained
        /// `a(b(c(...)))` line.
        span: Span,
    },
    /// Array / object literal. Items may be keyed (`"k" => v`) or
    /// unkeyed (`v`); the emitter picks list vs object form based on
    /// uniformity.
    Array(Vec<ArrayItem>),
    /// `obj[key]` — bracket access, the same form for arrays and objects.
    Index { obj: Box<Expr>, key: Box<Expr> },
    /// Member or scope-resolution access. Covers all three of `$obj->name`,
    /// `$obj?->name`, and `Foo::name`. Right-side `$` (PHP static property
    /// syntax `Foo::$prop`) is stripped at parse time so the name field is
    /// always sigil-free.
    Access {
        target: Box<Expr>,
        name: String,
        op: AccessOp,
    },
    /// `new ClassName(args)` — instantiation. The `class` is parsed as a
    /// type annotation so `new Foo<int>()` (generic constructor) drops in
    /// without changing the AST.
    New { class: TypeAnn, args: Vec<Expr> },
    /// `self` — refers to the lexically enclosing class. Always paired with
    /// `::` in source. The emitter resolves it to the current class name.
    SelfRef,
    /// `parent` — refers to the parent class. Emits as TS `super`.
    ParentRef,
    /// `static` — late static binding. For MVP we emit it the same as
    /// `self::` (i.e. the lexically enclosing class). True LSB is deferred.
    StaticRef,
    /// PHP 8 `match` expression. Each arm has zero or more conditions
    /// (`None` => default arm) and a body expression. Strict equality.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// `cond ? then : else_` — full ternary.
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    /// `cond ?: else_` — short ternary (PHP-flavoured Elvis). Returns
    /// `cond` when truthy, otherwise `else_`. Emits as JS `||`.
    ShortTernary { cond: Box<Expr>, else_: Box<Expr> },
    /// Assignment expression. PHP-style: assignment is an *expression* and
    /// can appear inside other expressions. The emitter has special-case
    /// handling when an assignment is the top-level expression of an
    /// `ExprStmt`, lifting it to a `let`-declaration on first sight.
    Assign { target: Box<Expr>, value: Box<Expr> },
    /// Compound assignment: `+=`, `-=`, `.=`, `??=`, etc. Always assumes the
    /// target is already declared — does NOT lift to `let`.
    CompoundAssign {
        op: BinOp,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// Binary expression. Precedence/associativity is encoded by tree shape;
    /// the emitter parenthesises operands as needed.
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Prefix unary expression.
    Unary { op: UnOp, expr: Box<Expr> },
    /// `await <expr>` — only meaningful inside an `async` function body.
    /// Emit-side does not enforce that; tsc will.
    Await(Box<Expr>),
    /// PHP 8.1 first-class callable syntax: `target(...)` produces a
    /// Closure value rather than calling `target`. Emitted as a JS
    /// function reference (with `.bind` for instance methods).
    FirstClassCallable(Box<Expr>),
    /// PHP 7.4 arrow function: `fn(params): RetType => expr`. The body is
    /// a single expression; multi-statement bodies use the full `function`
    /// keyword form which lives at statement level (`Stmt::Function`).
    ArrowFn {
        params: Vec<Param>,
        return_type: Option<TypeAnn>,
        body: Box<Expr>,
    },
    /// `++$x` / `$x++` / `--$x` / `$x--`.
    IncDec {
        op: IncDecOp,
        fix: IncDecFix,
        target: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncDecOp {
    Inc, // ++
    Dec, // --
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncDecFix {
    Prefix,  // ++$x
    Postfix, // $x++
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    /// `None` for the default arm. Otherwise the values to test against
    /// the scrutinee with `===`.
    pub conds: Option<Vec<Expr>>,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArrayItem {
    pub key: Option<Expr>,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpolatedPart {
    Lit(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessOp {
    /// `->` — instance member.
    Arrow,
    /// `?->` — null-safe instance member.
    NullSafeArrow,
    /// `::` — static / scope-resolution.
    DoubleColon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // Arithmetic.
    Add, // +
    Sub, // -
    Mul, // *
    Div, // /
    Rem, // %
    Pow, // ** (right-associative)
    // String.
    Concat, // . — emits as `+` in TS
    // Comparison (always strict — PHPScript rejects `==` / `!=`).
    Eq,    // === in source, === in emit
    NotEq, // !== in source, !== in emit
    Lt,    // <
    Gt,    // >
    LtEq,  // <=
    GtEq,  // >=
    // Logical.
    And,      // &&
    Or,       // ||
    Coalesce, // ?? (right-associative)
    /// `$obj instanceof ClassName`. Highest binary precedence.
    Instanceof,
    /// `<=>` — three-way compare. Returns -1, 0, or 1. Emitted as a
    /// single-eval IIFE; no runtime helper.
    Spaceship,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg, // -x
    Pos, // +x  (no-op, kept for AST faithfulness)
}
