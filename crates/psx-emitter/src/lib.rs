//! PHPScript -> TypeScript source emitter.
//!
//! Walks a `psx-ast::Module` and produces TypeScript source. Each phase grows
//! coverage of the AST.

use std::collections::HashSet;
use std::fmt::Write as _;

use std::collections::BTreeMap;

use psx_ast::{
    AccessOp, ArrayItem, BinOp, Catch, Class, ClassConstant, ClassMember, EnumDecl, Expr,
    FunctionDecl, HookBody, IncDecFix, IncDecOp, Interface, InterfaceMember, InterpolatedPart,
    MatchArm, Method, Module, Param, Property, SetHook, Stmt, TraitAdaptation, TraitDecl, TypeAnn,
    UnOp, UseStmt, Visibility,
};
use psx_sourcemap::{LineCol, LineMap, SourceMapBuilder};

/// Map of trait FQN (or local name in single-file mode) to its declaration,
/// used at emit time to expand `use TraitX;` inside class bodies.
pub type TraitMap<'a> = BTreeMap<String, &'a TraitDecl>;

#[cfg(test)]
use psx_ast::Promotion;

/// Mutable emitter context — module-wide scope tracking + indent depth for
/// nested blocks.
///
/// Variables that are assigned anywhere below module level (inside a block,
/// `if` branch, etc.) get hoisted to a single `let n1, n2;` line at the top.
/// This gives PHP-style "function-scoped" semantics on top of JS — a
/// variable introduced in one `if` branch is visible to siblings and to
/// statements after the block.
struct Ctx<'a> {
    declared: HashSet<String>,
    indent: usize,
    /// Name of the class whose body we're currently emitting. Used to
    /// resolve `self::method()` and `static::method()` to
    /// `<CurrentClass>.method()` in TS.
    current_class: Option<String>,
    /// `Some(_)` when emitting a namespaced (module-shape) file. Top-level
    /// declarations inside the module gain an `export` prefix; nested scopes
    /// (function/method bodies) clear this so inner declarations don't get
    /// exported.
    module_namespace: Option<String>,
    /// `Some(<prop>)` while emitting a property hook body. Inside the
    /// hook, every `$this-><prop>` access rewrites to `this._<prop>` so
    /// the body talks to the backing field instead of recursing through
    /// the getter.
    current_hook_property: Option<String>,
    /// Trait map for inline-expansion of `use TraitX;` inside class bodies.
    /// Empty in unit-test contexts; populated by `emit()` from the same
    /// module's `Stmt::Trait`s, and by the CLI from a project-wide pass.
    traits: TraitMap<'a>,
    /// `Some(_)` when the caller asked for a source map. Each statement
    /// emitted by `emit_scope_contents` records a `(ts_line, source_line)`
    /// mapping into this builder. `None` keeps the emitter cost-free for
    /// callers that don't need source maps (unit tests, single-file
    /// `compile_str`).
    source_map: Option<SmContext>,
}

/// Per-emit source-map state: the builder we're populating, the source
/// file's line/column index (for converting byte offsets to line:col), and
/// the index assigned to the source file in `SourceMapBuilder::sources`.
struct SmContext {
    builder: SourceMapBuilder,
    line_map: LineMap,
    source_idx: u32,
}

impl<'a> Default for Ctx<'a> {
    fn default() -> Self {
        Self {
            declared: HashSet::new(),
            indent: 0,
            current_class: None,
            module_namespace: None,
            current_hook_property: None,
            traits: BTreeMap::new(),
            source_map: None,
        }
    }
}

const INDENT_UNIT: &str = "  ";

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str(INDENT_UNIT);
    }
}

/// Resolver callback used by `emit_with_resolver`. Given a `use`-item's
/// path segments and optional alias, returns:
/// - `path`: the literal TS import path (e.g. `"@types/node"`
///   or `"./Models/User"`).
/// - `name`: the imported symbol (e.g. `"Request"` or `"User"`).
/// - `alias`: passed through unchanged.
pub type UseResolver<'a> = dyn Fn(&[String], Option<&str>) -> (String, String, Option<String>) + 'a;

pub fn emit(module: &Module) -> String {
    emit_with(module, None, &BTreeMap::new(), None).ts
}

/// Like `emit`, but `use` statements are routed through `resolver` so the
/// emitted `import` lines reflect the project's PSR-4 / npm-prefix layout.
pub fn emit_with_resolver(module: &Module, resolver: &UseResolver<'_>) -> String {
    emit_with(module, Some(resolver), &BTreeMap::new(), None).ts
}

/// Like `emit_with_resolver`, but additionally accepts a project-wide trait
/// map so cross-file `use TraitX;` inside a class body inlines correctly.
pub fn emit_with_resolver_and_traits(
    module: &Module,
    resolver: &UseResolver<'_>,
    traits: &TraitMap<'_>,
) -> String {
    emit_with(module, Some(resolver), traits, None).ts
}

/// Input to `emit_with_source_map` describing the source file whose AST is
/// being emitted. The fields end up as the `sources` / `sourcesContent` /
/// `file` entries in the produced v3 source map.
pub struct SourceMapInput<'a> {
    /// Path to write in `sources`. Conventionally relative to the .ts.map
    /// file (e.g. `"../src/Main.psx"`).
    pub source_path: &'a str,
    /// Full source text. Inlined as `sourcesContent` so downstream tools
    /// can resolve `.psx` content without a separate fetch.
    pub source_text: &'a str,
    /// Path to write in `file` (the generated .ts filename).
    pub generated_file: &'a str,
}

/// Result of an emit that produced both TS source and a paired source map.
/// `ts` already contains the trailing `//# sourceMappingURL=...` comment so
/// callers just write the two strings side by side to disk.
pub struct EmitWithMap {
    pub ts: String,
    pub source_map_json: String,
    /// The `.map` filename embedded in the `sourceMappingURL` comment —
    /// e.g. `"Main.ts.map"`. Callers should write `source_map_json` to a
    /// file with this name next to the `.ts`.
    pub source_map_filename: String,
}

/// Emit `module` and produce a v3 source map alongside it. Each top-level
/// statement (and every statement inside a function/method/hook body) gets
/// a `(generated_line, 0) -> (source_line, source_col)` mapping. That's
/// granular enough for `node --enable-source-maps` to give a useful stack
/// trace that points back at the `.psx` file.
pub fn emit_with_source_map(
    module: &Module,
    resolver: &UseResolver<'_>,
    traits: &TraitMap<'_>,
    input: SourceMapInput<'_>,
) -> EmitWithMap {
    let mut builder = SourceMapBuilder::new();
    builder.file = Some(input.generated_file.to_string());
    let source_idx = builder.add_source(input.source_path, Some(input.source_text.to_string()));
    let sm = SmContext {
        builder,
        line_map: LineMap::new(input.source_text),
        source_idx,
    };
    let result = emit_with(module, Some(resolver), traits, Some(sm));
    let source_map_filename = format!("{}.map", input.generated_file);
    let trailer = format!("//# sourceMappingURL={source_map_filename}\n");
    let mut ts = result.ts;
    if !ts.ends_with('\n') {
        ts.push('\n');
    }
    ts.push_str(&trailer);
    let source_map_json = result
        .source_map
        .expect("emit_with returns Some(builder) when input is Some(_)")
        .to_json();
    EmitWithMap {
        ts,
        source_map_json,
        source_map_filename,
    }
}

/// Internal result of `emit_with`. `source_map` is `Some(_)` exactly when
/// the caller passed `Some(_)` for `sm` — guaranteeing the public API
/// `emit_with_source_map` always gets a builder back.
struct EmitInternal {
    ts: String,
    source_map: Option<SourceMapBuilder>,
}

fn emit_with(
    module: &Module,
    resolver: Option<&UseResolver<'_>>,
    extra_traits: &TraitMap<'_>,
    sm: Option<SmContext>,
) -> EmitInternal {
    // Collect any same-file trait declarations on top of the project map.
    // Same-file lookups win when the trait name collides — that matches the
    // PSR-4 lookup order (a local `use TraitX` resolves to the file
    // imported via the namespace, falling back to a same-file declaration
    // only when no `use` statement points elsewhere).
    let mut traits: TraitMap<'_> = extra_traits.clone();
    for stmt in &module.stmts {
        if let Stmt::Trait(t) = stmt {
            traits.entry(t.name.clone()).or_insert(t);
        }
    }

    let mut ctx = Ctx {
        declared: HashSet::new(),
        indent: 0,
        current_class: None,
        module_namespace: None,
        current_hook_property: None,
        traits,
        source_map: sm,
    };
    let mut out = String::new();

    // Pre-pass: find the file's namespace (if any) and collect resolved
    // imports for every `use` statement. The namespace controls whether
    // top-level decls get `export`; the imports are hoisted to the very
    // top of the output regardless of source order.
    for stmt in &module.stmts {
        if let Stmt::Namespace(path, _) = stmt {
            ctx.module_namespace = Some(path.join("\\"));
        }
    }
    for stmt in &module.stmts {
        if let Stmt::Use(u) = stmt {
            emit_use_as_imports(u, resolver, &ctx.traits, &mut out);
        }
    }

    emit_scope_contents(&module.stmts, &[], &mut ctx, &mut out);
    EmitInternal {
        ts: out,
        source_map: ctx.source_map.map(|s| s.builder),
    }
}

/// Emit a `UseStmt` as one or more TS `import { ... } from "..."` lines.
///
/// Without a resolver, the import path is the verbatim segments joined by
/// `/` and the imported name is the last segment — useful for unit tests
/// and single-file mode where there's no project context.
///
/// With a resolver, each item is routed through it.
fn emit_use_as_imports(
    u: &UseStmt,
    resolver: Option<&UseResolver<'_>>,
    traits: &TraitMap<'_>,
    out: &mut String,
) {
    // Resolve each item to (path, name, alias). Group by path so identical
    // paths collapse into a single import.
    let mut groups: Vec<(String, Vec<(String, Option<String>)>)> = Vec::new();
    for item in &u.items {
        if item.path.is_empty() {
            continue;
        }
        // Traits are erased at compile time — their members are inlined into
        // using classes by `expand_class_members`. A namespace `use ...;`
        // pointing at a trait must NOT emit a TS import, since the target
        // file emits nothing of substance.
        if let Some(last) = item.path.last() {
            if traits.contains_key(last) {
                continue;
            }
        }
        let (path, name, alias) = match resolver {
            Some(r) => r(&item.path, item.alias.as_deref()),
            None => (
                item.path[..item.path.len() - 1].join("/"),
                item.path.last().cloned().unwrap_or_default(),
                item.alias.clone(),
            ),
        };
        match groups.iter_mut().find(|(p, _)| *p == path) {
            Some((_, items)) => items.push((name, alias)),
            None => groups.push((path, vec![(name, alias)])),
        }
    }
    for (path, items) in groups {
        out.push_str("import { ");
        for (i, (name, alias)) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(name);
            if let Some(a) = alias {
                out.push_str(" as ");
                out.push_str(a);
            }
        }
        out.push_str(" } from \"");
        out.push_str(&path);
        out.push_str("\";\n");
    }
}

/// Emit a flat scope (module body OR function body): runs the hoist pre-pass,
/// emits the optional hoist line, then emits each statement with leading
/// indent and trailing newline. `params` are pre-declared at the start of
/// the scope so they don't get re-declared by the lift logic.
fn emit_scope_contents(stmts: &[Stmt], params: &[Param], ctx: &mut Ctx, out: &mut String) {
    let mut to_hoist: HashSet<String> = HashSet::new();
    let mut seen_at_top: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();

    for stmt in stmts {
        match stmt {
            Stmt::Expr(Expr::Assign { target, .. }, _)
            | Stmt::Expr(Expr::CompoundAssign { target, .. }, _) => {
                if let Expr::Var(name) = target.as_ref() {
                    seen_at_top.insert(name.clone());
                }
            }
            Stmt::Expr(_, _)
            | Stmt::Return(_, _)
            | Stmt::Throw(_, _)
            | Stmt::Break(_, _)
            | Stmt::Continue(_, _)
            | Stmt::Function(_)
            | Stmt::Class(_)
            | Stmt::Interface(_)
            | Stmt::Enum(_)
            | Stmt::Trait(_)
            | Stmt::Namespace(_, _)
            | Stmt::Use(_) => {
                // Function/class/interface/enum declarations don't introduce
                // variable bindings at this scope, and their bodies have
                // their own scope. `throw`, `break`, `continue`, `namespace`,
                // and `use` don't introduce bindings either.
            }
            Stmt::Try { .. } => {
                let mut deep_names: HashSet<String> = HashSet::new();
                collect_assignments_in_stmt(stmt, &mut deep_names);
                for name in deep_names {
                    if !seen_at_top.contains(&name) {
                        to_hoist.insert(name.clone());
                        seen_at_top.insert(name);
                    }
                }
            }
            Stmt::Block(_, _)
            | Stmt::If { .. }
            | Stmt::While { .. }
            | Stmt::DoWhile { .. }
            | Stmt::For { .. }
            | Stmt::Foreach { .. } => {
                let mut deep_names: HashSet<String> = HashSet::new();
                collect_assignments_in_stmt(stmt, &mut deep_names);
                for name in deep_names {
                    if !seen_at_top.contains(&name) {
                        to_hoist.insert(name.clone());
                        seen_at_top.insert(name);
                    }
                }
            }
        }
    }

    // Pre-populate the declared set with params + hoisted names so the
    // statement emit doesn't try to `let`-lift them.
    for p in params {
        ctx.declared.insert(p.name.clone());
    }
    ctx.declared.extend(to_hoist.iter().cloned());

    if !to_hoist.is_empty() {
        write_indent(out, ctx.indent);
        let mut names: Vec<&String> = to_hoist.iter().collect();
        names.sort();
        out.push_str("let ");
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(n);
        }
        out.push_str(";\n");
    }

    for stmt in stmts {
        // Module-shape declarations and traits are handled out-of-band:
        // namespaces/uses are pre-pass; traits emit nothing (their members
        // are inlined into using classes). Skip them all here so we don't
        // leave blank lines.
        if matches!(stmt, Stmt::Namespace(_, _) | Stmt::Use(_) | Stmt::Trait(_)) {
            continue;
        }
        record_stmt_mapping(stmt, ctx, out);
        write_indent(out, ctx.indent);
        emit_stmt(stmt, ctx, out);
        out.push('\n');
    }
}

/// If a source map is being built, record a mapping from the current
/// emit position back to the start of `stmt`'s source span. Called just
/// before each statement is written. The generated column is `ctx.indent
/// * INDENT_WIDTH` (where the statement text will start after
/// `write_indent`); the source column is taken from the precomputed
/// `LineMap` for the byte offset of `stmt.span().start`.
fn record_stmt_mapping(stmt: &Stmt, ctx: &mut Ctx, out: &str) {
    let Some(sm) = ctx.source_map.as_mut() else {
        return;
    };
    let ts_line = count_newlines(out);
    let span = stmt.span();
    let source = sm.line_map.line_col(span.start);
    sm.builder.record(
        LineCol {
            line: ts_line,
            column: (ctx.indent * INDENT_UNIT.len()) as u32,
        },
        sm.source_idx,
        source,
    );
}

fn count_newlines(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'\n').count() as u32
}

/// Record a sub-statement source-map entry pointing at `span.start`. Used
/// by `emit_expr` at call-site boundaries to give stack traces resolution
/// finer than statement-level when a single line has multiple calls.
/// Generated column is the current column in the active output line.
fn record_expr_mapping(span: psx_ast::Span, ctx: &mut Ctx, out: &str) {
    let Some(sm) = ctx.source_map.as_mut() else {
        return;
    };
    // Last newline position in `out` tells us where the current line
    // started, and therefore the current generated column.
    let line_start = out.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let ts_col = (out.len() - line_start) as u32;
    let ts_line = count_newlines(out);
    let source = sm.line_map.line_col(span.start);
    sm.builder.record(
        LineCol {
            line: ts_line,
            column: ts_col,
        },
        sm.source_idx,
        source,
    );
}

fn collect_assignments_in_stmt(stmt: &Stmt, out: &mut HashSet<String>) {
    match stmt {
        Stmt::Expr(expr, _) => collect_assignments_in_expr(expr, out),
        Stmt::Block(stmts, _) => {
            for s in stmts {
                collect_assignments_in_stmt(s, out);
            }
        }
        Stmt::If {
            cond, then, else_, ..
        } => {
            collect_assignments_in_expr(cond, out);
            collect_assignments_in_stmt(then, out);
            if let Some(e) = else_ {
                collect_assignments_in_stmt(e, out);
            }
        }
        Stmt::Return(None, _) => {}
        Stmt::Return(Some(value), _) => collect_assignments_in_expr(value, out),
        Stmt::While { cond, body, .. } => {
            collect_assignments_in_expr(cond, out);
            collect_assignments_in_stmt(body, out);
        }
        Stmt::DoWhile { body, cond, .. } => {
            collect_assignments_in_stmt(body, out);
            collect_assignments_in_expr(cond, out);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                collect_assignments_in_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_assignments_in_expr(c, out);
            }
            if let Some(s) = step {
                collect_assignments_in_expr(s, out);
            }
            collect_assignments_in_stmt(body, out);
        }
        Stmt::Foreach {
            iter,
            key,
            value,
            body,
            ..
        } => {
            collect_assignments_in_expr(iter, out);
            // The foreach binding declares its own names (emitted as
            // `const` inside the `for` head), so they are NOT outer-scope
            // hoist targets.
            let _ = (key, value);
            collect_assignments_in_stmt(body, out);
        }
        Stmt::Throw(value, _) => collect_assignments_in_expr(value, out),
        Stmt::Break(_, _) | Stmt::Continue(_, _) => {}
        Stmt::Try {
            body,
            catches,
            finally,
            ..
        } => {
            for s in body {
                collect_assignments_in_stmt(s, out);
            }
            for c in catches {
                for s in &c.body {
                    collect_assignments_in_stmt(s, out);
                }
            }
            if let Some(f) = finally {
                for s in f {
                    collect_assignments_in_stmt(s, out);
                }
            }
        }
        Stmt::Function(_)
        | Stmt::Class(_)
        | Stmt::Interface(_)
        | Stmt::Enum(_)
        | Stmt::Trait(_) => {
            // Function/class/interface/enum/trait bodies are their own
            // scope — assignments inside don't contribute to the enclosing
            // scope's hoist set.
        }
        Stmt::Namespace(_, _) | Stmt::Use(_) => {
            // Module-shape declarations don't introduce assignments.
        }
    }
}

fn emit_function_decl(decl: &FunctionDecl, ctx: &mut Ctx, out: &mut String) {
    if decl.async_ {
        out.push_str("async ");
    }
    out.push_str("function ");
    out.push_str(&decl.name);
    out.push('(');
    for (i, p) in decl.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&p.name);
        if let Some(ty) = &p.ty {
            out.push_str(": ");
            emit_type(ty, out);
        }
        if let Some(default) = &p.default {
            out.push_str(" = ");
            emit_expr(default, ctx, out);
        }
    }
    out.push(')');
    if let Some(rt) = &decl.return_type {
        out.push_str(": ");
        emit_type_in_async(rt, decl.async_, ctx.current_class.as_deref(), out);
    } else if decl.async_ {
        out.push_str(": Promise<void>");
    }
    if decl.body.is_empty() {
        out.push_str(" {}");
        return;
    }
    out.push_str(" {\n");
    // Save outer scope state, enter a fresh inner one. Function bodies have
    // their own variable scope, their own hoist pre-pass, and are NOT a
    // module-level emit context (nested decls don't get `export`).
    let saved_declared = std::mem::take(&mut ctx.declared);
    let saved_namespace = ctx.module_namespace.take();
    ctx.indent += 1;
    emit_scope_contents(&decl.body, &decl.params, ctx, out);
    ctx.indent -= 1;
    ctx.declared = saved_declared;
    ctx.module_namespace = saved_namespace;
    write_indent(out, ctx.indent);
    out.push('}');
}

/// Emit an array literal as either a JS list `[...]` (no keys) or a JS
/// object literal `{ ... }` (all keys). Mixed forms are not supported in
/// MVP — they panic in debug for now; the parser accepts them but a future
/// slice will reject them at parse time with a helpful error.
fn emit_array_literal(items: &[ArrayItem], ctx: &mut Ctx, out: &mut String) {
    if items.is_empty() {
        // Empty literal is ambiguous; default to list (JS `[]`).
        out.push_str("[]");
        return;
    }
    let any_keyed = items.iter().any(|i| i.key.is_some());
    let any_unkeyed = items.iter().any(|i| i.key.is_none());

    if any_keyed && any_unkeyed {
        // Mixed-form arrays not yet supported; emit a TS comment so it's
        // visibly broken rather than silently wrong, plus a fallback list
        // that omits the keys.
        out.push_str("/* TODO: mixed list/map array literal not supported */ ");
        out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_expr(&item.value, ctx, out);
        }
        out.push(']');
        return;
    }

    if any_unkeyed {
        out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_expr(&item.value, ctx, out);
        }
        out.push(']');
    } else {
        out.push('{');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            } else {
                out.push(' ');
            }
            let key = item.key.as_ref().expect("any_keyed implies all-keyed here");
            emit_object_key(key, ctx, out);
            out.push_str(": ");
            emit_expr(&item.value, ctx, out);
        }
        out.push_str(" }");
    }
}

/// Emit a key inside an object literal. String literals become bare
/// identifiers when they're valid JS identifiers; otherwise they're
/// quoted. Other expressions become computed keys `[expr]`.
fn emit_object_key(key: &Expr, ctx: &mut Ctx, out: &mut String) {
    if let Expr::Str(s) = key {
        if is_valid_js_ident(s) {
            out.push_str(s);
            return;
        }
        // Fall through to a quoted string literal.
        emit_string_literal(s, out);
        return;
    }
    if let Expr::Int(n) = key {
        // Numeric keys are legal as bare numbers in JS object literals.
        let _ = write!(out, "{n}");
        return;
    }
    out.push('[');
    emit_expr(key, ctx, out);
    out.push(']');
}

fn is_valid_js_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
}

fn emit_class(class: &Class, ctx: &mut Ctx, out: &mut String) {
    // Header. abstract/final/readonly modifiers come in slices 5/7;
    // extends/implements come in slice 6 — they're already in the AST so the
    // emitter handles them when present.
    if class.abstract_ {
        out.push_str("abstract ");
    }
    out.push_str("class ");
    out.push_str(&class.name);
    if let Some(base) = &class.extends {
        out.push_str(" extends ");
        emit_type(base, out);
    }
    if !class.implements.is_empty() {
        out.push_str(" implements ");
        for (i, iface) in class.implements.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_type(iface, out);
        }
    }
    if class.members.is_empty() {
        out.push_str(" {}");
        return;
    }
    out.push_str(" {\n");
    let saved_declared = std::mem::take(&mut ctx.declared);
    let saved_class = ctx.current_class.replace(class.name.clone());
    let saved_namespace = ctx.module_namespace.take();
    ctx.indent += 1;
    let expanded = expand_class_members(&class.members, &ctx.traits);
    for member in &expanded {
        write_indent(out, ctx.indent);
        emit_class_member(member, class, ctx, out);
        out.push('\n');
    }
    ctx.indent -= 1;
    ctx.declared = saved_declared;
    ctx.current_class = saved_class;
    ctx.module_namespace = saved_namespace;
    write_indent(out, ctx.indent);
    out.push('}');
}

/// Walk a class's `members` list and replace each `ClassMember::UseTrait`
/// with the named traits' members inlined in source order. Supports
/// PHP-style `insteadof` / `as` adaptations and transitive trait expansion
/// (a trait `use`-ing another trait).
///
/// Conflict rules:
/// - The class's own members win over a trait's same-named member.
/// - `Foo::m insteadof Bar` drops `Bar::m` before expansion, so the trait-
///   vs-trait conflict marker is suppressed.
/// - Two traits contributing the same member name without an `insteadof`
///   produces a marker constant in the emitted TS so tsc fails loudly.
/// - `Bar::m as newName` (optional visibility) emits a renamed copy of
///   `Bar::m` alongside the standard expansion.
/// - Transitive: when a trait body contains `use OtherTrait;`, the other
///   trait's members flatten in at the same expansion site. Cycles emit a
///   marker constant rather than looping.
/// - Unknown trait names produce a marker so the user sees the error in
///   the output rather than silently losing members.
fn expand_class_members(members: &[ClassMember], traits: &TraitMap<'_>) -> Vec<ClassMember> {
    let mut class_member_names: HashSet<String> = HashSet::new();
    for m in members {
        if let Some(name) = member_name(m) {
            class_member_names.insert(name);
        }
    }

    let mut out: Vec<ClassMember> = Vec::with_capacity(members.len());
    let mut introduced_by_trait: HashSet<String> = HashSet::new();
    for m in members {
        match m {
            ClassMember::UseTrait(block) => {
                // Build per-trait loser-method sets from any `insteadof`
                // adaptations. `Foo::greet insteadof Bar, Baz` means Bar
                // and Baz each lose their `greet` member before expansion.
                let mut losers_for: BTreeMap<String, HashSet<String>> = BTreeMap::new();
                for adaptation in &block.adaptations {
                    if let TraitAdaptation::InsteadOf { method, losers, .. } = adaptation {
                        for loser in losers {
                            losers_for
                                .entry(loser.clone())
                                .or_default()
                                .insert(method.clone());
                        }
                    }
                }

                for ty in &block.traits {
                    let trait_name = type_simple_name(ty);
                    let mut visited: HashSet<String> = HashSet::new();
                    let mut flat: Vec<(String, ClassMember)> = Vec::new();
                    flatten_trait(&trait_name, traits, &mut visited, &mut flat);
                    let trait_losers = losers_for.get(&trait_name);
                    for (origin_trait, tm) in flat {
                        if matches!(tm, ClassMember::UseTrait(_)) {
                            continue;
                        }
                        let Some(name) = member_name(&tm) else {
                            continue;
                        };
                        if trait_losers.map(|s| s.contains(&name)).unwrap_or(false) {
                            continue;
                        }
                        if class_member_names.contains(&name) {
                            continue;
                        }
                        if !introduced_by_trait.insert(name.clone()) {
                            out.push(emit_comment_member(format!(
                                "trait `{origin_trait}` redeclares `{name}` already provided by another trait"
                            )));
                            continue;
                        }
                        out.push(tm);
                    }
                }

                // Apply `as` aliases AFTER the standard expansion so the
                // alias name is in addition to (not in place of) any
                // already-emitted original.
                for adaptation in &block.adaptations {
                    if let TraitAdaptation::Alias {
                        source_trait,
                        source_method,
                        new_name,
                        new_visibility,
                    } = adaptation
                    {
                        let mut visited: HashSet<String> = HashSet::new();
                        let mut flat: Vec<(String, ClassMember)> = Vec::new();
                        flatten_trait(source_trait, traits, &mut visited, &mut flat);
                        let original = flat.into_iter().find_map(|(_, m)| match &m {
                            ClassMember::Method(method) if method.name == *source_method => Some(m),
                            _ => None,
                        });
                        match original {
                            Some(ClassMember::Method(method)) => {
                                let mut renamed = method.clone();
                                renamed.name = new_name.clone();
                                if let Some(v) = new_visibility {
                                    renamed.visibility = v.clone();
                                }
                                out.push(ClassMember::Method(renamed));
                            }
                            _ => {
                                out.push(emit_comment_member(format!(
                                    "trait alias `{source_trait}::{source_method}` did not resolve to a method"
                                )));
                            }
                        }
                    }
                }
            }
            other => out.push(other.clone()),
        }
    }
    out
}

/// Recursively flatten a trait's members into `out`, following any nested
/// `ClassMember::UseTrait` directives inside trait bodies (transitive
/// expansion). Each pushed member is paired with the name of the trait
/// that actually declared it (for conflict reporting).
///
/// `visited` is the chain of trait names being expanded; if `name` is
/// already there we have a cycle (A uses B uses A) and push a marker
/// instead of recursing.
fn flatten_trait(
    name: &str,
    traits: &TraitMap<'_>,
    visited: &mut HashSet<String>,
    out: &mut Vec<(String, ClassMember)>,
) {
    if !visited.insert(name.to_string()) {
        out.push((
            name.to_string(),
            emit_comment_member(format!("trait cycle detected involving `{name}`")),
        ));
        return;
    }
    let Some(decl) = traits.get(name).copied() else {
        out.push((
            name.to_string(),
            emit_comment_member(format!("trait `{name}` not found at expansion time")),
        ));
        visited.remove(name);
        return;
    };
    for m in &decl.members {
        match m {
            ClassMember::UseTrait(block) => {
                for ty in &block.traits {
                    let sub = type_simple_name(ty);
                    flatten_trait(&sub, traits, visited, out);
                }
            }
            other => out.push((name.to_string(), other.clone())),
        }
    }
    visited.remove(name);
}

fn member_name(m: &ClassMember) -> Option<String> {
    match m {
        ClassMember::Property(p) => Some(p.name.clone()),
        ClassMember::Method(m) => Some(m.name.clone()),
        ClassMember::Constant(c) => Some(c.name.clone()),
        ClassMember::UseTrait(_) => None,
    }
}

/// The "name" of a trait reference in `use TraitX;`. We currently strip any
/// generic args / nullable wrappers and use the bare ident — multi-level
/// PHP `\` paths aren't representable in `TypeAnn::Named` (those parse as
/// generics or namespaced types in our grammar; defer richer resolution).
fn type_simple_name(ty: &TypeAnn) -> String {
    match ty {
        TypeAnn::Named(n) => n.clone(),
        TypeAnn::Nullable(inner) => type_simple_name(inner),
        TypeAnn::Generic(name, _) => name.clone(),
        TypeAnn::Union(parts) => parts.first().map(type_simple_name).unwrap_or_default(),
    }
}

/// Build a synthetic class constant whose declared "value" is a string
/// literal carrying a comment-style note. Used to inject visible markers
/// into the emitted class body when trait expansion has issues; the
/// constant serialises as a `/* note */` so it stays out of TS member
/// scope.
fn emit_comment_member(note: String) -> ClassMember {
    // We don't have a "comment" class member shape; encode the note as a
    // class constant with a conventionally-private visibility plus a
    // documented suffix so it round-trips as commented TS later. To keep
    // emit-side simple we instead use a Property with hooks=None and a
    // synthetic name prefixed with `__psx_note_`. Below is a less hacky
    // approach: a class constant with a name like `__PSX_NOTE_<n>`.
    ClassMember::Constant(ClassConstant {
        visibility: Visibility::Private,
        final_: false,
        ty: Some(TypeAnn::Named("string".into())),
        name: format!("__PSX_NOTE_{:p}", &note as *const _),
        value: Expr::Str(note),
    })
}

fn emit_class_member(member: &ClassMember, class: &Class, ctx: &mut Ctx, out: &mut String) {
    match member {
        ClassMember::Property(p) if p.hooks.is_some() => {
            emit_hooked_property(p, ctx, out);
        }
        ClassMember::Property(p) => emit_property(p, ctx, out),
        ClassMember::Method(m) => emit_method(m, class.readonly, ctx, out),
        ClassMember::Constant(c) => emit_class_constant(c, ctx, out),
        ClassMember::UseTrait(_) => {
            // Trait expansion happens earlier in `emit_class` via
            // `expand_class_members`; if we reach here we hit a programmer
            // bug — the expanded list shouldn't contain UseTrait nodes.
            unreachable!("UseTrait should be expanded before emit_class_member");
        }
    }
}

/// A property with hooks lowers to *three* TS class members in source
/// order: a private backing field, then the getter (if a `get` hook was
/// declared), then the setter (if a `set` hook was declared). The outer
/// loop in `emit_class` emits the backing field with the expected leading
/// indent + trailing newline; the getter and setter slots emit their own
/// indent and newline so they appear as siblings of the backing field.
fn emit_hooked_property(p: &Property, ctx: &mut Ctx, out: &mut String) {
    let hooks = p
        .hooks
        .as_ref()
        .expect("emit_hooked_property called only when hooks set");

    // 1. Backing field — `private _<name>: T = default;`.
    // When the hooked property has no default, emit a `!` definite-
    // assignment assertion: TS strict mode otherwise complains that the
    // private backing field isn't initialised. The setter (whether the
    // user calls it explicitly or via the constructor) is responsible
    // for filling it in.
    out.push_str("private _");
    out.push_str(&p.name);
    if p.default.is_none() {
        out.push('!');
    }
    if let Some(ty) = &p.ty {
        out.push_str(": ");
        emit_type_in(ty, ctx.current_class.as_deref(), out);
    }
    if let Some(default) = &p.default {
        out.push_str(" = ");
        let mut tmp = Ctx {
            current_class: ctx.current_class.clone(),
            ..Ctx::default()
        };
        emit_expr(default, &mut tmp, out);
    }
    out.push(';');

    if let Some(body) = &hooks.get {
        out.push('\n');
        write_indent(out, ctx.indent);
        emit_get_hook(p, body, ctx, out);
    }

    if let Some(set) = &hooks.set {
        out.push('\n');
        write_indent(out, ctx.indent);
        emit_set_hook(p, set, ctx, out);
    }
}

fn emit_get_hook(p: &Property, body: &HookBody, ctx: &mut Ctx, out: &mut String) {
    emit_visibility(p.visibility, out);
    out.push_str(" get ");
    out.push_str(&p.name);
    out.push_str("()");
    if let Some(ty) = &p.ty {
        out.push_str(": ");
        emit_type_in(ty, ctx.current_class.as_deref(), out);
    }
    out.push_str(" {\n");
    let saved_hook = ctx.current_hook_property.replace(p.name.clone());
    let saved_declared = std::mem::take(&mut ctx.declared);
    ctx.indent += 1;
    match body {
        HookBody::Expr(e) => {
            write_indent(out, ctx.indent);
            out.push_str("return ");
            emit_expr(e, ctx, out);
            out.push_str(";\n");
        }
        HookBody::Block(stmts) => {
            emit_scope_contents(stmts, &[], ctx, out);
        }
    }
    ctx.indent -= 1;
    ctx.declared = saved_declared;
    ctx.current_hook_property = saved_hook;
    write_indent(out, ctx.indent);
    out.push('}');
}

fn emit_set_hook(p: &Property, set: &SetHook, ctx: &mut Ctx, out: &mut String) {
    let setter_vis = p.set_visibility.unwrap_or(p.visibility);
    emit_visibility(setter_vis, out);
    out.push_str(" set ");
    out.push_str(&p.name);
    out.push('(');
    out.push_str(&set.param_name);
    let param_ty = set.param_type.as_ref().or(p.ty.as_ref());
    if let Some(ty) = param_ty {
        out.push_str(": ");
        emit_type_in(ty, ctx.current_class.as_deref(), out);
    }
    out.push_str(") {\n");
    let saved_hook = ctx.current_hook_property.replace(p.name.clone());
    let saved_declared = std::mem::take(&mut ctx.declared);
    ctx.indent += 1;
    match &set.body {
        HookBody::Expr(e) => {
            // Short setter: assign the expression's value to the backing
            // field. `set => strtolower($value);` becomes
            // `this._name = strtolower(value);`.
            write_indent(out, ctx.indent);
            out.push_str("this._");
            out.push_str(&p.name);
            out.push_str(" = ");
            emit_expr(e, ctx, out);
            out.push_str(";\n");
        }
        HookBody::Block(stmts) => {
            // The hook's parameter is treated like a method parameter: it's
            // pre-declared so the hoist pass doesn't try to `let`-lift it.
            let pseudo_param = Param {
                name: set.param_name.clone(),
                ty: set.param_type.clone().or_else(|| p.ty.clone()),
                default: None,
                promotion: None,
            };
            emit_scope_contents(stmts, std::slice::from_ref(&pseudo_param), ctx, out);
        }
    }
    ctx.indent -= 1;
    ctx.declared = saved_declared;
    ctx.current_hook_property = saved_hook;
    write_indent(out, ctx.indent);
    out.push('}');
}

fn emit_enum(e: &EnumDecl, ctx: &mut Ctx, out: &mut String) {
    // PHP enums map to TS `enum`. Backed PHP enums correspond to TS
    // string/number enums depending on the backing type. Pure PHP enums
    // map to TS auto-numbering enums.
    //
    // `implements` and per-enum constants don't have a clean TS-`enum`
    // home — TS `enum` is just a syntactic enumeration with no member
    // body. Constants and `implements` are accepted by the parser and
    // emitted as a doc-comment for now; the proper home is the
    // class-shape emit slated for the methods-on-enums Phase 5 task.
    if !e.implements.is_empty() || !e.constants.is_empty() {
        out.push_str("/* note: enum `implements`/`const` are accepted but ");
        out.push_str("not represented in TS `enum`; deferred to Phase 5. */\n");
        write_indent(out, ctx.indent);
    }
    out.push_str("enum ");
    out.push_str(&e.name);
    if e.cases.is_empty() {
        out.push_str(" {}");
        return;
    }
    out.push_str(" {\n");
    ctx.indent += 1;
    for (i, case) in e.cases.iter().enumerate() {
        write_indent(out, ctx.indent);
        out.push_str(&case.name);
        if let Some(v) = &case.value {
            out.push_str(" = ");
            // Enum case values are evaluated in class scope; use a fresh
            // declared set but inherit current_class for any self/static.
            let mut tmp = Ctx {
                current_class: ctx.current_class.clone(),
                ..Ctx::default()
            };
            emit_expr(v, &mut tmp, out);
        }
        if i + 1 < e.cases.len() {
            out.push(',');
        }
        out.push('\n');
    }
    ctx.indent -= 1;
    write_indent(out, ctx.indent);
    out.push('}');
    let _ = (ctx,);
    let _ = (e.backed_type.as_ref(),);
}

fn emit_interface(iface: &Interface, ctx: &mut Ctx, out: &mut String) {
    out.push_str("interface ");
    out.push_str(&iface.name);
    if !iface.extends.is_empty() {
        out.push_str(" extends ");
        for (i, parent) in iface.extends.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            emit_type(parent, out);
        }
    }
    if iface.members.is_empty() {
        out.push_str(" {}");
        return;
    }
    out.push_str(" {\n");
    let saved_class = ctx.current_class.replace(iface.name.clone());
    let saved_namespace = ctx.module_namespace.take();
    ctx.indent += 1;
    for member in &iface.members {
        write_indent(out, ctx.indent);
        emit_interface_member(member, ctx, out);
        out.push('\n');
    }
    ctx.indent -= 1;
    ctx.current_class = saved_class;
    ctx.module_namespace = saved_namespace;
    write_indent(out, ctx.indent);
    out.push('}');
}

fn emit_interface_member(member: &InterfaceMember, ctx: &mut Ctx, out: &mut String) {
    match member {
        InterfaceMember::Method(m) => {
            // TS interface methods have NO visibility, NO `function`/`abstract`
            // keywords, NO body — just `name(params): RetType;`.
            out.push_str(&m.name);
            out.push('(');
            for (i, p) in m.params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&p.name);
                if let Some(t) = &p.ty {
                    out.push_str(": ");
                    emit_type_in(t, ctx.current_class.as_deref(), out);
                }
                if let Some(d) = &p.default {
                    out.push_str(" = ");
                    emit_expr(d, ctx, out);
                }
            }
            out.push(')');
            if let Some(rt) = &m.return_type {
                out.push_str(": ");
                emit_type_in(rt, ctx.current_class.as_deref(), out);
            }
            out.push(';');
        }
        InterfaceMember::Constant(c) => {
            // Interface constants don't have a clean TS home — putting them
            // inside `interface { readonly K: T; }` forces every implementor
            // to provide them, which doesn't match PHP semantics (the
            // constant value lives on the interface itself, not on
            // implementors). We emit a doc-comment placeholder; runtime
            // access via `Iface::CONST` is a known gap and a Phase 5
            // follow-up (likely as a sibling `namespace Iface { ... }`).
            out.push_str("// const ");
            out.push_str(&c.name);
            if let Some(t) = &c.ty {
                out.push_str(": ");
                emit_type_in(t, ctx.current_class.as_deref(), out);
            }
            out.push_str(" = ");
            emit_expr(&c.value, ctx, out);
            out.push(';');
        }
    }
}

fn emit_class_constant(c: &ClassConstant, ctx: &mut Ctx, out: &mut String) {
    // PHP class constants are always implicitly static and read-only. TS
    // expresses this as `static readonly`.
    emit_visibility(c.visibility, out);
    out.push_str(" static readonly ");
    out.push_str(&c.name);
    if let Some(t) = &c.ty {
        out.push_str(": ");
        emit_type_in(t, ctx.current_class.as_deref(), out);
    }
    out.push_str(" = ");
    emit_expr(&c.value, ctx, out);
    out.push(';');
}

fn emit_visibility(v: Visibility, out: &mut String) {
    out.push_str(visibility_word(v));
}

fn visibility_word(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Protected => "protected",
        Visibility::Private => "private",
    }
}

fn emit_property(p: &Property, ctx: &mut Ctx, out: &mut String) {
    emit_visibility(p.visibility, out);
    out.push(' ');
    if p.static_ {
        out.push_str("static ");
    }
    // Asymmetric visibility -> `readonly` on the TS side. We pick the
    // wider of the get-side / set-side for output and add `readonly` so
    // outside-class writes are blocked at compile time. A note comment is
    // emitted when the mapping is lossy (set side is wider than `private`).
    let asym_readonly = p.set_visibility.is_some();
    if p.readonly || asym_readonly {
        out.push_str("readonly ");
    }
    if let Some(set_v) = p.set_visibility {
        if !matches!(set_v, Visibility::Private) {
            // Lossy mapping — record the source intent for human readers.
            out.push_str("/* ");
            out.push_str(visibility_word(p.visibility));
            out.push(' ');
            out.push_str(visibility_word(set_v));
            out.push_str("(set) — TS approximates as readonly */ ");
        }
    }
    out.push_str(&p.name);
    if let Some(t) = &p.ty {
        out.push_str(": ");
        emit_type_in(t, ctx.current_class.as_deref(), out);
    }
    if let Some(d) = &p.default {
        out.push_str(" = ");
        // Default values are evaluated in class scope; emit with a fresh
        // declared set so they don't accidentally pull names from the
        // surrounding module hoist set, but keep current_class tied through.
        let mut tmp = Ctx {
            current_class: ctx.current_class.clone(),
            ..Ctx::default()
        };
        emit_expr(d, &mut tmp, out);
    }
    out.push(';');
}

fn emit_method(m: &Method, class_is_readonly: bool, ctx: &mut Ctx, out: &mut String) {
    emit_visibility(m.visibility, out);
    out.push(' ');
    if m.static_ {
        out.push_str("static ");
    }
    if m.abstract_ {
        out.push_str("abstract ");
    }
    if m.async_ {
        out.push_str("async ");
    }
    // PHP `__construct` -> TS `constructor`. The constructor has no return
    // type in TS even if PHP source declared `: void`.
    let is_constructor = m.name == "__construct";
    let display_name = if is_constructor {
        "constructor"
    } else {
        &m.name
    };
    out.push_str(display_name);
    out.push('(');
    for (i, p) in m.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        // Promoted constructor parameters become TS parameter properties:
        // `constructor(public readonly name: string)`. A `readonly` class
        // forces every promoted property to be readonly. Asymmetric
        // visibility uses the same readonly-fallback as regular properties.
        if let Some(promo) = &p.promotion {
            emit_visibility(promo.visibility, out);
            out.push(' ');
            let asym_readonly = promo.set_visibility.is_some();
            if promo.readonly || class_is_readonly || asym_readonly {
                out.push_str("readonly ");
            }
            if let Some(set_v) = promo.set_visibility {
                if !matches!(set_v, Visibility::Private) {
                    out.push_str("/* ");
                    out.push_str(visibility_word(promo.visibility));
                    out.push(' ');
                    out.push_str(visibility_word(set_v));
                    out.push_str("(set) — TS approximates as readonly */ ");
                }
            }
        }
        out.push_str(&p.name);
        if let Some(t) = &p.ty {
            out.push_str(": ");
            emit_type_in(t, ctx.current_class.as_deref(), out);
        }
        if let Some(d) = &p.default {
            out.push_str(" = ");
            emit_expr(d, ctx, out);
        }
    }
    out.push(')');
    if !is_constructor {
        if let Some(rt) = &m.return_type {
            out.push_str(": ");
            emit_type_in_async(rt, m.async_, ctx.current_class.as_deref(), out);
        } else if m.async_ {
            // Async without explicit return type -> Promise<void>.
            out.push_str(": Promise<void>");
        }
    }
    match &m.body {
        None => {
            // Abstract or interface declaration — no body, just a semicolon.
            out.push(';');
        }
        Some(body) if body.is_empty() => {
            out.push_str(" {}");
        }
        Some(body) => {
            out.push_str(" {\n");
            let saved_declared = std::mem::take(&mut ctx.declared);
            let saved_namespace = ctx.module_namespace.take();
            ctx.indent += 1;
            emit_scope_contents(body, &m.params, ctx, out);
            ctx.indent -= 1;
            ctx.declared = saved_declared;
            ctx.module_namespace = saved_namespace;
            write_indent(out, ctx.indent);
            out.push('}');
        }
    }
}

/// PHPScript -> TypeScript type emit. `current_class` is the lexically
/// enclosing class (or `None` outside a class body); `self` and `static`
/// in type position resolve to that name in TS.
fn emit_type(t: &TypeAnn, out: &mut String) {
    emit_type_in(t, None, out);
}

/// Emit a function/method return type, auto-wrapping in `Promise<...>` if
/// the function is `async` and the user didn't already write `Promise<...>`.
fn emit_type_in_async(t: &TypeAnn, is_async: bool, current_class: Option<&str>, out: &mut String) {
    if !is_async {
        emit_type_in(t, current_class, out);
        return;
    }
    let already_promise = matches!(t, TypeAnn::Generic(name, _) if name == "Promise");
    if already_promise {
        emit_type_in(t, current_class, out);
    } else {
        out.push_str("Promise<");
        emit_type_in(t, current_class, out);
        out.push('>');
    }
}

fn emit_type_in(t: &TypeAnn, current_class: Option<&str>, out: &mut String) {
    match t {
        TypeAnn::Named(name) => match name.as_str() {
            "int" | "float" => out.push_str("number"),
            "bool" => out.push_str("boolean"),
            "mixed" => out.push_str("any"),
            // `self` and `static` in type position resolve to the enclosing
            // class. Outside a class we leave them verbatim — tsc will
            // reject them, which is the correct outcome.
            "self" | "static" => match current_class {
                Some(c) => out.push_str(c),
                None => out.push_str(name),
            },
            // Pass through: string, void, never, null, object, identifiers,
            // class names.
            other => out.push_str(other),
        },
        TypeAnn::Nullable(inner) => {
            emit_type_in(inner, current_class, out);
            out.push_str(" | null");
        }
        TypeAnn::Union(parts) => {
            for (i, p) in parts.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                emit_type_in(p, current_class, out);
            }
        }
        TypeAnn::Generic(name, params) => match (name.as_str(), params.len()) {
            // `array<T>` -> `T[]`
            ("array", 1) => {
                emit_type_in(&params[0], current_class, out);
                out.push_str("[]");
            }
            // `array<K, V>` -> `Record<K, V>`
            ("array", 2) => {
                out.push_str("Record<");
                emit_type_in(&params[0], current_class, out);
                out.push_str(", ");
                emit_type_in(&params[1], current_class, out);
                out.push('>');
            }
            // Pass through everything else as a TS generic instantiation.
            _ => {
                out.push_str(name);
                out.push('<');
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    emit_type_in(p, current_class, out);
                }
                out.push('>');
            }
        },
    }
}

fn collect_assignments_in_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Assign { target, value } | Expr::CompoundAssign { target, value, .. } => {
            if let Expr::Var(name) = target.as_ref() {
                out.insert(name.clone());
            }
            collect_assignments_in_expr(value, out);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_assignments_in_expr(lhs, out);
            collect_assignments_in_expr(rhs, out);
        }
        Expr::Unary { expr, .. } | Expr::Await(expr) | Expr::FirstClassCallable(expr) => {
            collect_assignments_in_expr(expr, out);
        }
        Expr::IncDec { target, .. } => {
            collect_assignments_in_expr(target, out);
        }
        Expr::ArrowFn { body, .. } => {
            // Arrow-fn bodies are their own scope; don't hoist names from
            // inside into the enclosing scope.
            let _ = body;
        }
        Expr::Call { callee, args, .. } => {
            collect_assignments_in_expr(callee, out);
            for a in args {
                collect_assignments_in_expr(a, out);
            }
        }
        Expr::Array(items) => {
            for item in items {
                if let Some(k) = &item.key {
                    collect_assignments_in_expr(k, out);
                }
                collect_assignments_in_expr(&item.value, out);
            }
        }
        Expr::Index { obj, key } => {
            collect_assignments_in_expr(obj, out);
            collect_assignments_in_expr(key, out);
        }
        Expr::Access { target, .. } => {
            collect_assignments_in_expr(target, out);
        }
        Expr::New { args, .. } => {
            for a in args {
                collect_assignments_in_expr(a, out);
            }
        }
        Expr::Match { scrutinee, arms } => {
            collect_assignments_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(cs) = &arm.conds {
                    for c in cs {
                        collect_assignments_in_expr(c, out);
                    }
                }
                collect_assignments_in_expr(&arm.body, out);
            }
        }
        Expr::Ternary { cond, then, else_ } => {
            collect_assignments_in_expr(cond, out);
            collect_assignments_in_expr(then, out);
            collect_assignments_in_expr(else_, out);
        }
        Expr::ShortTernary { cond, else_ } => {
            collect_assignments_in_expr(cond, out);
            collect_assignments_in_expr(else_, out);
        }
        Expr::InterpolatedStr(parts) => {
            for part in parts {
                if let InterpolatedPart::Expr(e) = part {
                    collect_assignments_in_expr(e, out);
                }
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Str(_)
        | Expr::Bool(_)
        | Expr::Null
        | Expr::Var(_)
        | Expr::Ident(_)
        | Expr::SelfRef
        | Expr::ParentRef
        | Expr::StaticRef => {}
    }
}

/// Emit a single statement WITHOUT leading indent or trailing newline; the
/// caller provides those. This makes inline contexts (e.g., `if (cond) <stmt>`)
/// straightforward.
fn emit_stmt(stmt: &Stmt, ctx: &mut Ctx, out: &mut String) {
    match stmt {
        // Assignment as a top-level expression-statement is special: when the
        // target is a bare variable not yet declared, lift it to `let`.
        Stmt::Expr(Expr::Assign { target, value }, _) => {
            if let Expr::Var(name) = target.as_ref() {
                if !ctx.declared.contains(name) {
                    ctx.declared.insert(name.clone());
                    out.push_str("let ");
                    out.push_str(name);
                    out.push_str(" = ");
                    emit_expr(value, ctx, out);
                    out.push(';');
                    return;
                }
            }
            emit_expr(
                &Expr::Assign {
                    target: target.clone(),
                    value: value.clone(),
                },
                ctx,
                out,
            );
            out.push(';');
        }
        // Compound assignment never lifts to `let`.
        Stmt::Expr(Expr::CompoundAssign { op, target, value }, _) => {
            emit_expr(
                &Expr::CompoundAssign {
                    op: *op,
                    target: target.clone(),
                    value: value.clone(),
                },
                ctx,
                out,
            );
            out.push(';');
        }
        Stmt::Expr(expr, _) => {
            emit_expr(expr, ctx, out);
            out.push(';');
        }
        Stmt::Block(stmts, _) => {
            out.push('{');
            if !stmts.is_empty() {
                out.push('\n');
                ctx.indent += 1;
                for s in stmts {
                    write_indent(out, ctx.indent);
                    emit_stmt(s, ctx, out);
                    out.push('\n');
                }
                ctx.indent -= 1;
                write_indent(out, ctx.indent);
            }
            out.push('}');
        }
        Stmt::If {
            cond, then, else_, ..
        } => {
            out.push_str("if (");
            emit_expr(cond, ctx, out);
            out.push_str(") ");
            emit_stmt(then, ctx, out);
            if let Some(e) = else_ {
                out.push_str(" else ");
                emit_stmt(e, ctx, out);
            }
        }
        Stmt::Return(None, _) => out.push_str("return;"),
        Stmt::Return(Some(value), _) => {
            out.push_str("return ");
            emit_expr(value, ctx, out);
            out.push(';');
        }
        Stmt::While { cond, body, .. } => {
            out.push_str("while (");
            emit_expr(cond, ctx, out);
            out.push_str(") ");
            emit_stmt(body, ctx, out);
        }
        Stmt::DoWhile { body, cond, .. } => {
            out.push_str("do ");
            emit_stmt(body, ctx, out);
            out.push_str(" while (");
            emit_expr(cond, ctx, out);
            out.push_str(");");
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            out.push_str("for (");
            if let Some(i) = init {
                // `init` was parsed as `Stmt::Expr(<expr>, _)`; emit just
                // the expression so we don't get a stray `;`.
                if let Stmt::Expr(e, _) = i.as_ref() {
                    // Detect `$x = …` first-use: hoist as `let` since the
                    // for-head is the var's introduction.
                    match e {
                        Expr::Assign { target, value } => {
                            if let Expr::Var(name) = target.as_ref() {
                                if !ctx.declared.contains(name) {
                                    ctx.declared.insert(name.clone());
                                    out.push_str("let ");
                                    out.push_str(name);
                                    out.push_str(" = ");
                                    emit_expr(value, ctx, out);
                                } else {
                                    emit_expr(e, ctx, out);
                                }
                            } else {
                                emit_expr(e, ctx, out);
                            }
                        }
                        _ => emit_expr(e, ctx, out),
                    }
                } else {
                    emit_stmt(i, ctx, out);
                }
            }
            out.push_str("; ");
            if let Some(c) = cond {
                emit_expr(c, ctx, out);
            }
            out.push_str("; ");
            if let Some(s) = step {
                emit_expr(s, ctx, out);
            }
            out.push_str(") ");
            emit_stmt(body, ctx, out);
        }
        Stmt::Break(level, _) => {
            if let Some(n) = level {
                // PHP `break N` means break out of N nested loops. TS
                // doesn't have an integer-level form; emit a comment
                // explaining and use plain `break` (covers level=1; for
                // N > 1 the user should refactor — flag clearly).
                if *n > 1 {
                    out.push_str("/* break ");
                    out.push_str(&n.to_string());
                    out.push_str(" — multi-level break is not representable in TS; refactor */ ");
                }
            }
            out.push_str("break;");
        }
        Stmt::Continue(level, _) => {
            if let Some(n) = level {
                if *n > 1 {
                    out.push_str("/* continue ");
                    out.push_str(&n.to_string());
                    out.push_str(
                        " — multi-level continue is not representable in TS; refactor */ ",
                    );
                }
            }
            out.push_str("continue;");
        }
        Stmt::Function(decl) => {
            if ctx.module_namespace.is_some() {
                out.push_str("export ");
            }
            emit_function_decl(decl, ctx, out);
        }
        Stmt::Throw(value, _) => {
            out.push_str("throw ");
            emit_expr(value, ctx, out);
            out.push(';');
        }
        Stmt::Namespace(_, _) | Stmt::Use(_) => {
            // No-op at emit_stmt time. Namespace + uses are handled in
            // `emit()`'s pre-pass (which sets `Ctx.module_namespace` and
            // hoists imports above all output). The surrounding scope
            // emitter must skip these stmts to avoid emitting blank lines.
        }
        Stmt::Try {
            body,
            catches,
            finally,
            ..
        } => {
            emit_try(body, catches, finally.as_deref(), ctx, out);
        }
        Stmt::Class(class) => {
            if ctx.module_namespace.is_some() {
                out.push_str("export ");
            }
            emit_class(class, ctx, out);
        }
        Stmt::Interface(iface) => {
            if ctx.module_namespace.is_some() {
                out.push_str("export ");
            }
            emit_interface(iface, ctx, out);
        }
        Stmt::Enum(e) => {
            if ctx.module_namespace.is_some() {
                out.push_str("export ");
            }
            emit_enum(e, ctx, out);
        }
        Stmt::Trait(_) => {
            // Traits are erased at compile time. Their members live in
            // the using classes via `emit_class`'s expansion of
            // `ClassMember::UseTrait`. Nothing emits to TS here.
        }
        Stmt::Foreach {
            iter,
            key,
            value,
            body,
            ..
        } => {
            // `foreach (xs as $v)`         -> `for (const v of xs)`
            // `foreach (xs as $k => $v)`   -> `for (const [k, v] of Object.entries(xs))`
            //
            // Without type info we can't pick array-vs-object iteration
            // perfectly. The value-only form assumes an iterable (works for
            // arrays/maps/sets); the key-value form uses Object.entries which
            // works for plain objects. PHP's mixed-key arrays don't survive
            // either form intact — that's a documented gap until we have a
            // proper type system in Phase 5+.
            match key {
                None => {
                    out.push_str("for (const ");
                    out.push_str(value);
                    out.push_str(" of ");
                    emit_expr(iter, ctx, out);
                    out.push_str(") ");
                }
                Some(k) => {
                    out.push_str("for (const [");
                    out.push_str(k);
                    out.push_str(", ");
                    out.push_str(value);
                    out.push_str("] of Object.entries(");
                    emit_expr(iter, ctx, out);
                    out.push_str(")) ");
                }
            }
            emit_stmt(body, ctx, out);
        }
    }
}

fn emit_expr(expr: &Expr, ctx: &mut Ctx, out: &mut String) {
    match expr {
        Expr::Int(v) => {
            let _ = write!(out, "{v}");
        }
        Expr::Float(v) => {
            // `{}` for f64 prints `3.14` and `42` (no fractional). To preserve
            // the "this is a float" distinction in TS source, force at least
            // one decimal place when the value happens to be integral.
            if v.fract() == 0.0 && v.is_finite() {
                let _ = write!(out, "{v:.1}");
            } else {
                let _ = write!(out, "{v}");
            }
        }
        Expr::Str(s) => {
            emit_string_literal(s, out);
        }
        Expr::InterpolatedStr(parts) => {
            emit_interpolated_string(parts, ctx, out);
        }
        Expr::Bool(true) => out.push_str("true"),
        Expr::Bool(false) => out.push_str("false"),
        Expr::Null => out.push_str("null"),
        Expr::Var(name) => out.push_str(name),
        Expr::Ident(name) => out.push_str(name),
        Expr::Call {
            callee, args, span, ..
        } => {
            // Record a sub-statement source-map entry pointing at the
            // start of this call site. Useful for stack traces in chained
            // expression lines like `a(b(c()))`.
            record_expr_mapping(*span, ctx, out);
            // Wrap the callee in parens if it's a binary/unary/etc that
            // would otherwise bind too loosely. Most call targets are bare
            // identifiers or variables and don't need them.
            match callee.as_ref() {
                Expr::Ident(_)
                | Expr::Var(_)
                | Expr::Call { .. }
                | Expr::Index { .. }
                | Expr::Access { .. } => {
                    emit_expr(callee, ctx, out);
                }
                _ => {
                    out.push('(');
                    emit_expr(callee, ctx, out);
                    out.push(')');
                }
            }
            out.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(arg, ctx, out);
            }
            out.push(')');
        }
        Expr::Array(items) => {
            emit_array_literal(items, ctx, out);
        }
        Expr::Index { obj, key } => {
            // Same parens rule as call: only literals/idents/vars/postfix
            // chain stay bare.
            match obj.as_ref() {
                Expr::Ident(_)
                | Expr::Var(_)
                | Expr::Call { .. }
                | Expr::Index { .. }
                | Expr::Access { .. }
                | Expr::Array(_) => {
                    emit_expr(obj, ctx, out);
                }
                _ => {
                    out.push('(');
                    emit_expr(obj, ctx, out);
                    out.push(')');
                }
            }
            out.push('[');
            emit_expr(key, ctx, out);
            out.push(']');
        }
        Expr::Access { target, name, op } => {
            // Special-case: `parent::__construct` -> bare `super`. The
            // surrounding Call wraps it into `super(args)` which is the TS
            // form of "call the parent constructor".
            if matches!(target.as_ref(), Expr::ParentRef)
                && matches!(op, AccessOp::DoubleColon)
                && name == "__construct"
            {
                out.push_str("super");
                return;
            }
            // Inside a property hook body for `<prop>`, rewrite
            // `$this-><prop>` to `this._<prop>` so the body manipulates the
            // backing field instead of recursing through the getter/setter.
            if matches!(op, AccessOp::Arrow)
                && matches!(target.as_ref(), Expr::Var(n) if n == "this")
            {
                if let Some(prop) = ctx.current_hook_property.as_ref() {
                    if name == prop {
                        out.push_str("this._");
                        out.push_str(name);
                        return;
                    }
                }
            }
            match target.as_ref() {
                Expr::Ident(_)
                | Expr::Var(_)
                | Expr::Call { .. }
                | Expr::Index { .. }
                | Expr::Access { .. }
                | Expr::Array(_)
                | Expr::New { .. }
                | Expr::SelfRef
                | Expr::ParentRef
                | Expr::StaticRef => emit_expr(target, ctx, out),
                _ => {
                    out.push('(');
                    emit_expr(target, ctx, out);
                    out.push(')');
                }
            }
            out.push_str(match op {
                AccessOp::Arrow | AccessOp::DoubleColon => ".",
                AccessOp::NullSafeArrow => "?.",
            });
            out.push_str(name);
        }
        Expr::New { class, args } => {
            out.push_str("new ");
            emit_type_in(class, ctx.current_class.as_deref(), out);
            out.push('(');
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                emit_expr(arg, ctx, out);
            }
            out.push(')');
        }
        Expr::SelfRef | Expr::StaticRef => {
            // `self::` / `static::` resolve to the lexically enclosing class
            // name in TS. Outside a class body the user has a parse-time
            // error case; if we somehow get here, fall back to `self` /
            // `static` verbatim — TS will reject it loudly.
            let name = ctx
                .current_class
                .as_deref()
                .unwrap_or(if matches!(expr, Expr::SelfRef) {
                    "self"
                } else {
                    "static"
                });
            out.push_str(name);
        }
        Expr::ParentRef => out.push_str("super"),
        Expr::Match { scrutinee, arms } => {
            emit_match(scrutinee, arms, ctx, out);
        }
        Expr::Ternary { cond, then, else_ } => {
            emit_ternary_operand(cond, ctx, out);
            out.push_str(" ? ");
            emit_ternary_operand(then, ctx, out);
            out.push_str(" : ");
            emit_ternary_operand(else_, ctx, out);
        }
        Expr::ShortTernary { cond, else_ } => {
            // PHP `$a ?: $b` returns `$a` when truthy else `$b`. JS `||` has
            // the same shape (returns lhs if truthy else rhs).
            emit_ternary_operand(cond, ctx, out);
            out.push_str(" || ");
            emit_ternary_operand(else_, ctx, out);
        }
        Expr::Assign { target, value } => {
            emit_expr(target, ctx, out);
            out.push_str(" = ");
            emit_expr(value, ctx, out);
        }
        Expr::CompoundAssign { op, target, value } => {
            emit_expr(target, ctx, out);
            out.push(' ');
            out.push_str(compound_op_str(*op));
            out.push(' ');
            emit_expr(value, ctx, out);
        }
        Expr::Binary { op, lhs, rhs } => {
            emit_binary(*op, lhs, rhs, ctx, out);
        }
        Expr::Unary { op, expr } => {
            out.push(match op {
                UnOp::Neg => '-',
                UnOp::Pos => '+',
            });
            emit_unary_operand(expr, ctx, out);
        }
        Expr::Await(inner) => {
            out.push_str("await ");
            emit_unary_operand(inner, ctx, out);
        }
        Expr::FirstClassCallable(target) => {
            emit_first_class_callable(target, ctx, out);
        }
        Expr::IncDec { op, fix, target } => {
            let op_s = match op {
                IncDecOp::Inc => "++",
                IncDecOp::Dec => "--",
            };
            match fix {
                IncDecFix::Prefix => {
                    out.push_str(op_s);
                    emit_expr(target, ctx, out);
                }
                IncDecFix::Postfix => {
                    emit_expr(target, ctx, out);
                    out.push_str(op_s);
                }
            }
        }
        Expr::ArrowFn {
            params,
            return_type,
            body,
        } => {
            out.push('(');
            for (i, p) in params.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&p.name);
                if let Some(t) = &p.ty {
                    out.push_str(": ");
                    emit_type_in(t, ctx.current_class.as_deref(), out);
                }
            }
            out.push(')');
            if let Some(rt) = return_type {
                out.push_str(": ");
                emit_type_in(rt, ctx.current_class.as_deref(), out);
            }
            out.push_str(" => ");
            // Arrow-fn body executes in its own param scope; emit with a
            // fresh `declared` set so it doesn't accidentally inherit
            // outer-scope let-tracking.
            let mut inner = Ctx {
                current_class: ctx.current_class.clone(),
                module_namespace: ctx.module_namespace.clone(),
                indent: ctx.indent,
                ..Ctx::default()
            };
            for p in params {
                inner.declared.insert(p.name.clone());
            }
            emit_expr(body, &mut inner, out);
        }
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Pow => "**",
        // PHPScript `.` is JS `+`. Type safety is enforced by tsc, not us.
        BinOp::Concat => "+",
        BinOp::Eq => "===",
        BinOp::NotEq => "!==",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::LtEq => "<=",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Coalesce => "??",
        BinOp::Instanceof => "instanceof",
        // Spaceship has no direct JS operator — special-cased in
        // `emit_binary`. The string here is unreachable; kept defensive.
        BinOp::Spaceship => "<=>",
    }
}

/// Precedence used by the emitter to decide parenthesisation. We use the
/// same scale as the parser (modern PHP), but careful: PHPScript `.` emits
/// as JS `+`, which in JS has the SAME precedence as `+`/`-` (10), not `.`'s
/// PHP-side level (9). To stay safe we treat `Concat` as TS-precedence 10
/// for output decisions, identical to Add/Sub. That way nested concat with
/// `+`/`-` parenthesises correctly on the TS side.
fn binop_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Pow => 12,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 11,
        BinOp::Add | BinOp::Sub | BinOp::Concat => 10,
        BinOp::Instanceof => 13,
        BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => 7,
        BinOp::Eq | BinOp::NotEq | BinOp::Spaceship => 6,
        BinOp::And => 3,
        BinOp::Or => 2,
        BinOp::Coalesce => 1,
    }
}

fn binop_right_assoc(op: BinOp) -> bool {
    matches!(op, BinOp::Pow | BinOp::Coalesce)
}

fn compound_op_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add | BinOp::Concat => "+=",
        BinOp::Sub => "-=",
        BinOp::Mul => "*=",
        BinOp::Div => "/=",
        BinOp::Rem => "%=",
        BinOp::Pow => "**=",
        BinOp::Coalesce => "??=",
        // Comparison/logical ops have no compound form in PHP or JS — the
        // parser never produces a CompoundAssign with these.
        BinOp::Eq
        | BinOp::NotEq
        | BinOp::Lt
        | BinOp::Gt
        | BinOp::LtEq
        | BinOp::GtEq
        | BinOp::And
        | BinOp::Or
        | BinOp::Instanceof
        | BinOp::Spaceship => {
            unreachable!("non-arithmetic op in compound assignment")
        }
    }
}

fn emit_binary(op: BinOp, lhs: &Expr, rhs: &Expr, ctx: &mut Ctx, out: &mut String) {
    // `<=>` has no JS counterpart. Emit as a single-eval IIFE so operands
    // with side effects (e.g. `foo() <=> bar()`) are evaluated exactly
    // once, and we don't need a runtime helper.
    if matches!(op, BinOp::Spaceship) {
        out.push_str("((__l, __r) => __l < __r ? -1 : __l > __r ? 1 : 0)(");
        emit_expr(lhs, ctx, out);
        out.push_str(", ");
        emit_expr(rhs, ctx, out);
        out.push(')');
        return;
    }
    emit_binary_operand(lhs, op, /*on_right=*/ false, ctx, out);
    out.push(' ');
    out.push_str(binop_str(op));
    out.push(' ');
    // For `instanceof`, the RHS is conceptually a class reference. The
    // parser produced it as an `Ident`/Access; emit verbatim.
    emit_binary_operand(rhs, op, /*on_right=*/ true, ctx, out);
}

/// Emit a PHP 8.1 first-class callable (`target(...)`).
///
/// - Bare function / variable / static method: emit verbatim. JS functions
///   are first-class; `Foo.bar` doesn't need rebinding (statics don't use
///   `this`); a variable already holds a callable.
/// - Instance method (`$obj->m(...)`): JS strips `this` when you write
///   `obj.m`, so we explicitly bind. If `obj` is pure (Var/Ident), emit
///   the bare `obj.m.bind(obj)` form. Otherwise wrap in an IIFE so the
///   non-pure target is evaluated exactly once.
fn emit_first_class_callable(target: &Expr, ctx: &mut Ctx, out: &mut String) {
    match target {
        // `$obj->method(...)` — instance method needs binding.
        Expr::Access {
            target: obj,
            name,
            op: AccessOp::Arrow,
        } => {
            let pure = matches!(obj.as_ref(), Expr::Var(_) | Expr::Ident(_));
            if pure {
                emit_expr(obj, ctx, out);
                out.push('.');
                out.push_str(name);
                out.push_str(".bind(");
                emit_expr(obj, ctx, out);
                out.push(')');
            } else {
                out.push_str("((__o) => __o.");
                out.push_str(name);
                out.push_str(".bind(__o))(");
                emit_expr(obj, ctx, out);
                out.push(')');
            }
        }
        // `Foo::bar(...)` — static method, no binding needed.
        // Bare `strlen(...)`, `$f(...)`, etc. — just the value.
        // Other forms fall through and emit verbatim.
        _ => emit_expr(target, ctx, out),
    }
}

/// Wrap a binary operand in parens if and only if its precedence/associativity
/// would otherwise change the parse on the TS side.
fn emit_binary_operand(
    operand: &Expr,
    parent_op: BinOp,
    on_right: bool,
    ctx: &mut Ctx,
    out: &mut String,
) {
    let parent_prec = binop_prec(parent_op);
    let parent_right_assoc = binop_right_assoc(parent_op);
    if let Expr::Binary { op: child_op, .. } = operand {
        // Spaceship emits as a JS call expression (an IIFE) — call-precedence
        // is tighter than every binary, so no parens are ever needed when
        // it appears inside another binary.
        if matches!(child_op, BinOp::Spaceship) {
            emit_expr(operand, ctx, out);
            return;
        }
        let child_prec = binop_prec(*child_op);
        let needs_parens = child_prec < parent_prec
            || (child_prec == parent_prec
                && ((parent_right_assoc && !on_right) || (!parent_right_assoc && on_right)));
        if needs_parens {
            out.push('(');
            emit_expr(operand, ctx, out);
            out.push(')');
            return;
        }
    }
    emit_expr(operand, ctx, out);
}

/// Wrap a ternary operand in parens when it's another ternary or assignment.
/// Other expressions emit as-is.
fn emit_ternary_operand(operand: &Expr, ctx: &mut Ctx, out: &mut String) {
    match operand {
        Expr::Ternary { .. }
        | Expr::ShortTernary { .. }
        | Expr::Assign { .. }
        | Expr::CompoundAssign { .. } => {
            out.push('(');
            emit_expr(operand, ctx, out);
            out.push(')');
        }
        _ => emit_expr(operand, ctx, out),
    }
}

/// Emit `try { body } catch (...) { ... } finally { ... }`. PHP supports
/// multiple typed catches; JS has only one catch parameter, so we lower
/// the dispatch into an `if instanceof` chain over a synthesized binding
/// `__e`. Each PHP catch's binding (if any) is aliased inside its arm so
/// user code referring to `$e` keeps working.
fn emit_try(
    body: &[Stmt],
    catches: &[Catch],
    finally: Option<&[Stmt]>,
    ctx: &mut Ctx,
    out: &mut String,
) {
    out.push_str("try {\n");
    ctx.indent += 1;
    for s in body {
        write_indent(out, ctx.indent);
        emit_stmt(s, ctx, out);
        out.push('\n');
    }
    ctx.indent -= 1;
    write_indent(out, ctx.indent);
    out.push('}');

    if !catches.is_empty() {
        out.push_str(" catch (__e: unknown) {\n");
        ctx.indent += 1;
        for (i, c) in catches.iter().enumerate() {
            write_indent(out, ctx.indent);
            if i > 0 {
                out.push_str("} else ");
            }
            out.push_str("if (");
            for (j, t) in c.types.iter().enumerate() {
                if j > 0 {
                    out.push_str(" || ");
                }
                out.push_str("__e instanceof ");
                emit_type_in(t, ctx.current_class.as_deref(), out);
            }
            out.push_str(") {\n");
            ctx.indent += 1;
            // Alias the user-named catch var to __e so body code reads
            // `$e` correctly.
            if let Some(name) = &c.var {
                if name != "__e" {
                    write_indent(out, ctx.indent);
                    out.push_str("let ");
                    out.push_str(name);
                    out.push_str(" = __e;\n");
                }
            }
            for s in &c.body {
                write_indent(out, ctx.indent);
                emit_stmt(s, ctx, out);
                out.push('\n');
            }
            ctx.indent -= 1;
        }
        // Re-throw if no catch matched.
        write_indent(out, ctx.indent);
        out.push_str("} else {\n");
        ctx.indent += 1;
        write_indent(out, ctx.indent);
        out.push_str("throw __e;\n");
        ctx.indent -= 1;
        write_indent(out, ctx.indent);
        out.push_str("}\n");
        ctx.indent -= 1;
        write_indent(out, ctx.indent);
        out.push('}');
    }

    if let Some(f) = finally {
        out.push_str(" finally {\n");
        ctx.indent += 1;
        for s in f {
            write_indent(out, ctx.indent);
            emit_stmt(s, ctx, out);
            out.push('\n');
        }
        ctx.indent -= 1;
        write_indent(out, ctx.indent);
        out.push('}');
    }
}

/// Emit a `match` expression as a single-eval IIFE. The scrutinee is bound
/// once to a parameter so it's not re-evaluated for every arm.
fn emit_match(scrutinee: &Expr, arms: &[MatchArm], ctx: &mut Ctx, out: &mut String) {
    out.push_str("((__m) => { ");
    let mut has_default = false;
    for arm in arms {
        match &arm.conds {
            None => {
                has_default = true;
                out.push_str("return ");
                emit_expr(&arm.body, ctx, out);
                out.push_str("; ");
            }
            Some(cs) => {
                out.push_str("if (");
                for (i, c) in cs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" || ");
                    }
                    out.push_str("__m === ");
                    emit_expr(c, ctx, out);
                }
                out.push_str(") return ");
                emit_expr(&arm.body, ctx, out);
                out.push_str("; ");
            }
        }
    }
    if !has_default {
        // PHP throws UnhandledMatchError when no arm matches and no default.
        // Mirror that with a JS Error.
        out.push_str("throw new Error(\"Unhandled match value: \" + String(__m)); ");
    }
    out.push_str("})(");
    emit_expr(scrutinee, ctx, out);
    out.push(')');
}

fn emit_unary_operand(operand: &Expr, ctx: &mut Ctx, out: &mut String) {
    // Unary binds tighter than every binary, so any binary child must be parenthesised.
    if matches!(operand, Expr::Binary { .. } | Expr::Assign { .. }) {
        out.push('(');
        emit_expr(operand, ctx, out);
        out.push(')');
    } else {
        emit_expr(operand, ctx, out);
    }
}

/// Emit an interpolated string as a TS template literal — `` `lit${expr}lit` ``.
/// Literal segments need backtick and `${` escaping since both have special
/// meaning inside template literals.
fn emit_interpolated_string(parts: &[InterpolatedPart], ctx: &mut Ctx, out: &mut String) {
    out.push('`');
    for part in parts {
        match part {
            InterpolatedPart::Lit(s) => emit_template_literal_text(s, out),
            InterpolatedPart::Expr(expr) => {
                out.push_str("${");
                emit_expr(expr, ctx, out);
                out.push('}');
            }
        }
    }
    out.push('`');
}

/// Escape a literal text segment for use inside a TS template literal.
/// The two characters that need attention are the backtick (which would
/// terminate the literal) and `$` followed by `{` (which would start a
/// substitution). Other chars round-trip verbatim, including newlines —
/// template literals preserve them.
fn emit_template_literal_text(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'`' {
            out.push_str("\\`");
            i += 1;
        } else if b == b'\\' {
            out.push_str("\\\\");
            i += 1;
        } else if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            out.push_str("\\${");
            i += 2;
        } else {
            // Slice-copy the longest run of safe chars to keep multi-byte
            // UTF-8 sequences intact.
            let start = i;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'`'
                    || c == b'\\'
                    || (c == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{')
                {
                    break;
                }
                i += 1;
            }
            out.push_str(&s[start..i]);
        }
    }
}

/// Emit a string as a TypeScript double-quoted literal with the minimum
/// escapes required.
fn emit_string_literal(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_ast::Span;

    #[test]
    fn emits_empty_string_for_empty_module() {
        assert_eq!(emit(&Module::empty()), "");
    }

    #[test]
    fn emits_integer_expression_statement() {
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Int(42), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"42;");
    }

    #[test]
    fn emits_multiple_statements_with_newlines() {
        let m = Module {
            stmts: vec![
                Stmt::Expr(Expr::Int(1), Span::DUMMY),
                Stmt::Expr(Expr::Int(2), Span::DUMMY),
                Stmt::Expr(Expr::Int(3), Span::DUMMY),
            ],
        };
        insta::assert_snapshot!(emit(&m), @"
        1;
        2;
        3;
        ");
    }

    #[test]
    fn emits_float_literal() {
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Float(3.14), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"3.14;");
    }

    #[test]
    fn emits_integral_float_with_explicit_dot() {
        // `42.0` (Float, not Int) should round-trip with a fractional part so
        // the TS reader still sees "this was a float". Otherwise `42.` lossy.
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Float(42.0), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"42.0;");
    }

    #[test]
    fn emits_string_literal_with_escapes() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Str("hi \"there\"\n\t".into()),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#""hi \"there\"\n\t";"#);
    }

    #[test]
    fn emits_bool_literals() {
        let m = Module {
            stmts: vec![
                Stmt::Expr(Expr::Bool(true), Span::DUMMY),
                Stmt::Expr(Expr::Bool(false), Span::DUMMY),
            ],
        };
        insta::assert_snapshot!(emit(&m), @"
        true;
        false;
        ");
    }

    #[test]
    fn emits_null_literal() {
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Null, Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"null;");
    }

    #[test]
    fn emits_variable_drops_dollar_sigil() {
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Var("name".into()), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"name;");
    }

    fn assign_stmt(name: &str, value: Expr) -> Stmt {
        Stmt::Expr(
            Expr::Assign {
                target: Box::new(Expr::Var(name.into())),
                value: Box::new(value),
            },
            Span::DUMMY,
        )
    }

    #[test]
    fn first_assignment_to_a_variable_emits_let() {
        let m = Module {
            stmts: vec![assign_stmt("x", Expr::Int(42))],
        };
        insta::assert_snapshot!(emit(&m), @"let x = 42;");
    }

    #[test]
    fn reassignment_emits_plain_assignment() {
        let m = Module {
            stmts: vec![
                assign_stmt("x", Expr::Int(1)),
                assign_stmt("x", Expr::Int(2)),
            ],
        };
        insta::assert_snapshot!(emit(&m), @"
        let x = 1;
        x = 2;
        ");
    }

    /// `$a = $b = 1;` — outer assignment to `a` is a declaration; the inner
    /// assignment to `b` is nested inside the value, so it stays as a
    /// non-declaring expression. The reader gets `let a = b = 1;`, which is
    /// valid TS *iff* `b` is already declared. We accept this gap for the
    /// MVP — the design's scope-tracking story tightens here in a future
    /// pass.
    #[test]
    fn chained_assignment_lifts_only_the_outermost() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("a".into())),
                    value: Box::new(Expr::Assign {
                        target: Box::new(Expr::Var("b".into())),
                        value: Box::new(Expr::Int(1)),
                    }),
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"let a = b = 1;");
    }

    fn binp(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Binary {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        }
    }

    #[test]
    fn emits_simple_addition_no_parens_needed() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(BinOp::Add, Expr::Int(1), Expr::Int(2)),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"1 + 2;");
    }

    /// `1 + 2 * 3` -> `1 + 2 * 3` (no parens — TS precedence matches).
    #[test]
    fn emits_mul_inside_add_without_parens() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Add,
                    Expr::Int(1),
                    binp(BinOp::Mul, Expr::Int(2), Expr::Int(3)),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"1 + 2 * 3;");
    }

    /// `(1 + 2) * 3` AST -> emit must parenthesize the sum to preserve meaning.
    #[test]
    fn emits_add_inside_mul_with_parens() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Mul,
                    binp(BinOp::Add, Expr::Int(1), Expr::Int(2)),
                    Expr::Int(3),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"(1 + 2) * 3;");
    }

    /// Left-associative subtract: `(1 - 2) - 3` AST -> emit as `1 - 2 - 3` (no
    /// parens needed; TS reads it the same way left-to-right).
    #[test]
    fn left_associative_chain_no_parens_on_left_branch() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Sub,
                    binp(BinOp::Sub, Expr::Int(1), Expr::Int(2)),
                    Expr::Int(3),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"1 - 2 - 3;");
    }

    /// `1 - (2 - 3)` AST -> must keep parens to preserve meaning since `-` is
    /// left-associative.
    #[test]
    fn left_associative_chain_parens_needed_on_right_branch() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Sub,
                    Expr::Int(1),
                    binp(BinOp::Sub, Expr::Int(2), Expr::Int(3)),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"1 - (2 - 3);");
    }

    /// `2 ** 3 ** 2` AST is right-associative -> emit without parens (TS `**`
    /// matches).
    #[test]
    fn right_associative_pow_no_parens_on_right_branch() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Pow,
                    Expr::Int(2),
                    binp(BinOp::Pow, Expr::Int(3), Expr::Int(2)),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"2 ** 3 ** 2;");
    }

    #[test]
    fn emits_unary_minus() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(Expr::Int(7)),
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"-7;");
    }

    #[test]
    fn unary_over_binary_parenthesises_the_inner_binary() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(binp(BinOp::Add, Expr::Int(1), Expr::Int(2))),
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"-(1 + 2);");
    }

    // ---------- concat / comparison / logical / coalesce ----------

    fn s(text: &str) -> Expr {
        Expr::Str(text.into())
    }

    #[test]
    fn emits_concat_as_plus() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(BinOp::Concat, s("hi "), s("there")),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#""hi " + "there";"#);
    }

    #[test]
    fn emits_strict_equality() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(BinOp::Eq, Expr::Int(1), Expr::Int(2)),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"1 === 2;");
    }

    #[test]
    fn emits_strict_inequality() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(BinOp::NotEq, Expr::Int(1), Expr::Int(2)),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"1 !== 2;");
    }

    #[test]
    fn emits_logical_and_or() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Or,
                    Expr::Var("a".into()),
                    binp(BinOp::And, Expr::Var("b".into()), Expr::Var("c".into())),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"a || b && c;");
    }

    #[test]
    fn emits_coalesce_right_associative_no_parens() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                binp(
                    BinOp::Coalesce,
                    Expr::Var("a".into()),
                    binp(
                        BinOp::Coalesce,
                        Expr::Var("b".into()),
                        Expr::Var("c".into()),
                    ),
                ),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"a ?? b ?? c;");
    }

    // ---------- compound assignment ----------

    fn comp(op: BinOp, name: &str, value: Expr) -> Stmt {
        Stmt::Expr(
            Expr::CompoundAssign {
                op,
                target: Box::new(Expr::Var(name.into())),
                value: Box::new(value),
            },
            Span::DUMMY,
        )
    }

    #[test]
    fn emits_plus_equals() {
        let m = Module {
            stmts: vec![comp(BinOp::Add, "x", Expr::Int(1))],
        };
        insta::assert_snapshot!(emit(&m), @"x += 1;");
    }

    #[test]
    fn emits_dot_equals_as_plus_equals() {
        let m = Module {
            stmts: vec![comp(BinOp::Concat, "s", s("x"))],
        };
        insta::assert_snapshot!(emit(&m), @r#"s += "x";"#);
    }

    #[test]
    fn emits_coalesce_equals() {
        let m = Module {
            stmts: vec![comp(BinOp::Coalesce, "x", Expr::Int(1))],
        };
        insta::assert_snapshot!(emit(&m), @"x ??= 1;");
    }

    /// Compound assignment never lifts to `let` — even on a name that hasn't
    /// been seen. The user is expected to declare the variable first.
    #[test]
    fn compound_assignment_does_not_emit_let_for_undeclared_target() {
        let m = Module {
            stmts: vec![comp(BinOp::Add, "fresh", Expr::Int(5))],
        };
        insta::assert_snapshot!(emit(&m), @"fresh += 5;");
    }

    // ---------- blocks + if/else ----------

    #[test]
    fn emits_empty_block() {
        let m = Module {
            stmts: vec![Stmt::Block(vec![], Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"{}");
    }

    #[test]
    fn emits_block_with_indentation() {
        let m = Module {
            stmts: vec![Stmt::Block(
                vec![
                    Stmt::Expr(Expr::Int(1), Span::DUMMY),
                    Stmt::Expr(Expr::Int(2), Span::DUMMY),
                ],
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r"
        {
          1;
          2;
        }
        ");
    }

    #[test]
    fn emits_if_with_block() {
        let m = Module {
            stmts: vec![Stmt::If {
                cond: Expr::Var("x".into()),
                then: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Int(1), Span::DUMMY)],
                    Span::DUMMY,
                )),
                else_: None,
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @r"
        if (x) {
          1;
        }
        ");
    }

    #[test]
    fn emits_if_else_chain() {
        // if (a) { 1; } else { 2; }
        let m = Module {
            stmts: vec![Stmt::If {
                cond: Expr::Var("a".into()),
                then: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Int(1), Span::DUMMY)],
                    Span::DUMMY,
                )),
                else_: Some(Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Int(2), Span::DUMMY)],
                    Span::DUMMY,
                ))),
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @r"
        if (a) {
          1;
        } else {
          2;
        }
        ");
    }

    #[test]
    fn emits_else_if_chain_inline_no_extra_indent() {
        // if (a) { 1; } else if (b) { 2; } else { 3; }
        let inner_if = Stmt::If {
            cond: Expr::Var("b".into()),
            then: Box::new(Stmt::Block(
                vec![Stmt::Expr(Expr::Int(2), Span::DUMMY)],
                Span::DUMMY,
            )),
            else_: Some(Box::new(Stmt::Block(
                vec![Stmt::Expr(Expr::Int(3), Span::DUMMY)],
                Span::DUMMY,
            ))),
            span: Span::DUMMY,
        };
        let m = Module {
            stmts: vec![Stmt::If {
                cond: Expr::Var("a".into()),
                then: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Int(1), Span::DUMMY)],
                    Span::DUMMY,
                )),
                else_: Some(Box::new(inner_if)),
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @r"
        if (a) {
          1;
        } else if (b) {
          2;
        } else {
          3;
        }
        ");
    }

    /// Nested blocks indent properly.
    #[test]
    fn nested_blocks_increment_indent() {
        let inner = Stmt::Block(vec![Stmt::Expr(Expr::Int(7), Span::DUMMY)], Span::DUMMY);
        let outer = Stmt::Block(vec![inner], Span::DUMMY);
        let m = Module { stmts: vec![outer] };
        insta::assert_snapshot!(emit(&m), @r"
        {
          {
            7;
          }
        }
        ");
    }

    /// `if (cond) <single-stmt>` (no braces) emits inline.
    #[test]
    fn if_with_single_unbraced_body_inline() {
        let m = Module {
            stmts: vec![Stmt::If {
                cond: Expr::Var("x".into()),
                then: Box::new(Stmt::Expr(Expr::Int(42), Span::DUMMY)),
                else_: None,
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @"if (x) 42;");
    }

    // ---------- return / while / foreach ----------

    #[test]
    fn emits_bare_return() {
        let m = Module {
            stmts: vec![Stmt::Return(None, Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"return;");
    }

    #[test]
    fn emits_return_with_value() {
        let m = Module {
            stmts: vec![Stmt::Return(Some(Expr::Int(7)), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"return 7;");
    }

    #[test]
    fn emits_while_with_block_body() {
        let m = Module {
            stmts: vec![Stmt::While {
                cond: Expr::Var("x".into()),
                body: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Var("x".into()), Span::DUMMY)],
                    Span::DUMMY,
                )),
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @r"
        while (x) {
          x;
        }
        ");
    }

    #[test]
    fn emits_foreach_value_only_as_for_of() {
        let m = Module {
            stmts: vec![Stmt::Foreach {
                iter: Expr::Var("items".into()),
                key: None,
                value: "item".into(),
                body: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Var("item".into()), Span::DUMMY)],
                    Span::DUMMY,
                )),
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @r"
        for (const item of items) {
          item;
        }
        ");
    }

    #[test]
    fn emits_foreach_key_value_via_object_entries() {
        let m = Module {
            stmts: vec![Stmt::Foreach {
                iter: Expr::Var("items".into()),
                key: Some("k".into()),
                value: "v".into(),
                body: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Var("v".into()), Span::DUMMY)],
                    Span::DUMMY,
                )),
                span: Span::DUMMY,
            }],
        };
        insta::assert_snapshot!(emit(&m), @r"
        for (const [k, v] of Object.entries(items)) {
          v;
        }
        ");
    }

    // ---------- identifiers + calls ----------

    #[test]
    fn emits_bare_identifier() {
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Ident("undefined".into()), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"undefined;");
    }

    #[test]
    fn emits_call_with_args() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(Expr::Ident("greet".into())),
                    args: vec![s("hi"), Expr::Int(2)],
                    span: Span::DUMMY,
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#"greet("hi", 2);"#);
    }

    #[test]
    fn emits_call_with_no_args() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(Expr::Ident("now".into())),
                    args: vec![],
                    span: Span::DUMMY,
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"now();");
    }

    #[test]
    fn emits_chained_call_no_extra_parens() {
        let inner = Expr::Call {
            callee: Box::new(Expr::Ident("f".into())),
            args: vec![],
            span: Span::DUMMY,
        };
        let chained = Expr::Call {
            callee: Box::new(inner),
            args: vec![Expr::Int(1)],
            span: Span::DUMMY,
        };
        let m = Module {
            stmts: vec![Stmt::Expr(chained, Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"f()(1);");
    }

    // ---------- type emit ----------

    fn check_type_emit(t: TypeAnn, expected: &str) {
        let mut out = String::new();
        emit_type(&t, &mut out);
        assert_eq!(out, expected);
    }

    #[test]
    fn emits_named_types_via_mapping_table() {
        check_type_emit(TypeAnn::Named("int".into()), "number");
        check_type_emit(TypeAnn::Named("float".into()), "number");
        check_type_emit(TypeAnn::Named("bool".into()), "boolean");
        check_type_emit(TypeAnn::Named("string".into()), "string");
        check_type_emit(TypeAnn::Named("void".into()), "void");
        check_type_emit(TypeAnn::Named("null".into()), "null");
        check_type_emit(TypeAnn::Named("mixed".into()), "any");
        check_type_emit(TypeAnn::Named("never".into()), "never");
        check_type_emit(TypeAnn::Named("MyClass".into()), "MyClass");
    }

    #[test]
    fn emits_nullable_as_pipe_null() {
        check_type_emit(
            TypeAnn::Nullable(Box::new(TypeAnn::Named("Foo".into()))),
            "Foo | null",
        );
    }

    #[test]
    fn emits_union_as_pipe_separated() {
        check_type_emit(
            TypeAnn::Union(vec![
                TypeAnn::Named("int".into()),
                TypeAnn::Named("string".into()),
            ]),
            "number | string",
        );
    }

    #[test]
    fn emits_array_t_as_t_brackets() {
        check_type_emit(
            TypeAnn::Generic("array".into(), vec![TypeAnn::Named("int".into())]),
            "number[]",
        );
    }

    #[test]
    fn emits_array_k_v_as_record() {
        check_type_emit(
            TypeAnn::Generic(
                "array".into(),
                vec![
                    TypeAnn::Named("string".into()),
                    TypeAnn::Named("User".into()),
                ],
            ),
            "Record<string, User>",
        );
    }

    #[test]
    fn emits_arbitrary_generic_unchanged() {
        check_type_emit(
            TypeAnn::Generic(
                "Result".into(),
                vec![
                    TypeAnn::Named("User".into()),
                    TypeAnn::Named("Error".into()),
                ],
            ),
            "Result<User, Error>",
        );
    }

    // ---------- function declaration emit ----------

    #[test]
    fn emits_empty_function() {
        let m = Module {
            stmts: vec![Stmt::Function(FunctionDecl {
                name: "noop".into(),
                params: vec![],
                return_type: None,
                body: vec![],
                async_: false,
                span: Span::DUMMY,
            })],
        };
        insta::assert_snapshot!(emit(&m), @"function noop() {}");
    }

    #[test]
    fn emits_function_with_params_and_types() {
        let m = Module {
            stmts: vec![Stmt::Function(FunctionDecl {
                name: "add".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: Some(TypeAnn::Named("int".into())),
                        default: None,
                        promotion: None,
                    },
                    Param {
                        name: "b".into(),
                        ty: Some(TypeAnn::Named("int".into())),
                        default: None,
                        promotion: None,
                    },
                ],
                return_type: Some(TypeAnn::Named("int".into())),
                async_: false,
                body: vec![Stmt::Return(
                    Some(binp(
                        BinOp::Add,
                        Expr::Var("a".into()),
                        Expr::Var("b".into()),
                    )),
                    Span::DUMMY,
                )],
                span: Span::DUMMY,
            })],
        };
        insta::assert_snapshot!(emit(&m), @r"
        function add(a: number, b: number): number {
          return a + b;
        }
        ");
    }

    /// Function body has its own scope: a local `$x` doesn't lift to the
    /// outer hoist set.
    #[test]
    fn function_body_has_its_own_scope() {
        let m = Module {
            stmts: vec![Stmt::Function(FunctionDecl {
                name: "f".into(),
                params: vec![],
                return_type: None,
                async_: false,
                body: vec![Stmt::Expr(
                    Expr::Assign {
                        target: Box::new(Expr::Var("local".into())),
                        value: Box::new(Expr::Int(1)),
                    },
                    Span::DUMMY,
                )],
                span: Span::DUMMY,
            })],
        };
        insta::assert_snapshot!(emit(&m), @r"
        function f() {
          let local = 1;
        }
        ");
    }

    /// Parameters are pre-declared, so reassignment in the body doesn't
    /// produce a `let`.
    #[test]
    fn parameter_reassignment_does_not_emit_let() {
        let m = Module {
            stmts: vec![Stmt::Function(FunctionDecl {
                name: "bump".into(),
                params: vec![Param {
                    name: "n".into(),
                    ty: Some(TypeAnn::Named("int".into())),
                    default: None,
                    promotion: None,
                }],
                return_type: Some(TypeAnn::Named("int".into())),
                async_: false,
                body: vec![
                    Stmt::Expr(
                        Expr::Assign {
                            target: Box::new(Expr::Var("n".into())),
                            value: Box::new(binp(BinOp::Add, Expr::Var("n".into()), Expr::Int(1))),
                        },
                        Span::DUMMY,
                    ),
                    Stmt::Return(Some(Expr::Var("n".into())), Span::DUMMY),
                ],
                span: Span::DUMMY,
            })],
        };
        insta::assert_snapshot!(emit(&m), @r"
        function bump(n: number): number {
          n = n + 1;
          return n;
        }
        ");
    }

    // ---------- arrays ----------

    fn unkeyed(values: Vec<Expr>) -> Expr {
        Expr::Array(
            values
                .into_iter()
                .map(|v| ArrayItem {
                    key: None,
                    value: v,
                })
                .collect(),
        )
    }

    fn keyed(items: Vec<(Expr, Expr)>) -> Expr {
        Expr::Array(
            items
                .into_iter()
                .map(|(k, v)| ArrayItem {
                    key: Some(k),
                    value: v,
                })
                .collect(),
        )
    }

    #[test]
    fn emits_empty_array_as_brackets() {
        let m = Module {
            stmts: vec![Stmt::Expr(Expr::Array(vec![]), Span::DUMMY)],
        };
        insta::assert_snapshot!(emit(&m), @"[];");
    }

    #[test]
    fn emits_unkeyed_array_as_js_list() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                unkeyed(vec![Expr::Int(1), Expr::Int(2), Expr::Int(3)]),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"[1, 2, 3];");
    }

    #[test]
    fn emits_keyed_array_with_ident_keys_as_object() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                keyed(vec![(s("name"), s("Ada")), (s("age"), Expr::Int(36))]),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#"{ name: "Ada", age: 36 };"#);
    }

    #[test]
    fn emits_keyed_array_with_non_ident_string_keys_quoted() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                keyed(vec![(s("first-name"), s("Ada"))]),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#"{ "first-name": "Ada" };"#);
    }

    #[test]
    fn emits_index_access() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Index {
                    obj: Box::new(Expr::Var("arr".into())),
                    key: Box::new(Expr::Int(0)),
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"arr[0];");
    }

    #[test]
    fn index_assignment_does_not_lift_to_let() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Index {
                        obj: Box::new(Expr::Var("arr".into())),
                        key: Box::new(Expr::Int(0)),
                    }),
                    value: Box::new(Expr::Int(5)),
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"arr[0] = 5;");
    }

    // ---------- member access ----------

    fn acc(target: Expr, name: &str, op: AccessOp) -> Expr {
        Expr::Access {
            target: Box::new(target),
            name: name.into(),
            op,
        }
    }

    #[test]
    fn emits_arrow_as_dot() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                acc(Expr::Var("user".into()), "email", AccessOp::Arrow),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"user.email;");
    }

    #[test]
    fn emits_null_safe_arrow_as_optional_chain() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                acc(Expr::Var("user".into()), "email", AccessOp::NullSafeArrow),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"user?.email;");
    }

    #[test]
    fn emits_double_colon_as_dot() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                acc(Expr::Ident("Foo".into()), "bar", AccessOp::DoubleColon),
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"Foo.bar;");
    }

    /// Property write via member access (no let lift on Access targets).
    #[test]
    fn property_write_does_not_lift_to_let() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(acc(Expr::Var("this".into()), "name", AccessOp::Arrow)),
                    value: Box::new(s("Ada")),
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#"this.name = "Ada";"#);
    }

    // ---------- new expression ----------

    #[test]
    fn emits_new_with_no_args() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::New {
                    class: TypeAnn::Named("Foo".into()),
                    args: vec![],
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"new Foo();");
    }

    #[test]
    fn emits_new_with_args() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::New {
                    class: TypeAnn::Named("User".into()),
                    args: vec![s("Ada"), Expr::Int(36)],
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @r#"new User("Ada", 36);"#);
    }

    #[test]
    fn emits_new_with_generic_class() {
        let m = Module {
            stmts: vec![Stmt::Expr(
                Expr::New {
                    class: TypeAnn::Generic("Box".into(), vec![TypeAnn::Named("int".into())]),
                    args: vec![Expr::Int(5)],
                },
                Span::DUMMY,
            )],
        };
        insta::assert_snapshot!(emit(&m), @"new Box<number>(5);");
    }

    // ---------- class skeleton ----------

    fn class_with(members: Vec<ClassMember>) -> Stmt {
        Stmt::Class(Class {
            name: "User".into(),
            abstract_: false,
            final_: false,
            readonly: false,
            extends: None,
            implements: Vec::new(),
            members,
            span: Span::DUMMY,
        })
    }

    #[test]
    fn emits_empty_class() {
        let m = Module {
            stmts: vec![class_with(vec![])],
        };
        insta::assert_snapshot!(emit(&m), @"class User {}");
    }

    #[test]
    fn emits_class_with_property() {
        let m = Module {
            stmts: vec![class_with(vec![ClassMember::Property(Property {
                visibility: Visibility::Public,
                set_visibility: None,
                readonly: false,
                static_: false,
                ty: Some(TypeAnn::Named("string".into())),
                name: "name".into(),
                default: None,
                hooks: None,
            })])],
        };
        insta::assert_snapshot!(emit(&m), @r"
        class User {
          public name: string;
        }
        ");
    }

    #[test]
    fn emits_class_with_property_default() {
        let m = Module {
            stmts: vec![class_with(vec![ClassMember::Property(Property {
                visibility: Visibility::Private,
                set_visibility: None,
                readonly: false,
                static_: false,
                ty: Some(TypeAnn::Named("int".into())),
                name: "count".into(),
                default: Some(Expr::Int(0)),
                hooks: None,
            })])],
        };
        insta::assert_snapshot!(emit(&m), @r"
        class User {
          private count: number = 0;
        }
        ");
    }

    #[test]
    fn emits_class_with_method() {
        let m = Module {
            stmts: vec![class_with(vec![ClassMember::Method(Method {
                visibility: Visibility::Public,
                static_: false,
                abstract_: false,
                final_: false,
                async_: false,
                name: "greet".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: Some(TypeAnn::Named("string".into())),
                    default: None,
                    promotion: None,
                }],
                return_type: Some(TypeAnn::Named("string".into())),
                body: Some(vec![Stmt::Return(
                    Some(Expr::Var("name".into())),
                    Span::DUMMY,
                )]),
            })])],
        };
        insta::assert_snapshot!(emit(&m), @r"
        class User {
          public greet(name: string): string {
            return name;
          }
        }
        ");
    }

    // ---------- constructor property promotion ----------

    fn promoted_param(name: &str, ty_name: &str, vis: Visibility, readonly: bool) -> Param {
        Param {
            name: name.into(),
            ty: Some(TypeAnn::Named(ty_name.into())),
            default: None,
            promotion: Some(Promotion {
                visibility: vis,
                set_visibility: None,
                readonly,
            }),
        }
    }

    #[test]
    fn emits_promoted_param_as_ts_parameter_property() {
        let m = Module {
            stmts: vec![class_with(vec![ClassMember::Method(Method {
                visibility: Visibility::Public,
                static_: false,
                abstract_: false,
                final_: false,
                name: "__construct".into(),
                params: vec![promoted_param("name", "string", Visibility::Public, false)],
                return_type: None,
                body: Some(vec![]),
                async_: false,
            })])],
        };
        insta::assert_snapshot!(emit(&m), @r"
        class User {
          public constructor(public name: string) {}
        }
        ");
    }

    #[test]
    fn emits_readonly_promoted_param() {
        let m = Module {
            stmts: vec![class_with(vec![ClassMember::Method(Method {
                visibility: Visibility::Public,
                static_: false,
                abstract_: false,
                final_: false,
                name: "__construct".into(),
                params: vec![promoted_param("id", "string", Visibility::Public, true)],
                return_type: None,
                body: Some(vec![]),
                async_: false,
            })])],
        };
        insta::assert_snapshot!(emit(&m), @r"
        class User {
          public constructor(public readonly id: string) {}
        }
        ");
    }

    /// `__construct` becomes `constructor` and loses any declared return type.
    #[test]
    fn emits_constructor_with_renamed_keyword() {
        let m = Module {
            stmts: vec![class_with(vec![ClassMember::Method(Method {
                visibility: Visibility::Public,
                static_: false,
                abstract_: false,
                final_: false,
                async_: false,
                name: "__construct".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: Some(TypeAnn::Named("string".into())),
                    default: None,
                    promotion: None,
                }],
                return_type: Some(TypeAnn::Named("void".into())),
                body: Some(vec![Stmt::Expr(
                    Expr::Assign {
                        target: Box::new(acc(Expr::Var("this".into()), "name", AccessOp::Arrow)),
                        value: Box::new(Expr::Var("name".into())),
                    },
                    Span::DUMMY,
                )]),
            })])],
        };
        insta::assert_snapshot!(emit(&m), @r"
        class User {
          public constructor(name: string) {
            this.name = name;
          }
        }
        ");
    }
}
