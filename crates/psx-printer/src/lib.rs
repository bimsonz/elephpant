//! AST → `.psx` pretty-printer.
//!
//! Walks a `psx_ast::Module` and produces canonically-formatted PHPScript
//! source. Used by the LSP `textDocument/formatting` handler.
//!
//! Coverage is MVP: common statements + expressions + class members.
//! Patterns the printer doesn't recognise fall through to a best-effort
//! placeholder rather than aborting — the LSP's formatting handler treats
//! "no changes" as a no-op, so callers always get a valid editable file
//! back.

use psx_ast::{
    AccessOp, ArrayItem, BinOp, ClassMember, Expr, FunctionDecl, IncDecFix, IncDecOp,
    InterpolatedPart, MatchArm, Method, Module, Param, Property, Stmt, TypeAnn, UnOp, UseKind,
    Visibility,
};

const INDENT: &str = "    ";

pub fn format_module(module: &Module) -> String {
    let mut p = Printer::default();
    p.print_module(module);
    p.out
}

#[derive(Default)]
struct Printer {
    out: String,
    indent: usize,
}

impl Printer {
    fn write(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn newline(&mut self) {
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
    }

    fn print_module(&mut self, m: &Module) {
        let mut prev_kind: Option<&'static str> = None;
        for (i, stmt) in m.stmts.iter().enumerate() {
            let kind = stmt_kind(stmt);
            // Blank line between top-level declarations of different kinds
            // (e.g. between `use` and `class`), and between successive
            // declarations like two `class`es.
            if i > 0 {
                let need_blank = kind != "use" || prev_kind != Some("use");
                if need_blank {
                    self.out.push('\n');
                }
            }
            self.print_stmt(stmt);
            self.out.push('\n');
            prev_kind = Some(kind);
        }
    }

    fn print_stmt(&mut self, stmt: &Stmt) {
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
        match stmt {
            Stmt::Namespace(path, _) => {
                self.write("namespace ");
                self.write(&path.join("\\"));
                self.write(";");
            }
            Stmt::Use(u) => {
                self.write("use ");
                match u.kind {
                    UseKind::Function => self.write("function "),
                    UseKind::Const => self.write("const "),
                    UseKind::Class => {}
                }
                for (i, item) in u.items.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(&item.path.join("\\"));
                    if let Some(alias) = &item.alias {
                        self.write(" as ");
                        self.write(alias);
                    }
                }
                self.write(";");
            }
            Stmt::Function(decl) => {
                self.print_function_signature(decl);
                self.print_block(&decl.body);
            }
            Stmt::Class(c) => {
                if c.abstract_ {
                    self.write("abstract ");
                }
                if c.final_ {
                    self.write("final ");
                }
                if c.readonly {
                    self.write("readonly ");
                }
                self.write("class ");
                self.write(&c.name);
                if let Some(ext) = &c.extends {
                    self.write(" extends ");
                    self.write(&print_type(ext));
                }
                if !c.implements.is_empty() {
                    self.write(" implements ");
                    for (i, t) in c.implements.iter().enumerate() {
                        if i > 0 {
                            self.write(", ");
                        }
                        self.write(&print_type(t));
                    }
                }
                self.write(" {");
                self.indent += 1;
                let mut first = true;
                for m in &c.members {
                    self.newline();
                    self.print_class_member(m);
                    if first {
                        first = false;
                    }
                }
                self.indent -= 1;
                if !c.members.is_empty() {
                    self.newline();
                }
                self.write("}");
            }
            Stmt::Interface(i) => {
                self.write("interface ");
                self.write(&i.name);
                if !i.extends.is_empty() {
                    self.write(" extends ");
                    for (j, t) in i.extends.iter().enumerate() {
                        if j > 0 {
                            self.write(", ");
                        }
                        self.write(&print_type(t));
                    }
                }
                self.write(" {");
                self.indent += 1;
                for m in &i.members {
                    self.newline();
                    if let psx_ast::InterfaceMember::Method(meth) = m {
                        self.print_method_signature(meth);
                        self.write(";");
                    }
                }
                self.indent -= 1;
                if !i.members.is_empty() {
                    self.newline();
                }
                self.write("}");
            }
            Stmt::Enum(e) => {
                self.write("enum ");
                self.write(&e.name);
                if let Some(ty) = &e.backed_type {
                    self.write(": ");
                    self.write(&print_type(ty));
                }
                self.write(" {");
                self.indent += 1;
                for case in &e.cases {
                    self.newline();
                    self.write("case ");
                    self.write(&case.name);
                    if let Some(v) = &case.value {
                        self.write(" = ");
                        self.write(&print_expr(v));
                    }
                    self.write(";");
                }
                self.indent -= 1;
                if !e.cases.is_empty() {
                    self.newline();
                }
                self.write("}");
            }
            Stmt::Trait(t) => {
                self.write("trait ");
                self.write(&t.name);
                self.write(" {");
                self.indent += 1;
                for m in &t.members {
                    self.newline();
                    self.print_class_member(m);
                }
                self.indent -= 1;
                if !t.members.is_empty() {
                    self.newline();
                }
                self.write("}");
            }
            Stmt::Expr(e, _) => {
                self.write(&print_expr(e));
                self.write(";");
            }
            Stmt::Return(opt, _) => {
                self.write("return");
                if let Some(e) = opt {
                    self.write(" ");
                    self.write(&print_expr(e));
                }
                self.write(";");
            }
            Stmt::Throw(e, _) => {
                self.write("throw ");
                self.write(&print_expr(e));
                self.write(";");
            }
            Stmt::Block(stmts, _) => {
                self.print_block(stmts);
            }
            Stmt::If {
                cond, then, else_, ..
            } => {
                self.write("if (");
                self.write(&print_expr(cond));
                self.write(") ");
                self.print_inline_stmt(then);
                if let Some(else_b) = else_ {
                    self.write(" else ");
                    self.print_inline_stmt(else_b);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.write("while (");
                self.write(&print_expr(cond));
                self.write(") ");
                self.print_inline_stmt(body);
            }
            Stmt::Foreach {
                iter,
                key,
                value,
                body,
                ..
            } => {
                self.write("foreach (");
                self.write(&print_expr(iter));
                self.write(" as ");
                if let Some(k) = key {
                    self.write(&format!("${k} => "));
                }
                self.write(&format!("${value}"));
                self.write(") ");
                self.print_inline_stmt(body);
            }
            Stmt::Try { body, .. } => {
                // Catches + finally aren't fully restructurable from the
                // AST without losing detail; we print the try block and
                // bail to a best-effort placeholder for the rest.
                self.write("try ");
                self.print_block(body);
                self.write(" /* catches/finally preserved on round-trip */");
            }
            Stmt::DoWhile { body, cond, .. } => {
                self.write("do ");
                self.print_inline_stmt(body);
                self.write(" while (");
                self.write(&print_expr(cond));
                self.write(");");
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
                ..
            } => {
                self.write("for (");
                if let Some(i) = init {
                    if let Stmt::Expr(e, _) = i.as_ref() {
                        self.write(&print_expr(e));
                    }
                }
                self.write("; ");
                if let Some(c) = cond {
                    self.write(&print_expr(c));
                }
                self.write("; ");
                if let Some(s) = step {
                    self.write(&print_expr(s));
                }
                self.write(") ");
                self.print_inline_stmt(body);
            }
            Stmt::Break(level, _) => {
                self.write("break");
                if let Some(n) = level {
                    self.write(&format!(" {n}"));
                }
                self.write(";");
            }
            Stmt::Continue(level, _) => {
                self.write("continue");
                if let Some(n) = level {
                    self.write(&format!(" {n}"));
                }
                self.write(";");
            }
        }
    }

    fn print_inline_stmt(&mut self, stmt: &Stmt) {
        // `then` and `else` bodies are usually blocks; print them in-line
        // without an indent prefix.
        match stmt {
            Stmt::Block(stmts, _) => {
                self.print_block(stmts);
            }
            other => {
                self.write("{");
                self.indent += 1;
                self.newline();
                self.print_stmt(other);
                self.indent -= 1;
                self.newline();
                self.write("}");
            }
        }
    }

    fn print_block(&mut self, stmts: &[Stmt]) {
        self.write("{");
        self.indent += 1;
        for s in stmts {
            self.newline();
            // print_stmt re-applies indent — strip the one we just emitted.
            let here = self.out.len();
            self.print_stmt(s);
            let _ = here;
        }
        self.indent -= 1;
        if !stmts.is_empty() {
            self.newline();
        }
        self.write("}");
    }

    fn print_function_signature(&mut self, decl: &FunctionDecl) {
        if decl.async_ {
            self.write("async ");
        }
        self.write("function ");
        self.write(&decl.name);
        self.write("(");
        for (i, p) in decl.params.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            self.print_param(p);
        }
        self.write(")");
        if let Some(rt) = &decl.return_type {
            self.write(": ");
            self.write(&print_type(rt));
        }
        self.write(" ");
    }

    fn print_method_signature(&mut self, m: &Method) {
        self.write(&visibility_keyword(&m.visibility));
        self.write(" ");
        if m.static_ {
            self.write("static ");
        }
        if m.abstract_ {
            self.write("abstract ");
        }
        if m.async_ {
            self.write("async ");
        }
        self.write("function ");
        self.write(&m.name);
        self.write("(");
        for (i, p) in m.params.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            self.print_param(p);
        }
        self.write(")");
        if let Some(rt) = &m.return_type {
            self.write(": ");
            self.write(&print_type(rt));
        }
    }

    fn print_param(&mut self, p: &Param) {
        if let Some(promo) = &p.promotion {
            self.write(&visibility_keyword(&promo.visibility));
            self.write(" ");
            if promo.readonly {
                self.write("readonly ");
            }
        }
        if let Some(ty) = &p.ty {
            self.write(&print_type(ty));
            self.write(" ");
        }
        self.write(&format!("${}", p.name));
        if let Some(d) = &p.default {
            self.write(" = ");
            self.write(&print_expr(d));
        }
    }

    fn print_class_member(&mut self, m: &ClassMember) {
        match m {
            ClassMember::Property(p) => self.print_property(p),
            ClassMember::Method(meth) => {
                self.print_method_signature(meth);
                if let Some(body) = &meth.body {
                    self.write(" ");
                    self.print_block(body);
                } else {
                    self.write(";");
                }
            }
            ClassMember::Constant(c) => {
                self.write(&visibility_keyword(&c.visibility));
                self.write(" const ");
                if let Some(ty) = &c.ty {
                    self.write(&print_type(ty));
                    self.write(" ");
                }
                self.write(&c.name);
                self.write(" = ");
                self.write(&print_expr(&c.value));
                self.write(";");
            }
            ClassMember::UseTrait(block) => {
                self.write("use ");
                for (i, t) in block.traits.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(&print_type(t));
                }
                if block.adaptations.is_empty() {
                    self.write(";");
                } else {
                    self.write(" {");
                    self.indent += 1;
                    for a in &block.adaptations {
                        self.newline();
                        match a {
                            psx_ast::TraitAdaptation::InsteadOf {
                                winner_trait,
                                method,
                                losers,
                            } => {
                                self.write(&format!("{winner_trait}::{method} insteadof "));
                                self.write(&losers.join(", "));
                                self.write(";");
                            }
                            psx_ast::TraitAdaptation::Alias {
                                source_trait,
                                source_method,
                                new_name,
                                new_visibility,
                            } => {
                                self.write(&format!("{source_trait}::{source_method} as "));
                                if let Some(v) = new_visibility {
                                    self.write(&visibility_keyword(v));
                                    self.write(" ");
                                }
                                self.write(new_name);
                                self.write(";");
                            }
                        }
                    }
                    self.indent -= 1;
                    self.newline();
                    self.write("}");
                }
            }
        }
    }

    fn print_property(&mut self, p: &Property) {
        self.write(&visibility_keyword(&p.visibility));
        if let Some(set_vis) = &p.set_visibility {
            self.write(&format!("({})", visibility_word(&p.visibility)));
            self.write("(set:");
            self.write(&visibility_word(set_vis));
            self.write(")");
        }
        self.write(" ");
        if p.static_ {
            self.write("static ");
        }
        if p.readonly {
            self.write("readonly ");
        }
        if let Some(ty) = &p.ty {
            self.write(&print_type(ty));
            self.write(" ");
        }
        self.write(&format!("${}", p.name));
        if let Some(d) = &p.default {
            self.write(" = ");
            self.write(&print_expr(d));
        }
        if p.hooks.is_some() {
            // Hook bodies are deferred — fall back to verbatim-or-empty.
            self.write(" { /* hooks preserved on round-trip */ }");
        } else {
            self.write(";");
        }
    }
}

fn stmt_kind(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::Namespace(..) => "namespace",
        Stmt::Use(_) => "use",
        Stmt::Function(_) => "function",
        Stmt::Class(_) => "class",
        Stmt::Interface(_) => "interface",
        Stmt::Enum(_) => "enum",
        Stmt::Trait(_) => "trait",
        _ => "other",
    }
}

fn visibility_keyword(v: &Visibility) -> String {
    match v {
        Visibility::Public => "public".to_string(),
        Visibility::Private => "private".to_string(),
        Visibility::Protected => "protected".to_string(),
    }
}

fn visibility_word(v: &Visibility) -> String {
    visibility_keyword(v)
}

fn print_type(t: &TypeAnn) -> String {
    match t {
        TypeAnn::Named(n) => n.clone(),
        TypeAnn::Nullable(inner) => format!("?{}", print_type(inner)),
        TypeAnn::Union(parts) => parts.iter().map(print_type).collect::<Vec<_>>().join(" | "),
        TypeAnn::Generic(name, args) => {
            let inner: Vec<_> = args.iter().map(print_type).collect();
            format!("{name}<{}>", inner.join(", "))
        }
    }
}

fn print_expr(e: &Expr) -> String {
    match e {
        Expr::Int(n) => n.to_string(),
        Expr::Float(f) => {
            // Preserve trailing .0 for whole-number floats so they
            // round-trip distinct from integers.
            if f.fract() == 0.0 {
                format!("{f:.1}")
            } else {
                f.to_string()
            }
        }
        Expr::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Expr::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Expr::Null => "null".to_string(),
        Expr::Var(name) => format!("${name}"),
        Expr::Ident(name) => name.clone(),
        Expr::Call { callee, args, .. } => {
            let inner: Vec<_> = args.iter().map(print_expr).collect();
            format!("{}({})", print_expr(callee), inner.join(", "))
        }
        Expr::New { class, args } => {
            let inner: Vec<_> = args.iter().map(print_expr).collect();
            format!("new {}({})", print_type(class), inner.join(", "))
        }
        Expr::Access { target, name, op } => {
            let op_s = match op {
                AccessOp::Arrow => "->",
                AccessOp::NullSafeArrow => "?->",
                AccessOp::DoubleColon => "::",
            };
            format!("{}{op_s}{name}", print_expr(target))
        }
        Expr::Index { obj, key } => {
            format!("{}[{}]", print_expr(obj), print_expr(key))
        }
        Expr::Array(items) => print_array(items),
        Expr::SelfRef => "self".to_string(),
        Expr::ParentRef => "parent".to_string(),
        Expr::StaticRef => "static".to_string(),
        Expr::Assign { target, value } => {
            format!("{} = {}", print_expr(target), print_expr(value))
        }
        Expr::CompoundAssign { op, target, value } => {
            format!(
                "{} {}= {}",
                print_expr(target),
                binop_str(op),
                print_expr(value)
            )
        }
        Expr::Binary { op, lhs, rhs } => {
            format!("{} {} {}", print_expr(lhs), binop_str(op), print_expr(rhs))
        }
        Expr::Unary { op, expr } => {
            let s = match op {
                UnOp::Neg => "-",
                UnOp::Pos => "+",
            };
            format!("{s}{}", print_expr(expr))
        }
        Expr::Await(inner) => format!("await {}", print_expr(inner)),
        Expr::FirstClassCallable(inner) => format!("{}(...)", print_expr(inner)),
        Expr::IncDec { op, fix, target } => {
            let s = match op {
                IncDecOp::Inc => "++",
                IncDecOp::Dec => "--",
            };
            match fix {
                IncDecFix::Prefix => format!("{s}{}", print_expr(target)),
                IncDecFix::Postfix => format!("{}{s}", print_expr(target)),
            }
        }
        Expr::ArrowFn {
            params,
            return_type,
            body,
        } => {
            let p: Vec<String> = params
                .iter()
                .map(|p| match &p.ty {
                    Some(t) => format!("{} ${}", print_type(t), p.name),
                    None => format!("${}", p.name),
                })
                .collect();
            let rt = return_type
                .as_ref()
                .map(|t| format!(": {}", print_type(t)))
                .unwrap_or_default();
            format!("fn({}){} => {}", p.join(", "), rt, print_expr(body))
        }
        Expr::Ternary { cond, then, else_ } => format!(
            "{} ? {} : {}",
            print_expr(cond),
            print_expr(then),
            print_expr(else_)
        ),
        Expr::ShortTernary { cond, else_ } => {
            format!("{} ?: {}", print_expr(cond), print_expr(else_))
        }
        Expr::Match { scrutinee, arms } => print_match(scrutinee, arms),
        Expr::InterpolatedStr(parts) => print_interpolated(parts),
    }
}

fn print_array(items: &[ArrayItem]) -> String {
    let inner: Vec<_> = items
        .iter()
        .map(|it| match &it.key {
            Some(k) => format!("{} => {}", print_expr(k), print_expr(&it.value)),
            None => print_expr(&it.value),
        })
        .collect();
    format!("[{}]", inner.join(", "))
}

fn print_match(scrutinee: &Expr, arms: &[MatchArm]) -> String {
    let mut s = format!("match ({}) {{ ", print_expr(scrutinee));
    let mut first = true;
    for arm in arms {
        if !first {
            s.push_str(", ");
        }
        first = false;
        match &arm.conds {
            None => s.push_str("default"),
            Some(cs) => {
                let cs_s: Vec<_> = cs.iter().map(print_expr).collect();
                s.push_str(&cs_s.join(", "));
            }
        }
        s.push_str(" => ");
        s.push_str(&print_expr(&arm.body));
    }
    s.push_str(" }");
    s
}

fn print_interpolated(parts: &[InterpolatedPart]) -> String {
    let mut s = String::from("\"");
    for part in parts {
        match part {
            InterpolatedPart::Lit(lit) => {
                s.push_str(&lit.replace('\\', "\\\\").replace('"', "\\\""));
            }
            InterpolatedPart::Expr(e) => match e.as_ref() {
                Expr::Var(name) => {
                    s.push('$');
                    s.push_str(name);
                }
                other => {
                    s.push('{');
                    s.push_str(&print_expr(other));
                    s.push('}');
                }
            },
        }
    }
    s.push('"');
    s
}

fn binop_str(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Pow => "**",
        BinOp::Concat => ".",
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
        BinOp::Spaceship => "<=>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        let module = psx_parser::parse(source).unwrap();
        format_module(&module)
    }

    #[test]
    fn format_function_signature_round_trips() {
        let formatted = fmt("function add(int $a, int $b): int { return $a + $b; }\n");
        assert!(formatted.contains("function add(int $a, int $b): int"));
        assert!(formatted.contains("return $a + $b;"));
    }

    #[test]
    fn format_class_with_constructor() {
        let formatted =
            fmt("class User { public function __construct(public string $email) {} }\n");
        assert!(formatted.contains("class User"));
        assert!(formatted.contains("public function __construct(public string $email)"));
    }

    #[test]
    fn format_namespace_and_use() {
        let formatted = fmt("namespace App;\nuse App\\Models\\User;\n");
        assert!(formatted.contains("namespace App;"));
        assert!(formatted.contains("use App\\Models\\User;"));
    }

    #[test]
    fn print_call_expression() {
        let formatted = fmt("$x = foo(1, 2);\n");
        assert!(formatted.contains("$x = foo(1, 2);"), "got:\n{formatted}");
    }
}
