//! PHPScript parser.
//!
//! Recursive-descent. Each phase grows the parser; this is the Phase 1 seed
//! that handles expression statements and integer literals.

use psx_ast::{
    AccessOp, ArrayItem, BinOp, Catch, Class, ClassConstant, ClassMember, EnumCase, EnumDecl, Expr,
    FunctionDecl, HookBody, IncDecFix, IncDecOp, Interface, InterfaceMember, InterpolatedPart,
    MatchArm, Method, Module, Param, Promotion, Property, PropertyHooks, SetHook, Span, Stmt,
    TraitAdaptation, TraitDecl, TypeAnn, UnOp, UseItem, UseKind, UseStmt, UseTraitBlock,
    Visibility,
};
use psx_lexer::{lex, InterpolatedSegment, LexError, Token, TokenKind};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error(transparent)]
    Lex(#[from] LexError),
    #[error("unexpected token {kind:?} at byte {pos}; expected {expected}")]
    UnexpectedToken {
        kind: TokenKind,
        pos: u32,
        expected: &'static str,
    },
    #[error("unexpected end of input; expected {expected}")]
    UnexpectedEof { expected: &'static str },
    #[error(
        "loose equality operator `{op}` is not supported in PHPScript at byte {pos}; \
         use `{suggested}` (strict equality) instead"
    )]
    LooseEqualityRejected {
        op: &'static str,
        suggested: &'static str,
        pos: u32,
    },
    #[error(
        "asymmetric visibility at byte {pos}: write side `{set_vis}(set)` is more \
         permissive than read side `{read_vis}`. PHP 8.4 requires write ≤ read \
         (see https://wiki.php.net/rfc/asymmetric-visibility)."
    )]
    AsymVisWriteWiderThanRead {
        read_vis: &'static str,
        set_vis: &'static str,
        pos: u32,
    },
    #[error(
        "deprecated `var` keyword at byte {pos}. PHPScript follows modern PHP — \
         use `public`, `protected`, or `private` instead."
    )]
    DeprecatedVarKeyword { pos: u32 },
    #[error(
        "deprecated `array()` long-form constructor at byte {pos}. Use the short \
         `[...]` literal — PHPScript follows modern PHP and rejects the long form."
    )]
    DeprecatedArrayConstructor { pos: u32 },
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

/// Modifiers that may appear on either a class property or method (in any
/// order). Visibility defaults to public when omitted.
#[derive(Default)]
struct MemberModifiers {
    visibility: Option<Visibility>,
    /// PHP 8.4 asymmetric visibility — set when `<vis>(set)` appears. When
    /// `None`, the property is symmetric.
    set_visibility: Option<Visibility>,
    static_: bool,
    readonly: bool,
    abstract_: bool,
    final_: bool,
    async_: bool,
}

impl Parser {
    fn peek(&self) -> &Token {
        // The lexer always pushes a trailing Eof, so this never goes oob.
        &self.tokens[self.pos]
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        tok
    }

    /// Byte offset where the next token starts. Used to record `Span.start`
    /// at the head of a `parse_*` function before any tokens are consumed.
    fn next_start(&self) -> u32 {
        self.peek().span.start
    }

    /// Byte offset where the most-recently consumed token ended. Used to
    /// close out a `Span` after a `parse_*` function has done its work.
    fn last_end(&self) -> u32 {
        // pos points at the next token; the previous one (if any) is the
        // one we just consumed.
        if self.pos == 0 {
            self.tokens[0].span.start
        } else {
            self.tokens[self.pos - 1].span.end
        }
    }

    fn span_from(&self, start: u32) -> Span {
        Span::new(start, self.last_end())
    }

    fn expect(&mut self, kind: &TokenKind, label: &'static str) -> Result<Token, ParseError> {
        let cur = self.peek();
        if std::mem::discriminant(&cur.kind) == std::mem::discriminant(kind) {
            Ok(self.bump())
        } else if matches!(cur.kind, TokenKind::Eof) {
            Err(ParseError::UnexpectedEof { expected: label })
        } else {
            Err(ParseError::UnexpectedToken {
                kind: cur.kind.clone(),
                pos: cur.span.start,
                expected: label,
            })
        }
    }

    fn parse_module(&mut self) -> Result<Module, ParseError> {
        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        Ok(Module { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek().kind {
            TokenKind::LBrace => self.parse_block(),
            TokenKind::If => self.parse_if(),
            TokenKind::Return => self.parse_return(),
            TokenKind::While => self.parse_while(),
            TokenKind::Do => self.parse_do_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Foreach => self.parse_foreach(),
            TokenKind::Break => self.parse_break_continue(true),
            TokenKind::Continue => self.parse_break_continue(false),
            TokenKind::Function | TokenKind::Async => self.parse_function_decl(),
            TokenKind::Throw => self.parse_throw(),
            TokenKind::Try => self.parse_try(),
            TokenKind::Namespace => self.parse_namespace_decl(),
            TokenKind::Use => self.parse_use_stmt(),
            TokenKind::Class | TokenKind::Abstract | TokenKind::Final | TokenKind::Readonly => {
                self.parse_class_decl()
            }
            TokenKind::Interface => self.parse_interface_decl(),
            TokenKind::Enum => self.parse_enum_decl(),
            TokenKind::Trait => self.parse_trait_decl(),
            _ => self.parse_expr_stmt(),
        }
    }

    fn parse_class_decl(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        // Class-level modifiers in any order: `abstract`, `final`, `readonly`.
        // A class can be only ONE of `abstract` / `final`; both is a parse
        // error in PHP. We accept it permissively here (parser stays simple)
        // and let the user / tsc catch it.
        let mut abstract_ = false;
        let mut final_ = false;
        let mut readonly = false;
        loop {
            match self.peek().kind {
                TokenKind::Abstract if !abstract_ => {
                    self.bump();
                    abstract_ = true;
                }
                TokenKind::Final if !final_ => {
                    self.bump();
                    final_ = true;
                }
                TokenKind::Readonly if !readonly => {
                    self.bump();
                    readonly = true;
                }
                _ => break,
            }
        }
        self.expect(&TokenKind::Class, "`class`")?;
        let name = self.expect_identifier("class name")?;
        let extends = if matches!(self.peek().kind, TokenKind::Extends) {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        let mut implements = Vec::new();
        if matches!(self.peek().kind, TokenKind::Implements) {
            self.bump();
            loop {
                implements.push(self.parse_type_annotation()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::LBrace, "`{` to open class body")?;
        let mut members = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            members.push(self.parse_class_member(readonly)?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close class body")?;
        Ok(Stmt::Class(Class {
            name,
            abstract_,
            final_,
            readonly,
            extends,
            implements,
            members,
            span: self.span_from(start),
        }))
    }

    fn parse_match_arm(&mut self) -> Result<MatchArm, ParseError> {
        let conds = if matches!(self.peek().kind, TokenKind::Default) {
            self.bump();
            None
        } else {
            let mut cs = Vec::new();
            loop {
                cs.push(self.parse_expr()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.bump();
                    if matches!(self.peek().kind, TokenKind::FatArrow) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
            Some(cs)
        };
        self.expect(&TokenKind::FatArrow, "`=>` in match arm")?;
        let body = self.parse_expr()?;
        self.expect(&TokenKind::Comma, "`,` after match arm body")?;
        Ok(MatchArm { conds, body })
    }

    fn parse_trait_decl(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Trait, "`trait`")?;
        let name = self.expect_identifier("trait name")?;
        self.expect(&TokenKind::LBrace, "`{` to open trait body")?;
        let mut members = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            // Traits aren't readonly classes, so `class_is_readonly` is
            // always false at this site.
            members.push(self.parse_class_member(false)?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close trait body")?;
        Ok(Stmt::Trait(TraitDecl {
            name,
            members,
            span: self.span_from(start),
        }))
    }

    fn parse_enum_decl(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Enum, "`enum`")?;
        let name = self.expect_identifier("enum name")?;
        let backed_type = if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        let mut implements = Vec::new();
        if matches!(self.peek().kind, TokenKind::Implements) {
            self.bump();
            loop {
                implements.push(self.parse_type_annotation()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::LBrace, "`{` to open enum body")?;
        let mut cases = Vec::new();
        let mut constants = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            if matches!(self.peek().kind, TokenKind::Case) {
                cases.push(self.parse_enum_case(backed_type.is_some())?);
            } else {
                // const-style member.
                let mods = self.parse_member_modifiers();
                if matches!(self.peek().kind, TokenKind::Const) {
                    constants.push(self.parse_class_constant(mods)?);
                } else {
                    let cur = self.peek().clone();
                    return Err(ParseError::UnexpectedToken {
                        kind: cur.kind,
                        pos: cur.span.start,
                        expected: "`case` or `const` in enum body (methods deferred to later)",
                    });
                }
            }
        }
        self.expect(&TokenKind::RBrace, "`}` to close enum body")?;
        Ok(Stmt::Enum(EnumDecl {
            name,
            backed_type,
            implements,
            cases,
            constants,
            span: self.span_from(start),
        }))
    }

    fn parse_enum_case(&mut self, is_backed: bool) -> Result<EnumCase, ParseError> {
        self.expect(&TokenKind::Case, "`case`")?;
        let name = self.expect_identifier("enum case name")?;
        let value = if matches!(self.peek().kind, TokenKind::Eq) {
            if !is_backed {
                let span = self.peek().span;
                return Err(ParseError::UnexpectedToken {
                    kind: TokenKind::Eq,
                    pos: span.start,
                    expected: "`;` (pure enum cases have no value)",
                });
            }
            self.bump();
            Some(self.parse_expr()?)
        } else {
            if is_backed {
                let cur = self.peek().clone();
                return Err(ParseError::UnexpectedToken {
                    kind: cur.kind,
                    pos: cur.span.start,
                    expected: "`= <value>` (backed enum cases require a value)",
                });
            }
            None
        };
        self.expect(&TokenKind::Semicolon, "`;` after enum case")?;
        Ok(EnumCase { name, value })
    }

    fn parse_interface_decl(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Interface, "`interface`")?;
        let name = self.expect_identifier("interface name")?;
        let mut extends = Vec::new();
        if matches!(self.peek().kind, TokenKind::Extends) {
            self.bump();
            loop {
                extends.push(self.parse_type_annotation()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::LBrace, "`{` to open interface body")?;
        let mut members = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            members.push(self.parse_interface_member()?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close interface body")?;
        Ok(Stmt::Interface(Interface {
            name,
            extends,
            members,
            span: self.span_from(start),
        }))
    }

    fn parse_interface_member(&mut self) -> Result<InterfaceMember, ParseError> {
        let mods = self.parse_member_modifiers();
        if matches!(self.peek().kind, TokenKind::Const) {
            return self
                .parse_class_constant(mods)
                .map(InterfaceMember::Constant);
        }
        // Interface methods have no body — must end with `;` after the
        // signature. We piggyback on parse_method's abstract path by forcing
        // the modifier; PHP doesn't actually require `abstract` here, so we
        // don't surface a warning if the user wrote it.
        self.expect(&TokenKind::Function, "`function` in interface body")?;
        let name = self.expect_identifier("method name")?;
        self.expect(&TokenKind::LParen, "`(` in method signature")?;
        let params = self.parse_param_list()?;
        self.expect(&TokenKind::RParen, "`)` to close method parameters")?;
        let return_type = if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        self.expect(
            &TokenKind::Semicolon,
            "`;` after interface method signature",
        )?;
        Ok(InterfaceMember::Method(Method {
            visibility: mods.visibility.unwrap_or(Visibility::Public),
            static_: mods.static_,
            abstract_: mods.abstract_,
            final_: mods.final_,
            async_: mods.async_,
            name,
            params,
            return_type,
            body: None,
        }))
    }

    fn parse_class_member(&mut self, class_is_readonly: bool) -> Result<ClassMember, ParseError> {
        // `use TraitA, TraitB;` is a class-body trait import. It has no
        // modifiers and must come before the modifier dispatch.
        if matches!(self.peek().kind, TokenKind::Use) {
            return self.parse_use_trait();
        }
        // Reject the deprecated `var` property declaration explicitly.
        // PHP still parses it but emits a deprecation notice; PHPScript
        // is modern-only and rejects outright.
        if let TokenKind::Identifier(n) = &self.peek().kind {
            if n == "var" {
                return Err(ParseError::DeprecatedVarKeyword {
                    pos: self.peek().span.start,
                });
            }
        }
        let mods = self.parse_member_modifiers();
        if matches!(self.peek().kind, TokenKind::Const) {
            return self.parse_class_constant(mods).map(ClassMember::Constant);
        }
        if matches!(self.peek().kind, TokenKind::Function) {
            return self.parse_method(mods).map(ClassMember::Method);
        }
        self.parse_property(mods, class_is_readonly)
            .map(ClassMember::Property)
    }

    fn parse_use_trait(&mut self) -> Result<ClassMember, ParseError> {
        self.expect(&TokenKind::Use, "`use`")?;
        let mut types = vec![self.parse_type_atom()?];
        while matches!(self.peek().kind, TokenKind::Comma) {
            self.bump();
            types.push(self.parse_type_atom()?);
        }
        let adaptations = if matches!(self.peek().kind, TokenKind::LBrace) {
            self.bump();
            let mut adaptations = Vec::new();
            while !matches!(self.peek().kind, TokenKind::RBrace) {
                adaptations.push(self.parse_trait_adaptation()?);
            }
            self.expect(&TokenKind::RBrace, "`}` to close `use ... { ... }`")?;
            adaptations
        } else {
            self.expect(&TokenKind::Semicolon, "`;` after `use TraitName`")?;
            Vec::new()
        };
        Ok(ClassMember::UseTrait(UseTraitBlock {
            traits: types,
            adaptations,
        }))
    }

    /// Parse a single trait adaptation inside `use ... { ... }`:
    ///   `Trait::method insteadof OtherTrait[, ...] ;`
    ///   `Trait::method as [visibility] newName ;`
    fn parse_trait_adaptation(&mut self) -> Result<TraitAdaptation, ParseError> {
        let source_trait = self.parse_bare_ident("trait name")?;
        self.expect(&TokenKind::ColonColon, "`::` after trait name")?;
        let source_method = self.parse_bare_ident("trait method name")?;
        let adaptation = match self.peek().kind {
            TokenKind::InsteadOf => {
                self.bump();
                let mut losers = vec![self.parse_bare_ident("trait name after `insteadof`")?];
                while matches!(self.peek().kind, TokenKind::Comma) {
                    self.bump();
                    losers.push(self.parse_bare_ident("trait name after `,`")?);
                }
                TraitAdaptation::InsteadOf {
                    winner_trait: source_trait,
                    method: source_method,
                    losers,
                }
            }
            TokenKind::As => {
                self.bump();
                let new_visibility = match self.peek().kind {
                    TokenKind::Public => {
                        self.bump();
                        Some(Visibility::Public)
                    }
                    TokenKind::Private => {
                        self.bump();
                        Some(Visibility::Private)
                    }
                    TokenKind::Protected => {
                        self.bump();
                        Some(Visibility::Protected)
                    }
                    _ => None,
                };
                let new_name = self.parse_bare_ident("aliased method name after `as`")?;
                TraitAdaptation::Alias {
                    source_trait,
                    source_method,
                    new_name,
                    new_visibility,
                }
            }
            _ => {
                let cur = self.peek().clone();
                return Err(ParseError::UnexpectedToken {
                    kind: cur.kind,
                    pos: cur.span.start,
                    expected: "`insteadof` or `as` in trait adaptation",
                });
            }
        };
        self.expect(&TokenKind::Semicolon, "`;` after trait adaptation")?;
        Ok(adaptation)
    }

    fn parse_bare_ident(&mut self, expected: &'static str) -> Result<String, ParseError> {
        let cur = self.peek().clone();
        match cur.kind {
            TokenKind::Identifier(n) => {
                self.bump();
                Ok(n)
            }
            other => Err(ParseError::UnexpectedToken {
                kind: other,
                pos: cur.span.start,
                expected,
            }),
        }
    }

    fn parse_class_constant(&mut self, mods: MemberModifiers) -> Result<ClassConstant, ParseError> {
        self.expect(&TokenKind::Const, "`const`")?;
        // Optional type annotation (PHP 8.3+ typed class constants).
        let ty = if !matches!(self.peek().kind, TokenKind::Identifier(_)) {
            // Identifier ahead is the constant name; no type.
            // But `Foo::BAR` style class names are also Identifiers, so we
            // need to look at what follows. For now, peek-2: if next token
            // is `=`, the identifier is the name. Otherwise the identifier
            // is a type and we consume it.
            None
        } else {
            // Look at the token AFTER the identifier. If it's `=`, the
            // identifier is the constant name (no type). Otherwise it's a
            // type annotation.
            let next_after = self.tokens.get(self.pos + 1).map(|t| &t.kind);
            if matches!(next_after, Some(TokenKind::Eq)) {
                None
            } else {
                Some(self.parse_type_annotation()?)
            }
        };
        let name = self.expect_identifier("class constant name")?;
        self.expect(&TokenKind::Eq, "`=` in class constant declaration")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "`;` after class constant value")?;
        Ok(ClassConstant {
            visibility: mods.visibility.unwrap_or(Visibility::Public),
            final_: mods.final_,
            ty,
            name,
            value,
        })
    }

    fn parse_member_modifiers(&mut self) -> MemberModifiers {
        let mut mods = MemberModifiers::default();
        loop {
            // PHP 8.4 asymmetric visibility uses the form:
            //   `<read-vis> <write-vis>(set) <type> $name`
            // The read visibility is just the bare keyword; only the *write*
            // visibility is qualified with `(set)`. PHP does NOT have a
            // `(get)` qualifier — see the asymmetric-visibility RFC.
            if let Some(v) = self.peek_visibility_kw() {
                let saved = self.pos;
                self.bump();
                if self.peek_set_qualifier() {
                    if mods.set_visibility.is_some() {
                        self.pos = saved;
                        break;
                    }
                    mods.set_visibility = Some(v);
                } else {
                    if mods.visibility.is_some() {
                        self.pos = saved;
                        break;
                    }
                    mods.visibility = Some(v);
                }
                continue;
            }
            match self.peek().kind {
                TokenKind::Static if !mods.static_ => {
                    self.bump();
                    mods.static_ = true;
                }
                TokenKind::Readonly if !mods.readonly => {
                    self.bump();
                    mods.readonly = true;
                }
                TokenKind::Abstract if !mods.abstract_ => {
                    self.bump();
                    mods.abstract_ = true;
                }
                TokenKind::Final if !mods.final_ => {
                    self.bump();
                    mods.final_ = true;
                }
                TokenKind::Async if !mods.async_ => {
                    self.bump();
                    mods.async_ = true;
                }
                _ => break,
            }
        }
        mods
    }

    /// Helper: peek for `Public` / `Private` / `Protected` and return the
    /// matching `Visibility` without consuming.
    fn peek_visibility_kw(&self) -> Option<Visibility> {
        match self.peek().kind {
            TokenKind::Public => Some(Visibility::Public),
            TokenKind::Private => Some(Visibility::Private),
            TokenKind::Protected => Some(Visibility::Protected),
            _ => None,
        }
    }

    /// After consuming a visibility keyword, peek for `(set)` — the PHP 8.4
    /// asymmetric-write-visibility qualifier. Consumes the three tokens
    /// (`(`, `set`, `)`) on a hit; otherwise leaves the parser position
    /// untouched. Anything else (`(get)`, `(whatever)`) returns false with
    /// the position restored, so the next caller can surface a clean parse
    /// error at the unexpected `(`.
    fn peek_set_qualifier(&mut self) -> bool {
        if !matches!(self.peek().kind, TokenKind::LParen) {
            return false;
        }
        let saved = self.pos;
        self.bump(); // `(`
        let is_set = matches!(&self.peek().kind, TokenKind::Identifier(n) if n == "set");
        if !is_set {
            self.pos = saved;
            return false;
        }
        self.bump(); // `set`
        if !matches!(self.peek().kind, TokenKind::RParen) {
            self.pos = saved;
            return false;
        }
        self.bump(); // `)`
        true
    }

    fn parse_method(&mut self, mods: MemberModifiers) -> Result<Method, ParseError> {
        self.expect(&TokenKind::Function, "`function` in class body")?;
        let name = self.expect_identifier("method name")?;
        self.expect(&TokenKind::LParen, "`(` in method signature")?;
        let params = self.parse_param_list()?;
        self.expect(&TokenKind::RParen, "`)` to close method parameters")?;
        let return_type = if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        // Abstract methods have NO body — just a semicolon. Otherwise expect
        // a normal `{ body }` block.
        let body = if mods.abstract_ {
            self.expect(&TokenKind::Semicolon, "`;` after abstract method signature")?;
            None
        } else {
            self.expect(&TokenKind::LBrace, "`{` to open method body")?;
            let mut stmts = Vec::new();
            while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
                stmts.push(self.parse_stmt()?);
            }
            self.expect(&TokenKind::RBrace, "`}` to close method body")?;
            Some(stmts)
        };
        Ok(Method {
            visibility: mods.visibility.unwrap_or(Visibility::Public),
            static_: mods.static_,
            abstract_: mods.abstract_,
            final_: mods.final_,
            async_: mods.async_,
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_property(
        &mut self,
        mods: MemberModifiers,
        class_is_readonly: bool,
    ) -> Result<Property, ParseError> {
        // Optional type annotation (anything that isn't a Variable indicates
        // a type comes first).
        let ty = if matches!(self.peek().kind, TokenKind::Variable(_)) {
            None
        } else {
            Some(self.parse_type_annotation()?)
        };
        let name = self.expect_variable("property name (e.g. `$name`)")?;
        let default = if matches!(self.peek().kind, TokenKind::Eq) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        // Either `;` (plain property) or `{ get/set ... }` (PHP 8.4 hooks).
        let hooks = if matches!(self.peek().kind, TokenKind::LBrace) {
            self.bump();
            let parsed = self.parse_property_hooks(&ty)?;
            self.expect(&TokenKind::RBrace, "`}` to close property hooks")?;
            Some(parsed)
        } else {
            self.expect(
                &TokenKind::Semicolon,
                "`;` or `{` after property declaration",
            )?;
            None
        };
        let visibility = mods.visibility.unwrap_or(Visibility::Public);
        // Only record set_visibility when it actually differs from the
        // get-side; symmetric properties keep `None`.
        let set_visibility = match mods.set_visibility {
            Some(v) if v != visibility => Some(v),
            _ => None,
        };
        if let Some(sv) = set_visibility {
            check_asym_vis(visibility, sv, self.next_start())?;
        }
        Ok(Property {
            visibility,
            set_visibility,
            readonly: mods.readonly || class_is_readonly,
            static_: mods.static_,
            ty,
            name,
            default,
            hooks,
        })
    }

    fn parse_property_hooks(
        &mut self,
        prop_ty: &Option<TypeAnn>,
    ) -> Result<PropertyHooks, ParseError> {
        let mut get: Option<HookBody> = None;
        let mut set: Option<SetHook> = None;
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let cur = self.peek().clone();
            match &cur.kind {
                TokenKind::Identifier(n) if n == "get" => {
                    if get.is_some() {
                        return Err(ParseError::UnexpectedToken {
                            kind: cur.kind.clone(),
                            pos: cur.span.start,
                            expected: "single `get` hook (already declared)",
                        });
                    }
                    self.bump();
                    get = Some(self.parse_hook_body()?);
                }
                TokenKind::Identifier(n) if n == "set" => {
                    if set.is_some() {
                        return Err(ParseError::UnexpectedToken {
                            kind: cur.kind.clone(),
                            pos: cur.span.start,
                            expected: "single `set` hook (already declared)",
                        });
                    }
                    self.bump();
                    let (param_name, param_type) = if matches!(self.peek().kind, TokenKind::LParen)
                    {
                        self.bump();
                        let ty = if matches!(self.peek().kind, TokenKind::Variable(_)) {
                            None
                        } else {
                            Some(self.parse_type_annotation()?)
                        };
                        let var_name = self.expect_variable("set hook parameter name")?;
                        self.expect(&TokenKind::RParen, "`)` to close set param")?;
                        (var_name, ty.or_else(|| prop_ty.clone()))
                    } else {
                        ("value".into(), prop_ty.clone())
                    };
                    let body = self.parse_hook_body()?;
                    set = Some(SetHook {
                        param_name,
                        param_type,
                        body,
                    });
                }
                _ => {
                    return Err(ParseError::UnexpectedToken {
                        kind: cur.kind,
                        pos: cur.span.start,
                        expected: "`get` or `set` in property hook block",
                    });
                }
            }
        }
        Ok(PropertyHooks { get, set })
    }

    fn parse_hook_body(&mut self) -> Result<HookBody, ParseError> {
        if matches!(self.peek().kind, TokenKind::FatArrow) {
            self.bump();
            let expr = self.parse_expr()?;
            self.expect(&TokenKind::Semicolon, "`;` after short hook body")?;
            return Ok(HookBody::Expr(expr));
        }
        self.expect(&TokenKind::LBrace, "`{` or `=>` to start hook body")?;
        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close hook body")?;
        Ok(HookBody::Block(stmts))
    }

    fn parse_namespace_decl(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Namespace, "`namespace`")?;
        // PHP allows a leading backslash on the namespace path; consume it
        // permissively (it has no effect on the AST).
        if matches!(self.peek().kind, TokenKind::Backslash) {
            self.bump();
        }
        let mut path = vec![self.expect_identifier("namespace segment")?];
        while matches!(self.peek().kind, TokenKind::Backslash) {
            self.bump();
            path.push(self.expect_identifier("namespace segment")?);
        }
        self.expect(&TokenKind::Semicolon, "`;` after namespace declaration")?;
        Ok(Stmt::Namespace(path, self.span_from(start)))
    }

    fn parse_use_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Use, "`use`")?;
        // Optional kind: `use function ...` or `use const ...`.
        let kind = match self.peek().kind {
            TokenKind::Function => {
                self.bump();
                UseKind::Function
            }
            TokenKind::Const => {
                self.bump();
                UseKind::Const
            }
            _ => UseKind::Class,
        };
        // Optional leading backslash (PHP "global" indicator). Ignored.
        if matches!(self.peek().kind, TokenKind::Backslash) {
            self.bump();
        }
        let mut prefix = vec![self.expect_identifier("use path segment")?];
        loop {
            if matches!(self.peek().kind, TokenKind::Backslash) {
                self.bump();
                // Group form: `use Prefix\{A, B as B2};`.
                if matches!(self.peek().kind, TokenKind::LBrace) {
                    self.bump();
                    let items = self.parse_use_group_items(&prefix)?;
                    self.expect(&TokenKind::RBrace, "`}` to close use group")?;
                    self.expect(&TokenKind::Semicolon, "`;` after use group")?;
                    return Ok(Stmt::Use(UseStmt {
                        kind,
                        items,
                        span: self.span_from(start),
                    }));
                }
                prefix.push(self.expect_identifier("use path segment")?);
            } else {
                // Single-item: optional alias, then `;`.
                let alias = if matches!(self.peek().kind, TokenKind::As) {
                    self.bump();
                    Some(self.expect_identifier("alias name after `as`")?)
                } else {
                    None
                };
                self.expect(&TokenKind::Semicolon, "`;` after use")?;
                return Ok(Stmt::Use(UseStmt {
                    kind,
                    items: vec![UseItem {
                        path: prefix,
                        alias,
                    }],
                    span: self.span_from(start),
                }));
            }
        }
    }

    fn parse_use_group_items(&mut self, prefix: &[String]) -> Result<Vec<UseItem>, ParseError> {
        let mut items = Vec::new();
        if matches!(self.peek().kind, TokenKind::RBrace) {
            // Empty group `use Foo\{};` — accept silently; emit nothing.
            return Ok(items);
        }
        loop {
            let mut path: Vec<String> = prefix.to_vec();
            path.push(self.expect_identifier("group use item segment")?);
            while matches!(self.peek().kind, TokenKind::Backslash) {
                self.bump();
                path.push(self.expect_identifier("group use item segment")?);
            }
            let alias = if matches!(self.peek().kind, TokenKind::As) {
                self.bump();
                Some(self.expect_identifier("alias name after `as`")?)
            } else {
                None
            };
            items.push(UseItem { path, alias });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.bump();
                // Trailing comma allowed.
                if matches!(self.peek().kind, TokenKind::RBrace) {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(items)
    }

    fn parse_throw(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Throw, "`throw`")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "`;` after throw expression")?;
        Ok(Stmt::Throw(value, self.span_from(start)))
    }

    fn parse_try(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Try, "`try`")?;
        let body = self.parse_brace_block_stmts("try block")?;
        let mut catches = Vec::new();
        while matches!(self.peek().kind, TokenKind::Catch) {
            catches.push(self.parse_catch()?);
        }
        let finally = if matches!(self.peek().kind, TokenKind::Finally) {
            self.bump();
            Some(self.parse_brace_block_stmts("finally block")?)
        } else {
            None
        };
        Ok(Stmt::Try {
            body,
            catches,
            finally,
            span: self.span_from(start),
        })
    }

    fn parse_catch(&mut self) -> Result<Catch, ParseError> {
        self.expect(&TokenKind::Catch, "`catch`")?;
        self.expect(&TokenKind::LParen, "`(` after `catch`")?;
        let mut types = vec![self.parse_type_atom()?];
        while matches!(self.peek().kind, TokenKind::Pipe) {
            self.bump();
            types.push(self.parse_type_atom()?);
        }
        let var = if matches!(self.peek().kind, TokenKind::Variable(_)) {
            Some(self.expect_variable("catch binding name")?)
        } else {
            None
        };
        self.expect(&TokenKind::RParen, "`)` after catch type/binding")?;
        let body = self.parse_brace_block_stmts("catch block")?;
        Ok(Catch { types, var, body })
    }

    fn parse_brace_block_stmts(&mut self, ctx: &'static str) -> Result<Vec<Stmt>, ParseError> {
        let _ = ctx;
        self.expect(&TokenKind::LBrace, "`{` to open block")?;
        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close block")?;
        Ok(stmts)
    }

    fn parse_function_decl(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        // Optional `async` modifier before `function` (PHPScript extension).
        let async_ = if matches!(self.peek().kind, TokenKind::Async) {
            self.bump();
            true
        } else {
            false
        };
        self.expect(&TokenKind::Function, "`function`")?;
        let name = self.expect_identifier("function name")?;
        self.expect(&TokenKind::LParen, "`(` in function declaration")?;
        let params = self.parse_param_list()?;
        self.expect(&TokenKind::RParen, "`)` to close parameter list")?;
        let return_type = if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        self.expect(&TokenKind::LBrace, "`{` to open function body")?;
        let mut body = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            body.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close function body")?;
        Ok(Stmt::Function(FunctionDecl {
            name,
            params,
            return_type,
            body,
            async_,
            span: self.span_from(start),
        }))
    }

    fn parse_param_list(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if matches!(self.peek().kind, TokenKind::RParen) {
            return Ok(params);
        }
        loop {
            params.push(self.parse_param()?);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.bump();
                if matches!(self.peek().kind, TokenKind::RParen) {
                    break; // trailing comma
                }
            } else {
                break;
            }
        }
        Ok(params)
    }

    fn parse_param(&mut self) -> Result<Param, ParseError> {
        // Optional promotion modifiers (constructor property promotion).
        // PHP requires at least one of public/private/protected to opt in;
        // `readonly` may appear in any order. Order is forgiving.
        // PHP 8.4 asymmetric visibility on promoted params uses the form:
        // `public private(set) string $name` — read-side bare, write-side
        // qualified with `(set)`. No `(get)` qualifier exists.
        let mut visibility: Option<Visibility> = None;
        let mut set_visibility: Option<Visibility> = None;
        let mut readonly = false;
        loop {
            if let Some(v) = self.peek_visibility_kw() {
                let saved = self.pos;
                self.bump();
                if self.peek_set_qualifier() {
                    if set_visibility.is_some() {
                        self.pos = saved;
                        break;
                    }
                    set_visibility = Some(v);
                } else {
                    if visibility.is_some() {
                        self.pos = saved;
                        break;
                    }
                    visibility = Some(v);
                }
                continue;
            }
            if matches!(self.peek().kind, TokenKind::Readonly) && !readonly {
                self.bump();
                readonly = true;
                continue;
            }
            break;
        }
        let promotion = match visibility.or(set_visibility) {
            Some(_) => {
                let v = visibility.unwrap_or(Visibility::Public);
                let sv = match set_visibility {
                    Some(s) if s != v => Some(s),
                    _ => None,
                };
                if let Some(s) = sv {
                    check_asym_vis(v, s, self.next_start())?;
                }
                Some(Promotion {
                    visibility: v,
                    set_visibility: sv,
                    readonly,
                })
            }
            None => None,
        };

        // Optional type annotation.
        let ty = if !matches!(self.peek().kind, TokenKind::Variable(_)) {
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        let name = self.expect_variable("parameter name")?;
        // Optional default: `= <expr>`.
        let default = if matches!(self.peek().kind, TokenKind::Eq) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(Param {
            name,
            ty,
            default,
            promotion,
        })
    }

    /// Type annotation — `?Foo`, `Foo|Bar`, `array<int>`, `Result<T, E>`, etc.
    fn parse_type_annotation(&mut self) -> Result<TypeAnn, ParseError> {
        if matches!(self.peek().kind, TokenKind::Question) {
            self.bump();
            return Ok(TypeAnn::Nullable(Box::new(self.parse_type_annotation()?)));
        }
        let head = self.parse_type_atom()?;
        if matches!(self.peek().kind, TokenKind::Pipe) {
            let mut alts = vec![head];
            while matches!(self.peek().kind, TokenKind::Pipe) {
                self.bump();
                alts.push(self.parse_type_atom()?);
            }
            return Ok(TypeAnn::Union(alts));
        }
        Ok(head)
    }

    fn parse_type_atom(&mut self) -> Result<TypeAnn, ParseError> {
        let cur = self.peek().clone();
        let name = match cur.kind {
            TokenKind::Identifier(n) => {
                self.bump();
                n
            }
            // PHP reserved keywords that double as type names.
            TokenKind::Null => {
                self.bump();
                "null".into()
            }
            TokenKind::True => {
                self.bump();
                "true".into()
            }
            TokenKind::False => {
                self.bump();
                "false".into()
            }
            // `self`, `static`, `parent` — class-context type references.
            // The emitter resolves them when we know the enclosing class.
            TokenKind::SelfKw => {
                self.bump();
                "self".into()
            }
            TokenKind::Static => {
                self.bump();
                "static".into()
            }
            TokenKind::Parent => {
                self.bump();
                "parent".into()
            }
            TokenKind::Eof => {
                return Err(ParseError::UnexpectedEof {
                    expected: "type annotation",
                });
            }
            other => {
                return Err(ParseError::UnexpectedToken {
                    kind: other,
                    pos: cur.span.start,
                    expected: "type annotation",
                });
            }
        };

        // Generic instantiation: `name<T1, T2, ...>`.
        if matches!(self.peek().kind, TokenKind::Lt) {
            self.bump();
            let mut args = Vec::new();
            loop {
                args.push(self.parse_type_annotation()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.bump();
                    if matches!(self.peek().kind, TokenKind::Gt) {
                        break;
                    }
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::Gt, "`>` to close generic argument list")?;
            return Ok(TypeAnn::Generic(name, args));
        }
        Ok(TypeAnn::Named(name))
    }

    fn expect_identifier(&mut self, label: &'static str) -> Result<String, ParseError> {
        let cur = self.peek().clone();
        match cur.kind {
            TokenKind::Identifier(n) => {
                self.bump();
                Ok(n)
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof { expected: label }),
            other => Err(ParseError::UnexpectedToken {
                kind: other,
                pos: cur.span.start,
                expected: label,
            }),
        }
    }

    /// Member name after `->` or `?->` — must be an identifier.
    fn expect_member_name(&mut self, label: &'static str) -> Result<String, ParseError> {
        self.expect_identifier(label)
    }

    /// Member name after `::` — accepts either an identifier (method/constant)
    /// or a variable (static property — strip the `$`). The `class` keyword
    /// (`Foo::class`) is deferred for a later slice.
    fn expect_static_member_name(&mut self) -> Result<String, ParseError> {
        let cur = self.peek().clone();
        match cur.kind {
            TokenKind::Identifier(n) => {
                self.bump();
                Ok(n)
            }
            TokenKind::Variable(n) => {
                self.bump();
                Ok(n)
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                expected: "member name after `::`",
            }),
            other => Err(ParseError::UnexpectedToken {
                kind: other,
                pos: cur.span.start,
                expected: "member name after `::`",
            }),
        }
    }

    fn parse_return(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Return, "`return`")?;
        // Bare `return;` (no value) is legal.
        if matches!(self.peek().kind, TokenKind::Semicolon) {
            self.bump();
            return Ok(Stmt::Return(None, self.span_from(start)));
        }
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "`;` after return value")?;
        Ok(Stmt::Return(Some(value), self.span_from(start)))
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::While, "`while`")?;
        self.expect(&TokenKind::LParen, "`(` after `while`")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after while condition")?;
        let body = Box::new(self.parse_stmt()?);
        Ok(Stmt::While {
            cond,
            body,
            span: self.span_from(start),
        })
    }

    fn parse_do_while(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Do, "`do`")?;
        let body = Box::new(self.parse_stmt()?);
        self.expect(&TokenKind::While, "`while` after `do` body")?;
        self.expect(&TokenKind::LParen, "`(` after `while`")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after do-while condition")?;
        self.expect(&TokenKind::Semicolon, "`;` after `do ... while (...)`")?;
        Ok(Stmt::DoWhile {
            body,
            cond,
            span: self.span_from(start),
        })
    }

    /// `for (init; cond; step) <body>` — any slot may be empty.
    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::For, "`for`")?;
        self.expect(&TokenKind::LParen, "`(` after `for`")?;
        let init = if matches!(self.peek().kind, TokenKind::Semicolon) {
            self.bump();
            None
        } else {
            // Init is an expression statement (assignment usually); consume
            // the terminating `;` here so the syntax stays C-flavoured.
            let init_start = self.next_start();
            let init_expr = self.parse_expr()?;
            self.expect(&TokenKind::Semicolon, "`;` after `for` init expression")?;
            Some(Box::new(Stmt::Expr(init_expr, self.span_from(init_start))))
        };
        let cond = if matches!(self.peek().kind, TokenKind::Semicolon) {
            self.bump();
            None
        } else {
            let c = self.parse_expr()?;
            self.expect(&TokenKind::Semicolon, "`;` after `for` condition")?;
            Some(c)
        };
        let step = if matches!(self.peek().kind, TokenKind::RParen) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(&TokenKind::RParen, "`)` after `for` header")?;
        let body = Box::new(self.parse_stmt()?);
        Ok(Stmt::For {
            init,
            cond,
            step,
            body,
            span: self.span_from(start),
        })
    }

    /// `break;` / `continue;` plus optional integer level (PHP allows
    /// `break 2;` to escape two nested loops). `is_break` selects which.
    fn parse_break_continue(&mut self, is_break: bool) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        if is_break {
            self.expect(&TokenKind::Break, "`break`")?;
        } else {
            self.expect(&TokenKind::Continue, "`continue`")?;
        }
        let level = if let TokenKind::Integer(n) = self.peek().kind {
            self.bump();
            Some(n as u32)
        } else {
            None
        };
        let kw = if is_break { "break" } else { "continue" };
        self.expect(&TokenKind::Semicolon, "`;` after break/continue")?;
        let _ = kw;
        Ok(if is_break {
            Stmt::Break(level, self.span_from(start))
        } else {
            Stmt::Continue(level, self.span_from(start))
        })
    }

    fn parse_foreach(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::Foreach, "`foreach`")?;
        self.expect(&TokenKind::LParen, "`(` after `foreach`")?;
        let iter = self.parse_expr()?;
        self.expect(&TokenKind::As, "`as` in foreach")?;
        let first = self.expect_variable("variable in foreach")?;
        // Either `as $value` or `as $key => $value`.
        let (key, value) = if matches!(self.peek().kind, TokenKind::FatArrow) {
            self.bump();
            let value = self.expect_variable("value variable in `foreach ... as $k => $v`")?;
            (Some(first), value)
        } else {
            (None, first)
        };
        self.expect(&TokenKind::RParen, "`)` after foreach binding")?;
        let body = Box::new(self.parse_stmt()?);
        Ok(Stmt::Foreach {
            iter,
            key,
            value,
            body,
            span: self.span_from(start),
        })
    }

    /// Consume a `Variable(name)` token and return its name. Convenience for
    /// the foreach pattern parser.
    fn expect_variable(&mut self, label: &'static str) -> Result<String, ParseError> {
        let cur = self.peek().clone();
        match cur.kind {
            TokenKind::Variable(name) => {
                self.bump();
                Ok(name)
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof { expected: label }),
            other => Err(ParseError::UnexpectedToken {
                kind: other,
                pos: cur.span.start,
                expected: label,
            }),
        }
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        let expr = self.parse_expr()?;
        self.expect(&TokenKind::Semicolon, "`;` after expression statement")?;
        Ok(Stmt::Expr(expr, self.span_from(start)))
    }

    fn parse_block(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::LBrace, "`{` to open block")?;
        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace, "`}` to close block")?;
        Ok(Stmt::Block(stmts, self.span_from(start)))
    }

    fn parse_if(&mut self) -> Result<Stmt, ParseError> {
        let start = self.next_start();
        self.expect(&TokenKind::If, "`if`")?;
        self.expect(&TokenKind::LParen, "`(` after `if`")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "`)` after if condition")?;
        let then = Box::new(self.parse_stmt()?);
        let else_ = self.parse_else_tail()?;
        Ok(Stmt::If {
            cond,
            then,
            else_,
            span: self.span_from(start),
        })
    }

    /// Handle the optional tail of an `if`. Three forms:
    /// - `elseif (cond) <stmt>` (one keyword)
    /// - `else if (cond) <stmt>` (two keywords)
    /// - `else <stmt>`
    /// Both `elseif` forms desugar to nested `If` ASTs.
    fn parse_else_tail(&mut self) -> Result<Option<Box<Stmt>>, ParseError> {
        match self.peek().kind {
            TokenKind::Elseif => {
                let start = self.next_start();
                self.bump();
                self.expect(&TokenKind::LParen, "`(` after `elseif`")?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)` after elseif condition")?;
                let then = Box::new(self.parse_stmt()?);
                let nested = self.parse_else_tail()?;
                Ok(Some(Box::new(Stmt::If {
                    cond,
                    then,
                    else_: nested,
                    span: self.span_from(start),
                })))
            }
            TokenKind::Else => {
                self.bump();
                if matches!(self.peek().kind, TokenKind::If) {
                    // `else if` (two-token form) — recurse into a fresh `if`.
                    Ok(Some(Box::new(self.parse_if()?)))
                } else {
                    Ok(Some(Box::new(self.parse_stmt()?)))
                }
            }
            _ => Ok(None),
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_assignment()
    }

    /// Ternary `? :` is right-associative and lower precedence than `??`
    /// but higher than assignment. Short ternary `?:` is parsed via the
    /// same path (we peek past `?` for `:` to disambiguate).
    fn parse_ternary(&mut self) -> Result<Expr, ParseError> {
        let cond = self.parse_binary(0)?;
        if !matches!(self.peek().kind, TokenKind::Question) {
            return Ok(cond);
        }
        self.bump(); // consume `?`
                     // Short ternary `?:` — middle is empty.
        if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump();
            let else_ = self.parse_ternary()?;
            return Ok(Expr::ShortTernary {
                cond: Box::new(cond),
                else_: Box::new(else_),
            });
        }
        let then = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:` in ternary")?;
        let else_ = self.parse_ternary()?;
        Ok(Expr::Ternary {
            cond: Box::new(cond),
            then: Box::new(then),
            else_: Box::new(else_),
        })
    }

    /// Assignment (and compound assignment) is right-associative and the
    /// lowest precedence operator.
    fn parse_assignment(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_ternary()?;

        // Plain assignment.
        if matches!(self.peek().kind, TokenKind::Eq) {
            self.bump();
            let rhs = self.parse_assignment()?;
            return Ok(Expr::Assign {
                target: Box::new(lhs),
                value: Box::new(rhs),
            });
        }

        // Compound assignment.
        let compound_op = match self.peek().kind {
            TokenKind::PlusEq => Some(BinOp::Add),
            TokenKind::MinusEq => Some(BinOp::Sub),
            TokenKind::StarEq => Some(BinOp::Mul),
            TokenKind::SlashEq => Some(BinOp::Div),
            TokenKind::PercentEq => Some(BinOp::Rem),
            TokenKind::StarStarEq => Some(BinOp::Pow),
            TokenKind::DotEq => Some(BinOp::Concat),
            TokenKind::QuestionQuestionEq => Some(BinOp::Coalesce),
            _ => None,
        };
        if let Some(op) = compound_op {
            self.bump();
            let rhs = self.parse_assignment()?;
            return Ok(Expr::CompoundAssign {
                op,
                target: Box::new(lhs),
                value: Box::new(rhs),
            });
        }

        Ok(lhs)
    }

    /// Pratt / precedence-climbing parser for binary operators. Higher
    /// precedence binds tighter; right-associative ops do not bump
    /// `min_prec` on recursion.
    ///
    /// Precedence levels (descending — modern PHP, with `.` lowered below
    /// `+ -` per PHP 8):
    /// - 12 `**` (right)
    /// - 11 `* / %`
    /// - 10 `+ -`
    /// -  9 `.` (concat)
    /// -  7 `< > <= >=`
    /// -  6 `=== !==`
    /// -  3 `&&`
    /// -  2 `||`
    /// -  1 `??` (right)
    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            // Reject loose-equality early with a clear message rather than
            // silently rewriting it. PHPScript only supports strict equality.
            match self.peek().kind {
                TokenKind::EqEq => {
                    let span = self.peek().span;
                    return Err(ParseError::LooseEqualityRejected {
                        op: "==",
                        suggested: "===",
                        pos: span.start,
                    });
                }
                TokenKind::BangEq => {
                    let span = self.peek().span;
                    return Err(ParseError::LooseEqualityRejected {
                        op: "!=",
                        suggested: "!==",
                        pos: span.start,
                    });
                }
                _ => {}
            }

            let (op, prec, right_assoc) = match self.peek().kind {
                TokenKind::Instanceof => (BinOp::Instanceof, 13, false),
                TokenKind::StarStar => (BinOp::Pow, 12, true),
                TokenKind::Star => (BinOp::Mul, 11, false),
                TokenKind::Slash => (BinOp::Div, 11, false),
                TokenKind::Percent => (BinOp::Rem, 11, false),
                TokenKind::Plus => (BinOp::Add, 10, false),
                TokenKind::Minus => (BinOp::Sub, 10, false),
                TokenKind::Dot => (BinOp::Concat, 9, false),
                TokenKind::Lt => (BinOp::Lt, 7, false),
                TokenKind::Gt => (BinOp::Gt, 7, false),
                TokenKind::LtEq => (BinOp::LtEq, 7, false),
                TokenKind::GtEq => (BinOp::GtEq, 7, false),
                TokenKind::EqEqEq => (BinOp::Eq, 6, false),
                TokenKind::BangEqEq => (BinOp::NotEq, 6, false),
                TokenKind::Spaceship => (BinOp::Spaceship, 6, false),
                TokenKind::AmpAmp => (BinOp::And, 3, false),
                TokenKind::PipePipe => (BinOp::Or, 2, false),
                TokenKind::QuestionQuestion => (BinOp::Coalesce, 1, true),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.bump();
            let next_min = if right_assoc { prec } else { prec + 1 };
            let rhs = self.parse_binary(next_min)?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().kind {
            TokenKind::Minus => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(inner),
                })
            }
            TokenKind::Plus => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnOp::Pos,
                    expr: Box::new(inner),
                })
            }
            TokenKind::PlusPlus => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::IncDec {
                    op: IncDecOp::Inc,
                    fix: IncDecFix::Prefix,
                    target: Box::new(inner),
                })
            }
            TokenKind::MinusMinus => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::IncDec {
                    op: IncDecOp::Dec,
                    fix: IncDecFix::Prefix,
                    target: Box::new(inner),
                })
            }
            TokenKind::Await => {
                self.bump();
                let inner = self.parse_unary()?;
                Ok(Expr::Await(Box::new(inner)))
            }
            _ => self.parse_postfix(),
        }
    }

    /// Postfix operators applied left-to-right after a primary expression:
    /// `(args)` calls and `[key]` index access.
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let start = self.next_start();
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek().kind {
                TokenKind::LParen => {
                    // Reject the deprecated `array(...)` long-form
                    // constructor. PHP itself still accepts it but emits
                    // a "discouraged" notice; PHPScript follows the modern
                    // style and rejects outright.
                    if let Expr::Ident(name) = &expr {
                        if name == "array" {
                            return Err(ParseError::DeprecatedArrayConstructor {
                                pos: self.peek().span.start,
                            });
                        }
                    }
                    self.bump();
                    // PHP 8.1 first-class callable: `target(...)`.
                    // Recognised when the entire arg list is a bare ellipsis.
                    if matches!(self.peek().kind, TokenKind::Ellipsis) {
                        let saved = self.pos;
                        self.bump();
                        if matches!(self.peek().kind, TokenKind::RParen) {
                            self.bump();
                            expr = Expr::FirstClassCallable(Box::new(expr));
                            continue;
                        }
                        self.pos = saved;
                    }
                    let mut args = Vec::new();
                    if !matches!(self.peek().kind, TokenKind::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if matches!(self.peek().kind, TokenKind::Comma) {
                                self.bump();
                                // Trailing comma allowed: `foo(1, 2,)`.
                                if matches!(self.peek().kind, TokenKind::RParen) {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&TokenKind::RParen, "`)` to close call")?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        span: self.span_from(start),
                    };
                }
                TokenKind::LBracket => {
                    self.bump();
                    let key = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "`]` to close index")?;
                    expr = Expr::Index {
                        obj: Box::new(expr),
                        key: Box::new(key),
                    };
                }
                TokenKind::Arrow => {
                    self.bump();
                    let name = self.expect_member_name("member name after `->`")?;
                    expr = Expr::Access {
                        target: Box::new(expr),
                        name,
                        op: AccessOp::Arrow,
                    };
                }
                TokenKind::NullSafeArrow => {
                    self.bump();
                    let name = self.expect_member_name("member name after `?->`")?;
                    expr = Expr::Access {
                        target: Box::new(expr),
                        name,
                        op: AccessOp::NullSafeArrow,
                    };
                }
                TokenKind::ColonColon => {
                    self.bump();
                    // After `::` we accept either an Identifier (method or
                    // class constant) or a Variable (static property `$prop`).
                    // The `$` is stripped so AST is uniform.
                    let name = self.expect_static_member_name()?;
                    expr = Expr::Access {
                        target: Box::new(expr),
                        name,
                        op: AccessOp::DoubleColon,
                    };
                }
                TokenKind::PlusPlus => {
                    self.bump();
                    expr = Expr::IncDec {
                        op: IncDecOp::Inc,
                        fix: IncDecFix::Postfix,
                        target: Box::new(expr),
                    };
                }
                TokenKind::MinusMinus => {
                    self.bump();
                    expr = Expr::IncDec {
                        op: IncDecOp::Dec,
                        fix: IncDecFix::Postfix,
                        target: Box::new(expr),
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// PHP 7.4 arrow function: `fn(params): RetType => expr`. Body is a
    /// single expression; full multi-statement closures use the
    /// `function(...) {...}` form (deferred).
    fn parse_arrow_fn(&mut self) -> Result<Expr, ParseError> {
        self.expect(&TokenKind::Fn, "`fn`")?;
        self.expect(&TokenKind::LParen, "`(` after `fn`")?;
        let params = self.parse_param_list()?;
        self.expect(&TokenKind::RParen, "`)` to close arrow-fn params")?;
        let return_type = if matches!(self.peek().kind, TokenKind::Colon) {
            self.bump();
            Some(self.parse_type_annotation()?)
        } else {
            None
        };
        self.expect(&TokenKind::FatArrow, "`=>` in arrow function")?;
        let body = self.parse_expr()?;
        Ok(Expr::ArrowFn {
            params,
            return_type,
            body: Box::new(body),
        })
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let cur = self.peek().clone();
        match cur.kind {
            TokenKind::Fn => self.parse_arrow_fn(),
            TokenKind::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)` to close grouped expression")?;
                Ok(inner)
            }
            TokenKind::SelfKw => {
                self.bump();
                Ok(Expr::SelfRef)
            }
            TokenKind::Parent => {
                self.bump();
                Ok(Expr::ParentRef)
            }
            TokenKind::Static => {
                self.bump();
                Ok(Expr::StaticRef)
            }
            TokenKind::Match => {
                self.bump();
                self.expect(&TokenKind::LParen, "`(` after `match`")?;
                let scrutinee = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)` after match scrutinee")?;
                self.expect(&TokenKind::LBrace, "`{` to open match body")?;
                let mut arms = Vec::new();
                while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
                    arms.push(self.parse_match_arm()?);
                }
                self.expect(&TokenKind::RBrace, "`}` to close match body")?;
                return Ok(Expr::Match {
                    scrutinee: Box::new(scrutinee),
                    arms,
                });
            }
            TokenKind::New => {
                self.bump();
                let class = self.parse_type_annotation()?;
                self.expect(&TokenKind::LParen, "`(` after `new <ClassName>`")?;
                let mut args = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if matches!(self.peek().kind, TokenKind::Comma) {
                            self.bump();
                            if matches!(self.peek().kind, TokenKind::RParen) {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RParen, "`)` to close `new` arguments")?;
                Ok(Expr::New { class, args })
            }
            TokenKind::LBracket => {
                self.bump();
                let mut items = Vec::new();
                if !matches!(self.peek().kind, TokenKind::RBracket) {
                    loop {
                        let first = self.parse_expr()?;
                        let item = if matches!(self.peek().kind, TokenKind::FatArrow) {
                            self.bump();
                            let value = self.parse_expr()?;
                            ArrayItem {
                                key: Some(first),
                                value,
                            }
                        } else {
                            ArrayItem {
                                key: None,
                                value: first,
                            }
                        };
                        items.push(item);
                        if matches!(self.peek().kind, TokenKind::Comma) {
                            self.bump();
                            // Trailing comma allowed.
                            if matches!(self.peek().kind, TokenKind::RBracket) {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RBracket, "`]` to close array literal")?;
                Ok(Expr::Array(items))
            }
            TokenKind::Integer(value) => {
                self.bump();
                Ok(Expr::Int(value))
            }
            TokenKind::Float(value) => {
                self.bump();
                Ok(Expr::Float(value))
            }
            TokenKind::String(value) => {
                self.bump();
                Ok(Expr::Str(value))
            }
            TokenKind::InterpolatedString(segments) => {
                self.bump();
                let parts = build_interpolated_parts(segments)?;
                Ok(Expr::InterpolatedStr(parts))
            }
            TokenKind::True => {
                self.bump();
                Ok(Expr::Bool(true))
            }
            TokenKind::False => {
                self.bump();
                Ok(Expr::Bool(false))
            }
            TokenKind::Null => {
                self.bump();
                Ok(Expr::Null)
            }
            TokenKind::Variable(name) => {
                self.bump();
                Ok(Expr::Var(name))
            }
            TokenKind::Identifier(name) => {
                self.bump();
                Ok(Expr::Ident(name))
            }
            TokenKind::Eof => Err(ParseError::UnexpectedEof {
                expected: "expression",
            }),
            other => Err(ParseError::UnexpectedToken {
                kind: other,
                pos: cur.span.start,
                expected: "expression",
            }),
        }
    }
}

pub fn parse(source: &str) -> Result<Module, ParseError> {
    let tokens = lex(source)?;
    let mut p = Parser { tokens, pos: 0 };
    p.parse_module()
}

/// Validate the PHP 8.4 rule: the write visibility of an asymmetric
/// property/promoted-param must be ≤ its read visibility. `private public(set)`
/// is invalid; `public private(set)` is the common shape.
fn check_asym_vis(read: Visibility, set: Visibility, pos: u32) -> Result<(), ParseError> {
    let read_rank = vis_rank(read);
    let set_rank = vis_rank(set);
    if set_rank > read_rank {
        return Err(ParseError::AsymVisWriteWiderThanRead {
            read_vis: vis_name(read),
            set_vis: vis_name(set),
            pos,
        });
    }
    Ok(())
}

fn vis_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Private => 1,
        Visibility::Protected => 2,
        Visibility::Public => 3,
    }
}

fn vis_name(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Protected => "protected",
        Visibility::Private => "private",
    }
}

/// Parse a single expression from `source`. Used to lower an interpolation
/// segment's verbatim source (the body of `{$...}`) into an AST sub-tree.
fn parse_expr_from_str(source: &str) -> Result<Expr, ParseError> {
    let tokens = lex(source)?;
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_expr()?;
    if !matches!(p.peek().kind, TokenKind::Eof) {
        let cur = p.peek().clone();
        return Err(ParseError::UnexpectedToken {
            kind: cur.kind,
            pos: cur.span.start,
            expected: "end of interpolation expression",
        });
    }
    Ok(expr)
}

/// Convert lexer segments into AST parts. `Var(name)` becomes `Var(name)`
/// expressions; `Expr(src)` segments are re-parsed via the regular
/// expression parser.
fn build_interpolated_parts(
    segments: Vec<InterpolatedSegment>,
) -> Result<Vec<InterpolatedPart>, ParseError> {
    let mut parts = Vec::with_capacity(segments.len());
    for seg in segments {
        match seg {
            InterpolatedSegment::Literal(s) => parts.push(InterpolatedPart::Lit(s)),
            InterpolatedSegment::Var(name) => {
                parts.push(InterpolatedPart::Expr(Box::new(Expr::Var(name))));
            }
            InterpolatedSegment::Expr(src) => {
                let expr = parse_expr_from_str(&src)?;
                parts.push(InterpolatedPart::Expr(Box::new(expr)));
            }
        }
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_source_to_empty_module() {
        let m = parse("").unwrap();
        assert!(m.stmts.is_empty());
    }

    #[test]
    fn parses_integer_expression_statement() {
        let m = parse("42;").unwrap();
        assert_eq!(m.stmts, vec![Stmt::Expr(Expr::Int(42), Span::DUMMY)]);
    }

    #[test]
    fn parses_multiple_integer_expression_statements() {
        let m = parse("1; 2; 3;").unwrap();
        assert_eq!(
            m.stmts,
            vec![
                Stmt::Expr(Expr::Int(1), Span::DUMMY),
                Stmt::Expr(Expr::Int(2), Span::DUMMY),
                Stmt::Expr(Expr::Int(3), Span::DUMMY),
            ]
        );
    }

    #[test]
    fn missing_semicolon_after_expression_errors() {
        let err = parse("42").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedEof { .. }));
    }

    #[test]
    fn non_expression_token_at_start_errors() {
        let err = parse(";").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedToken { .. }));
    }

    #[test]
    fn parses_float_literal_expression() {
        let m = parse("3.14;").unwrap();
        assert_eq!(m.stmts, vec![Stmt::Expr(Expr::Float(3.14), Span::DUMMY)]);
    }

    #[test]
    fn parses_string_literal_expression() {
        let m = parse(r#""hello";"#).unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(Expr::Str("hello".into()), Span::DUMMY)]
        );
    }

    #[test]
    fn parses_bool_literal_expressions() {
        let m = parse("true; false;").unwrap();
        assert_eq!(
            m.stmts,
            vec![
                Stmt::Expr(Expr::Bool(true), Span::DUMMY),
                Stmt::Expr(Expr::Bool(false), Span::DUMMY),
            ]
        );
    }

    #[test]
    fn parses_null_literal_expression() {
        let m = parse("null;").unwrap();
        assert_eq!(m.stmts, vec![Stmt::Expr(Expr::Null, Span::DUMMY)]);
    }

    #[test]
    fn parses_variable_expression() {
        let m = parse("$x;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(Expr::Var("x".into()), Span::DUMMY)]
        );
    }

    #[test]
    fn parses_simple_assignment() {
        let m = parse("$x = 42;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("x".into())),
                    value: Box::new(Expr::Int(42)),
                },
                Span::DUMMY
            )]
        );
    }

    /// Right-associative: `$a = $b = 1` parses as `$a = ($b = 1)`.
    #[test]
    fn assignment_is_right_associative() {
        let m = parse("$a = $b = 1;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("a".into())),
                    value: Box::new(Expr::Assign {
                        target: Box::new(Expr::Var("b".into())),
                        value: Box::new(Expr::Int(1)),
                    }),
                },
                Span::DUMMY
            )]
        );
    }

    fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::Binary {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        }
    }

    #[test]
    fn parses_simple_addition() {
        let m = parse("1 + 2;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(BinOp::Add, Expr::Int(1), Expr::Int(2)),
                Span::DUMMY
            )]
        );
    }

    /// Multiplication binds tighter than addition.
    #[test]
    fn arithmetic_precedence_mul_over_add() {
        let m = parse("1 + 2 * 3;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Add,
                    Expr::Int(1),
                    bin(BinOp::Mul, Expr::Int(2), Expr::Int(3)),
                ),
                Span::DUMMY
            )]
        );
    }

    /// `+`/`-` are left-associative.
    #[test]
    fn addition_is_left_associative() {
        let m = parse("1 + 2 + 3;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Add,
                    bin(BinOp::Add, Expr::Int(1), Expr::Int(2)),
                    Expr::Int(3),
                ),
                Span::DUMMY
            )]
        );
    }

    /// `**` is right-associative.
    #[test]
    fn power_is_right_associative() {
        let m = parse("2 ** 3 ** 2;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Pow,
                    Expr::Int(2),
                    bin(BinOp::Pow, Expr::Int(3), Expr::Int(2)),
                ),
                Span::DUMMY
            )]
        );
    }

    /// Parentheses override precedence.
    #[test]
    fn parens_override_precedence() {
        let m = parse("(1 + 2) * 3;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Mul,
                    bin(BinOp::Add, Expr::Int(1), Expr::Int(2)),
                    Expr::Int(3),
                ),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_unary_minus() {
        let m = parse("-42;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(Expr::Int(42)),
                },
                Span::DUMMY
            )]
        );
    }

    /// Unary binds tighter than binary: `-1 + 2` is `(-1) + 2`.
    #[test]
    fn unary_minus_binds_tighter_than_addition() {
        let m = parse("-1 + 2;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Add,
                    Expr::Unary {
                        op: UnOp::Neg,
                        expr: Box::new(Expr::Int(1)),
                    },
                    Expr::Int(2),
                ),
                Span::DUMMY
            )]
        );
    }

    // ---------- concat / comparison / logical / coalesce ----------

    fn s(text: &str) -> Expr {
        Expr::Str(text.into())
    }

    #[test]
    fn parses_string_concat() {
        let m = parse(r#""a" . "b";"#).unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(bin(BinOp::Concat, s("a"), s("b")), Span::DUMMY)]
        );
    }

    /// PHP 8: `.` has lower precedence than `+ -`. `1 + 2 . 3` is `(1 + 2) . 3`.
    #[test]
    fn add_binds_tighter_than_concat() {
        let m = parse(r#"1 + 2 . "x";"#).unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Concat,
                    bin(BinOp::Add, Expr::Int(1), Expr::Int(2)),
                    s("x"),
                ),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_strict_equality() {
        let m = parse("1 === 2;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(BinOp::Eq, Expr::Int(1), Expr::Int(2)),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn loose_equality_is_rejected_with_message() {
        let err = parse("1 == 2;").unwrap_err();
        assert!(matches!(
            err,
            ParseError::LooseEqualityRejected {
                op: "==",
                suggested: "===",
                ..
            }
        ));
    }

    #[test]
    fn loose_inequality_is_rejected_with_message() {
        let err = parse("1 != 2;").unwrap_err();
        assert!(matches!(
            err,
            ParseError::LooseEqualityRejected {
                op: "!=",
                suggested: "!==",
                ..
            }
        ));
    }

    #[test]
    fn parses_comparison_operators() {
        // Sample of each comparison, just to nail the dispatch.
        for (src, expected_op) in [
            ("1 < 2;", BinOp::Lt),
            ("1 > 2;", BinOp::Gt),
            ("1 <= 2;", BinOp::LtEq),
            ("1 >= 2;", BinOp::GtEq),
            ("1 !== 2;", BinOp::NotEq),
        ] {
            let m = parse(src).unwrap();
            assert_eq!(
                m.stmts,
                vec![Stmt::Expr(
                    bin(expected_op, Expr::Int(1), Expr::Int(2)),
                    Span::DUMMY
                )],
                "for `{src}`"
            );
        }
    }

    /// `&&` binds tighter than `||`.
    #[test]
    fn logical_and_binds_tighter_than_or() {
        let m = parse("$a || $b && $c;").unwrap();
        let v = |n: &str| Expr::Var(n.into());
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(BinOp::Or, v("a"), bin(BinOp::And, v("b"), v("c")),),
                Span::DUMMY
            )]
        );
    }

    /// `??` is right-associative.
    #[test]
    fn coalesce_is_right_associative() {
        let m = parse("$a ?? $b ?? $c;").unwrap();
        let v = |n: &str| Expr::Var(n.into());
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(
                    BinOp::Coalesce,
                    v("a"),
                    bin(BinOp::Coalesce, v("b"), v("c")),
                ),
                Span::DUMMY
            )]
        );
    }

    /// `??` has lower precedence than `||`.
    #[test]
    fn coalesce_lower_than_logical_or() {
        let m = parse("$a || $b ?? $c;").unwrap();
        let v = |n: &str| Expr::Var(n.into());
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                bin(BinOp::Coalesce, bin(BinOp::Or, v("a"), v("b")), v("c"),),
                Span::DUMMY
            )]
        );
    }

    // ---------- compound assignment ----------

    fn compound_assign(op: BinOp, name: &str, value: Expr) -> Stmt {
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
    fn parses_plus_equals() {
        let m = parse("$x += 1;").unwrap();
        assert_eq!(
            m.stmts,
            vec![compound_assign(BinOp::Add, "x", Expr::Int(1))]
        );
    }

    #[test]
    fn parses_dot_equals_as_concat_compound() {
        let m = parse(r#"$s .= "x";"#).unwrap();
        assert_eq!(
            m.stmts,
            vec![compound_assign(BinOp::Concat, "s", Expr::Str("x".into()))]
        );
    }

    #[test]
    fn parses_coalesce_equals() {
        let m = parse("$x ??= 1;").unwrap();
        assert_eq!(
            m.stmts,
            vec![compound_assign(BinOp::Coalesce, "x", Expr::Int(1))]
        );
    }

    /// `$a += $b += 1` parses as `$a += ($b += 1)` (right-associative).
    #[test]
    fn compound_assignment_is_right_associative() {
        let m = parse("$a += $b += 1;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::CompoundAssign {
                    op: BinOp::Add,
                    target: Box::new(Expr::Var("a".into())),
                    value: Box::new(Expr::CompoundAssign {
                        op: BinOp::Add,
                        target: Box::new(Expr::Var("b".into())),
                        value: Box::new(Expr::Int(1)),
                    }),
                },
                Span::DUMMY
            )]
        );
    }

    /// All compound forms recognised.
    #[test]
    fn parses_all_compound_assignment_forms() {
        let cases = [
            ("$x += 1;", BinOp::Add),
            ("$x -= 1;", BinOp::Sub),
            ("$x *= 1;", BinOp::Mul),
            ("$x /= 1;", BinOp::Div),
            ("$x %= 1;", BinOp::Rem),
            ("$x **= 1;", BinOp::Pow),
            ("$x .= 1;", BinOp::Concat),
            ("$x ??= 1;", BinOp::Coalesce),
        ];
        for (src, expected_op) in cases {
            let m = parse(src).unwrap();
            assert_eq!(
                m.stmts,
                vec![compound_assign(expected_op, "x", Expr::Int(1))],
                "for `{src}`"
            );
        }
    }

    // ---------- blocks + if/elseif/else ----------

    #[test]
    fn parses_empty_block() {
        let m = parse("{}").unwrap();
        assert_eq!(m.stmts, vec![Stmt::Block(vec![], Span::DUMMY)]);
    }

    #[test]
    fn parses_block_with_statements() {
        let m = parse("{ $x = 1; $y = 2; }").unwrap();
        let inner = vec![
            Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("x".into())),
                    value: Box::new(Expr::Int(1)),
                },
                Span::DUMMY,
            ),
            Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("y".into())),
                    value: Box::new(Expr::Int(2)),
                },
                Span::DUMMY,
            ),
        ];
        assert_eq!(m.stmts, vec![Stmt::Block(inner, Span::DUMMY)]);
    }

    #[test]
    fn parses_simple_if_with_block() {
        let m = parse("if ($x) { $y = 1; }").unwrap();
        let then_block = Stmt::Block(
            vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("y".into())),
                    value: Box::new(Expr::Int(1)),
                },
                Span::DUMMY,
            )],
            Span::DUMMY,
        );
        assert_eq!(
            m.stmts,
            vec![Stmt::If {
                cond: Expr::Var("x".into()),
                then: Box::new(then_block),
                else_: None,
                span: Span::DUMMY,
            }]
        );
    }

    #[test]
    fn parses_if_else() {
        let m = parse("if ($x) { 1; } else { 2; }").unwrap();
        let then_block = Stmt::Block(vec![Stmt::Expr(Expr::Int(1), Span::DUMMY)], Span::DUMMY);
        let else_block = Stmt::Block(vec![Stmt::Expr(Expr::Int(2), Span::DUMMY)], Span::DUMMY);
        assert_eq!(
            m.stmts,
            vec![Stmt::If {
                cond: Expr::Var("x".into()),
                then: Box::new(then_block),
                else_: Some(Box::new(else_block)),
                span: Span::DUMMY,
            }]
        );
    }

    /// `elseif` and `else if` should produce the same AST shape.
    #[test]
    fn elseif_keyword_and_else_if_two_token_form_match() {
        let one = parse("if (1) { 1; } elseif (2) { 2; }").unwrap();
        let two = parse("if (1) { 1; } else if (2) { 2; }").unwrap();
        assert_eq!(one.stmts, two.stmts);
    }

    #[test]
    fn parses_if_elseif_else() {
        let m = parse("if ($a) { 1; } elseif ($b) { 2; } else { 3; }").unwrap();
        // Expected: If(a, {1}, Some(If(b, {2}, Some({3}))))
        let inner_else = Stmt::Block(vec![Stmt::Expr(Expr::Int(3), Span::DUMMY)], Span::DUMMY);
        let inner_if = Stmt::If {
            cond: Expr::Var("b".into()),
            then: Box::new(Stmt::Block(
                vec![Stmt::Expr(Expr::Int(2), Span::DUMMY)],
                Span::DUMMY,
            )),
            else_: Some(Box::new(inner_else)),
            span: Span::DUMMY,
        };
        let outer = Stmt::If {
            cond: Expr::Var("a".into()),
            then: Box::new(Stmt::Block(
                vec![Stmt::Expr(Expr::Int(1), Span::DUMMY)],
                Span::DUMMY,
            )),
            else_: Some(Box::new(inner_if)),
            span: Span::DUMMY,
        };
        assert_eq!(m.stmts, vec![outer]);
    }

    /// `if` without braces — single statement body.
    #[test]
    fn parses_if_with_unbraced_body() {
        let m = parse("if ($x) 42;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::If {
                cond: Expr::Var("x".into()),
                then: Box::new(Stmt::Expr(Expr::Int(42), Span::DUMMY)),
                else_: None,
                span: Span::DUMMY,
            }]
        );
    }

    /// Missing `(` after `if` is a clear parse error.
    #[test]
    fn if_without_paren_errors() {
        let err = parse("if $x { }").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedToken { .. }));
    }

    // ---------- return / while / foreach ----------

    #[test]
    fn parses_bare_return() {
        let m = parse("return;").unwrap();
        assert_eq!(m.stmts, vec![Stmt::Return(None, Span::DUMMY)]);
    }

    #[test]
    fn parses_return_with_expression() {
        let m = parse("return 42;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Return(Some(Expr::Int(42)), Span::DUMMY)]
        );
    }

    #[test]
    fn parses_while_loop() {
        let m = parse("while ($x) { $x = 1; }").unwrap();
        let body = Stmt::Block(
            vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Var("x".into())),
                    value: Box::new(Expr::Int(1)),
                },
                Span::DUMMY,
            )],
            Span::DUMMY,
        );
        assert_eq!(
            m.stmts,
            vec![Stmt::While {
                cond: Expr::Var("x".into()),
                body: Box::new(body),
                span: Span::DUMMY,
            }]
        );
    }

    #[test]
    fn parses_foreach_value_only() {
        let m = parse("foreach ($items as $item) { $item; }").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Foreach {
                iter: Expr::Var("items".into()),
                key: None,
                value: "item".into(),
                body: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Var("item".into()), Span::DUMMY)],
                    Span::DUMMY
                )),
                span: Span::DUMMY,
            }]
        );
    }

    #[test]
    fn parses_foreach_key_value() {
        let m = parse("foreach ($items as $k => $v) { $v; }").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Foreach {
                iter: Expr::Var("items".into()),
                key: Some("k".into()),
                value: "v".into(),
                body: Box::new(Stmt::Block(
                    vec![Stmt::Expr(Expr::Var("v".into()), Span::DUMMY)],
                    Span::DUMMY
                )),
                span: Span::DUMMY,
            }]
        );
    }

    #[test]
    fn foreach_without_as_errors() {
        let err = parse("foreach ($items $item) { }").unwrap_err();
        assert!(
            matches!(err, ParseError::UnexpectedToken { expected, .. } if expected.contains("as"))
        );
    }

    // ---------- identifiers + calls ----------

    #[test]
    fn parses_bare_identifier_expression() {
        let m = parse("foo;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(Expr::Ident("foo".into()), Span::DUMMY)]
        );
    }

    #[test]
    fn parses_simple_function_call() {
        let m = parse("foo(1, 2);").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(Expr::Ident("foo".into())),
                    args: vec![Expr::Int(1), Expr::Int(2)],
                    span: Span::DUMMY,
                },
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_call_with_no_args() {
        let m = parse("foo();").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(Expr::Ident("foo".into())),
                    args: vec![],
                    span: Span::DUMMY,
                },
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_call_with_trailing_comma() {
        let m = parse("foo(1, 2,);").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(Expr::Ident("foo".into())),
                    args: vec![Expr::Int(1), Expr::Int(2)],
                    span: Span::DUMMY,
                },
                Span::DUMMY
            )]
        );
    }

    /// `f(g(1))` — nested call.
    #[test]
    fn parses_nested_call() {
        let m = parse("f(g(1));").unwrap();
        let inner = Expr::Call {
            callee: Box::new(Expr::Ident("g".into())),
            args: vec![Expr::Int(1)],
            span: Span::DUMMY,
        };
        let outer = Expr::Call {
            callee: Box::new(Expr::Ident("f".into())),
            args: vec![inner],
            span: Span::DUMMY,
        };
        assert_eq!(m.stmts, vec![Stmt::Expr(outer, Span::DUMMY)]);
    }

    /// `f()()` — call result is itself called.
    #[test]
    fn parses_chained_call() {
        let m = parse("f()();").unwrap();
        let first = Expr::Call {
            callee: Box::new(Expr::Ident("f".into())),
            args: vec![],
            span: Span::DUMMY,
        };
        let chained = Expr::Call {
            callee: Box::new(first),
            args: vec![],
            span: Span::DUMMY,
        };
        assert_eq!(m.stmts, vec![Stmt::Expr(chained, Span::DUMMY)]);
    }

    /// Variable callee: `$cb(1, 2)` — calls a function stored in a variable.
    #[test]
    fn parses_call_on_variable() {
        let m = parse("$cb(1, 2);").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(Expr::Var("cb".into())),
                    args: vec![Expr::Int(1), Expr::Int(2)],
                    span: Span::DUMMY,
                },
                Span::DUMMY
            )]
        );
    }

    // ---------- type annotations ----------

    fn t_named(s: &str) -> TypeAnn {
        TypeAnn::Named(s.into())
    }

    /// Helper to parse a type annotation in isolation by sticking it into a
    /// minimal function decl `function f(): T {}` and returning the return
    /// type.
    fn parse_type(src: &str) -> TypeAnn {
        let module = parse(&format!("function f(): {src} {{}}")).unwrap();
        match &module.stmts[0] {
            Stmt::Function(decl) => decl.return_type.clone().expect("expected return type"),
            other => panic!("expected function decl, got {other:?}"),
        }
    }

    #[test]
    fn parses_named_type() {
        assert_eq!(parse_type("int"), t_named("int"));
        assert_eq!(parse_type("string"), t_named("string"));
        assert_eq!(parse_type("MyClass"), t_named("MyClass"));
    }

    #[test]
    fn parses_nullable_type() {
        assert_eq!(
            parse_type("?int"),
            TypeAnn::Nullable(Box::new(t_named("int")))
        );
    }

    #[test]
    fn parses_union_type() {
        assert_eq!(
            parse_type("int|string"),
            TypeAnn::Union(vec![t_named("int"), t_named("string")])
        );
    }

    #[test]
    fn parses_three_way_union() {
        assert_eq!(
            parse_type("int|string|null"),
            TypeAnn::Union(vec![t_named("int"), t_named("string"), t_named("null")])
        );
    }

    #[test]
    fn parses_array_with_one_param() {
        assert_eq!(
            parse_type("array<int>"),
            TypeAnn::Generic("array".into(), vec![t_named("int")])
        );
    }

    #[test]
    fn parses_array_with_two_params() {
        assert_eq!(
            parse_type("array<string, User>"),
            TypeAnn::Generic("array".into(), vec![t_named("string"), t_named("User")])
        );
    }

    #[test]
    fn parses_nested_generic() {
        // array<array<int>>
        assert_eq!(
            parse_type("array<array<int>>"),
            TypeAnn::Generic(
                "array".into(),
                vec![TypeAnn::Generic("array".into(), vec![t_named("int")])]
            )
        );
    }

    // ---------- function declarations ----------

    #[test]
    fn parses_zero_arg_function_no_return_type() {
        let m = parse("function greet() {}").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Function(FunctionDecl {
                name: "greet".into(),
                params: vec![],
                return_type: None,
                body: vec![],
                async_: false,
                span: Span::DUMMY,
            })]
        );
    }

    #[test]
    fn parses_function_with_typed_params_and_return() {
        let m = parse("function add(int $a, int $b): int { return $a + $b; }").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Function(FunctionDecl {
                name: "add".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: Some(t_named("int")),
                        default: None,
                        promotion: None
                    },
                    Param {
                        name: "b".into(),
                        ty: Some(t_named("int")),
                        default: None,
                        promotion: None
                    },
                ],
                return_type: Some(t_named("int")),
                async_: false,
                body: vec![Stmt::Return(
                    Some(bin(
                        BinOp::Add,
                        Expr::Var("a".into()),
                        Expr::Var("b".into()),
                    )),
                    Span::DUMMY
                )],
                span: Span::DUMMY,
            })]
        );
    }

    #[test]
    fn parses_function_with_default_value() {
        let m =
            parse(r#"function greet(string $name = "world"): string { return $name; }"#).unwrap();
        if let Stmt::Function(d) = &m.stmts[0] {
            assert_eq!(d.params.len(), 1);
            assert_eq!(d.params[0].default, Some(Expr::Str("world".into())));
        } else {
            panic!("expected function decl");
        }
    }

    #[test]
    fn parses_function_with_untyped_params() {
        let m = parse("function f($a, $b) { return $a; }").unwrap();
        if let Stmt::Function(d) = &m.stmts[0] {
            assert_eq!(d.params.len(), 2);
            assert!(d.params[0].ty.is_none());
            assert!(d.params[1].ty.is_none());
        } else {
            panic!("expected function decl");
        }
    }

    #[test]
    fn parses_function_with_nullable_return_type() {
        let m = parse("function maybe(): ?string {}").unwrap();
        if let Stmt::Function(d) = &m.stmts[0] {
            assert_eq!(
                d.return_type,
                Some(TypeAnn::Nullable(Box::new(t_named("string"))))
            );
        } else {
            panic!("expected function decl");
        }
    }

    #[test]
    fn parses_function_with_union_return_type() {
        let m = parse("function multi(): int|string|null {}").unwrap();
        if let Stmt::Function(d) = &m.stmts[0] {
            assert_eq!(
                d.return_type,
                Some(TypeAnn::Union(vec![
                    t_named("int"),
                    t_named("string"),
                    t_named("null")
                ]))
            );
        } else {
            panic!("expected function decl");
        }
    }

    // ---------- arrays ----------

    fn lit(values: Vec<Expr>) -> Expr {
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

    #[test]
    fn parses_empty_array_literal() {
        let m = parse("[];").unwrap();
        assert_eq!(m.stmts, vec![Stmt::Expr(Expr::Array(vec![]), Span::DUMMY)]);
    }

    #[test]
    fn parses_unkeyed_array_literal() {
        let m = parse("[1, 2, 3];").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                lit(vec![Expr::Int(1), Expr::Int(2), Expr::Int(3)]),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_keyed_array_literal() {
        let m = parse(r#"["a" => 1, "b" => 2];"#).unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Array(vec![
                    ArrayItem {
                        key: Some(Expr::Str("a".into())),
                        value: Expr::Int(1),
                    },
                    ArrayItem {
                        key: Some(Expr::Str("b".into())),
                        value: Expr::Int(2),
                    },
                ]),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_array_with_trailing_comma() {
        let m = parse("[1, 2,];").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                lit(vec![Expr::Int(1), Expr::Int(2)]),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_array_index_access() {
        let m = parse("$arr[0];").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Index {
                    obj: Box::new(Expr::Var("arr".into())),
                    key: Box::new(Expr::Int(0)),
                },
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_chained_index_access() {
        let m = parse(r#"$grid[0][1];"#).unwrap();
        let inner = Expr::Index {
            obj: Box::new(Expr::Var("grid".into())),
            key: Box::new(Expr::Int(0)),
        };
        let outer = Expr::Index {
            obj: Box::new(inner),
            key: Box::new(Expr::Int(1)),
        };
        assert_eq!(m.stmts, vec![Stmt::Expr(outer, Span::DUMMY)]);
    }

    // ---------- namespace + use ----------

    #[test]
    fn parses_simple_namespace() {
        let m = parse("namespace App;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Namespace(vec!["App".into()], Span::DUMMY)]
        );
    }

    #[test]
    fn parses_multi_segment_namespace() {
        let m = parse("namespace App\\Models\\User;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Namespace(
                vec!["App".into(), "Models".into(), "User".into(),],
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_namespace_with_leading_backslash() {
        // Permissively accepted; not preserved in AST.
        let m = parse("namespace \\App\\Models;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Namespace(
                vec!["App".into(), "Models".into()],
                Span::DUMMY
            )]
        );
    }

    fn use_class(path: Vec<&str>, alias: Option<&str>) -> Stmt {
        Stmt::Use(UseStmt {
            kind: UseKind::Class,
            items: vec![UseItem {
                path: path.into_iter().map(String::from).collect(),
                alias: alias.map(String::from),
            }],
            span: Span::DUMMY,
        })
    }

    #[test]
    fn parses_simple_use() {
        let m = parse("use App\\Models\\User;").unwrap();
        assert_eq!(
            m.stmts,
            vec![use_class(vec!["App", "Models", "User"], None)]
        );
    }

    #[test]
    fn parses_aliased_use() {
        let m = parse("use App\\Models\\User as U;").unwrap();
        assert_eq!(
            m.stmts,
            vec![use_class(vec!["App", "Models", "User"], Some("U"))]
        );
    }

    #[test]
    fn parses_use_function() {
        let m = parse("use function App\\Util\\format;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Use(UseStmt {
                kind: UseKind::Function,
                items: vec![UseItem {
                    path: vec!["App".into(), "Util".into(), "format".into()],
                    alias: None,
                }],
                span: Span::DUMMY,
            })]
        );
    }

    #[test]
    fn parses_use_const() {
        let m = parse("use const App\\Constants\\PI;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Use(UseStmt {
                kind: UseKind::Const,
                items: vec![UseItem {
                    path: vec!["App".into(), "Constants".into(), "PI".into()],
                    alias: None,
                }],
                span: Span::DUMMY,
            })]
        );
    }

    #[test]
    fn parses_grouped_use() {
        let m = parse("use App\\Models\\{User, Profile};").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Use(UseStmt {
                kind: UseKind::Class,
                items: vec![
                    UseItem {
                        path: vec!["App".into(), "Models".into(), "User".into()],
                        alias: None,
                    },
                    UseItem {
                        path: vec!["App".into(), "Models".into(), "Profile".into()],
                        alias: None,
                    },
                ],
                span: Span::DUMMY,
            })]
        );
    }

    #[test]
    fn parses_grouped_use_with_aliases() {
        let m = parse("use App\\Models\\{User, Profile as P};").unwrap();
        if let Stmt::Use(u) = &m.stmts[0] {
            assert_eq!(u.items.len(), 2);
            assert_eq!(u.items[0].alias, None);
            assert_eq!(u.items[1].alias, Some("P".into()));
        } else {
            panic!("expected Stmt::Use");
        }
    }

    #[test]
    fn parses_grouped_use_with_trailing_comma() {
        let m = parse("use App\\Models\\{User, Profile,};").unwrap();
        if let Stmt::Use(u) = &m.stmts[0] {
            assert_eq!(u.items.len(), 2);
        } else {
            panic!("expected Stmt::Use");
        }
    }

    #[test]
    fn parses_grouped_use_with_nested_segments() {
        // PHP allows `use Foo\{Sub\Bar, Baz};` — the group items can have
        // their own multi-segment paths.
        let m = parse("use App\\{Models\\User, Profile};").unwrap();
        if let Stmt::Use(u) = &m.stmts[0] {
            assert_eq!(
                u.items[0].path,
                vec!["App".to_string(), "Models".to_string(), "User".to_string()]
            );
            assert_eq!(
                u.items[1].path,
                vec!["App".to_string(), "Profile".to_string()]
            );
        } else {
            panic!("expected Stmt::Use");
        }
    }

    #[test]
    fn parses_array_assignment() {
        let m = parse("$arr[0] = 5;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(Expr::Index {
                        obj: Box::new(Expr::Var("arr".into())),
                        key: Box::new(Expr::Int(0)),
                    }),
                    value: Box::new(Expr::Int(5)),
                },
                Span::DUMMY
            )]
        );
    }

    // ---------- member access ----------

    fn access(target: Expr, name: &str, op: AccessOp) -> Expr {
        Expr::Access {
            target: Box::new(target),
            name: name.into(),
            op,
        }
    }

    #[test]
    fn parses_arrow_member_access() {
        let m = parse("$user->email;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                access(Expr::Var("user".into()), "email", AccessOp::Arrow),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_null_safe_arrow_access() {
        let m = parse("$user?->email;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                access(Expr::Var("user".into()), "email", AccessOp::NullSafeArrow),
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_double_colon_method_access() {
        let m = parse("Foo::bar;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                access(Expr::Ident("Foo".into()), "bar", AccessOp::DoubleColon),
                Span::DUMMY
            )]
        );
    }

    /// `Foo::$staticProp` — `$` stripped from the static-prop name.
    #[test]
    fn parses_double_colon_static_property_access_strips_dollar() {
        let m = parse("Foo::$count;").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                access(Expr::Ident("Foo".into()), "count", AccessOp::DoubleColon),
                Span::DUMMY
            )]
        );
    }

    /// `$obj->method($x)` — Access wrapped in Call.
    #[test]
    fn parses_method_call_via_arrow() {
        let m = parse("$obj->method(1);").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Call {
                    callee: Box::new(access(Expr::Var("obj".into()), "method", AccessOp::Arrow)),
                    args: vec![Expr::Int(1)],
                    span: Span::DUMMY,
                },
                Span::DUMMY
            )]
        );
    }

    /// `$user->profile?->avatar->url` — chained mixed-op access.
    #[test]
    fn parses_chained_mixed_member_access() {
        let m = parse("$user->profile?->avatar->url;").unwrap();
        let expected = access(
            access(
                access(Expr::Var("user".into()), "profile", AccessOp::Arrow),
                "avatar",
                AccessOp::NullSafeArrow,
            ),
            "url",
            AccessOp::Arrow,
        );
        assert_eq!(m.stmts, vec![Stmt::Expr(expected, Span::DUMMY)]);
    }

    /// Property writes via `->`: `$this->name = "x"` parses as Assign with
    /// Access target.
    #[test]
    fn parses_property_assignment() {
        let m = parse("$this->name = \"Ada\";").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::Assign {
                    target: Box::new(access(Expr::Var("this".into()), "name", AccessOp::Arrow,)),
                    value: Box::new(Expr::Str("Ada".into())),
                },
                Span::DUMMY
            )]
        );
    }

    // ---------- new expression ----------

    #[test]
    fn parses_new_with_no_args() {
        let m = parse("new Foo();").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::New {
                    class: t_named("Foo"),
                    args: vec![],
                },
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_new_with_args() {
        let m = parse(r#"new User("Ada", 36);"#).unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::New {
                    class: t_named("User"),
                    args: vec![Expr::Str("Ada".into()), Expr::Int(36)],
                },
                Span::DUMMY
            )]
        );
    }

    #[test]
    fn parses_new_with_generic_class() {
        let m = parse("new Box<int>(5);").unwrap();
        assert_eq!(
            m.stmts,
            vec![Stmt::Expr(
                Expr::New {
                    class: TypeAnn::Generic("Box".into(), vec![t_named("int")]),
                    args: vec![Expr::Int(5)],
                },
                Span::DUMMY
            )]
        );
    }

    /// `new Foo()->bar()` — chained access on a fresh instance.
    #[test]
    fn parses_member_access_on_new_expression() {
        let m = parse("new Foo()->bar();").unwrap();
        let new_expr = Expr::New {
            class: t_named("Foo"),
            args: vec![],
        };
        let access_expr = access(new_expr, "bar", AccessOp::Arrow);
        let call_expr = Expr::Call {
            callee: Box::new(access_expr),
            args: vec![],
            span: Span::DUMMY,
        };
        assert_eq!(m.stmts, vec![Stmt::Expr(call_expr, Span::DUMMY)]);
    }

    #[test]
    fn parses_new_requires_parens() {
        let err = parse("new Foo;").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedToken { .. }));
    }

    // ---------- class skeleton ----------

    fn empty_class(name: &str) -> Stmt {
        Stmt::Class(Class {
            name: name.into(),
            abstract_: false,
            final_: false,
            readonly: false,
            extends: None,
            implements: Vec::new(),
            members: Vec::new(),
            span: Span::DUMMY,
        })
    }

    #[test]
    fn parses_empty_class() {
        let m = parse("class User {}").unwrap();
        assert_eq!(m.stmts, vec![empty_class("User")]);
    }

    #[test]
    fn parses_class_with_typed_property() {
        let m = parse("class User { public string $name; }").unwrap();
        let class = match &m.stmts[0] {
            Stmt::Class(c) => c,
            _ => panic!(),
        };
        assert_eq!(class.name, "User");
        assert_eq!(class.members.len(), 1);
        if let ClassMember::Property(p) = &class.members[0] {
            assert_eq!(p.visibility, Visibility::Public);
            assert_eq!(p.name, "name");
            assert_eq!(p.ty, Some(t_named("string")));
            assert!(p.default.is_none());
        } else {
            panic!("expected property");
        }
    }

    #[test]
    fn parses_class_property_with_default() {
        let m = parse("class C { private int $count = 0; }").unwrap();
        let class = match &m.stmts[0] {
            Stmt::Class(c) => c,
            _ => panic!(),
        };
        if let ClassMember::Property(p) = &class.members[0] {
            assert_eq!(p.visibility, Visibility::Private);
            assert_eq!(p.default, Some(Expr::Int(0)));
        } else {
            panic!();
        }
    }

    #[test]
    fn defaults_to_public_when_visibility_omitted() {
        // PHP allows omitting visibility (defaults to public). Modern style
        // discourages it but we still parse it.
        let m = parse("class C { string $name; }").unwrap();
        let class = match &m.stmts[0] {
            Stmt::Class(c) => c,
            _ => panic!(),
        };
        if let ClassMember::Property(p) = &class.members[0] {
            assert_eq!(p.visibility, Visibility::Public);
        } else {
            panic!();
        }
    }

    #[test]
    fn parses_class_method() {
        let m = parse("class C { public function greet(string $name): string { return $name; } }")
            .unwrap();
        let class = match &m.stmts[0] {
            Stmt::Class(c) => c,
            _ => panic!(),
        };
        if let ClassMember::Method(method) = &class.members[0] {
            assert_eq!(method.visibility, Visibility::Public);
            assert_eq!(method.name, "greet");
            assert_eq!(method.params.len(), 1);
            assert_eq!(method.return_type, Some(t_named("string")));
            assert!(method.body.is_some());
        } else {
            panic!("expected method");
        }
    }

    #[test]
    fn parses_class_with_constructor() {
        let m = parse(
            "class User { public function __construct(string $name) { $this->name = $name; } }",
        )
        .unwrap();
        let class = match &m.stmts[0] {
            Stmt::Class(c) => c,
            _ => panic!(),
        };
        if let ClassMember::Method(method) = &class.members[0] {
            assert_eq!(method.name, "__construct");
        } else {
            panic!();
        }
    }

    // ---------- constructor property promotion ----------

    fn first_method(m: &Module) -> &Method {
        match &m.stmts[0] {
            Stmt::Class(c) => match &c.members[0] {
                ClassMember::Method(method) => method,
                _ => panic!("expected method"),
            },
            _ => panic!("expected class"),
        }
    }

    #[test]
    fn parses_promoted_param_records_visibility() {
        let m =
            parse("class User { public function __construct(public string $name) {} }").unwrap();
        let method = first_method(&m);
        assert_eq!(method.params.len(), 1);
        let p = &method.params[0];
        assert_eq!(p.name, "name");
        assert_eq!(
            p.promotion,
            Some(Promotion {
                visibility: Visibility::Public,
                set_visibility: None,
                readonly: false,
            })
        );
    }

    #[test]
    fn parses_promoted_param_with_readonly() {
        let m = parse("class C { public function __construct(public readonly string $id) {} }")
            .unwrap();
        let p = &first_method(&m).params[0];
        assert_eq!(
            p.promotion,
            Some(Promotion {
                visibility: Visibility::Public,
                set_visibility: None,
                readonly: true,
            })
        );
    }

    /// PHP allows `readonly` before visibility too.
    #[test]
    fn promoted_param_modifier_order_is_flexible() {
        let m = parse("class C { public function __construct(readonly public string $id) {} }")
            .unwrap();
        let p = &first_method(&m).params[0];
        assert_eq!(
            p.promotion,
            Some(Promotion {
                visibility: Visibility::Public,
                set_visibility: None,
                readonly: true,
            })
        );
    }

    /// Mixed promoted + regular params.
    #[test]
    fn parses_mixed_promoted_and_regular_params() {
        let m = parse(
            "class C { public function __construct(string $external, public int $count) {} }",
        )
        .unwrap();
        let method = first_method(&m);
        assert_eq!(method.params.len(), 2);
        assert!(method.params[0].promotion.is_none());
        assert_eq!(
            method.params[1].promotion,
            Some(Promotion {
                visibility: Visibility::Public,
                set_visibility: None,
                readonly: false,
            })
        );
    }

    /// `private ?int $x = null` — promoted param with default.
    #[test]
    fn promoted_param_with_default_value() {
        let m =
            parse("class C { public function __construct(private ?int $age = null) {} }").unwrap();
        let p = &first_method(&m).params[0];
        assert_eq!(p.promotion.unwrap().visibility, Visibility::Private);
        assert_eq!(p.default, Some(Expr::Null));
    }

    /// PHP 8.4 asymmetric visibility — the real syntax.
    /// Read side bare, write side qualified with `(set)`.
    #[test]
    fn parses_asym_visibility_public_private_set() {
        let m = parse("class U { public private(set) string $name; }").unwrap();
        let Stmt::Class(c) = &m.stmts[0] else {
            panic!("expected class")
        };
        let ClassMember::Property(p) = &c.members[0] else {
            panic!("expected property")
        };
        assert_eq!(p.visibility, Visibility::Public);
        assert_eq!(p.set_visibility, Some(Visibility::Private));
    }

    #[test]
    fn parses_asym_visibility_protected_private_set() {
        let m = parse("class U { protected private(set) int $id; }").unwrap();
        let Stmt::Class(c) = &m.stmts[0] else {
            panic!("expected class")
        };
        let ClassMember::Property(p) = &c.members[0] else {
            panic!("expected property")
        };
        assert_eq!(p.visibility, Visibility::Protected);
        assert_eq!(p.set_visibility, Some(Visibility::Private));
    }

    /// PHP 8.4 forbids a write side broader than the read side.
    /// `private public(set)` is invalid because public > private.
    #[test]
    fn rejects_asym_visibility_with_wider_write_side() {
        let err = parse("class U { private public(set) string $x; }").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("write side") && msg.contains("more permissive"),
            "got: {msg}"
        );
    }

    /// PHP 8.4 has no `(get)` qualifier — only `(set)`. Anything else
    /// after a visibility keyword should be a parse error (the lingering
    /// `(` trips downstream when a type or identifier is expected).
    #[test]
    fn rejects_unknown_visibility_qualifier() {
        let err = parse("class U { public(get) private(set) string $x; }").unwrap_err();
        assert!(matches!(err, ParseError::UnexpectedToken { .. }));
    }

    /// Promoted constructor params should accept asymmetric visibility too.
    #[test]
    fn parses_asym_visibility_on_promoted_constructor_param() {
        let m =
            parse("class U { public function __construct(public private(set) string $name) {} }")
                .unwrap();
        let promo = first_method(&m).params[0].promotion.as_ref().unwrap();
        assert_eq!(promo.visibility, Visibility::Public);
        assert_eq!(promo.set_visibility, Some(Visibility::Private));
    }

    // ---------- PHP 7.4 arrow functions ----------

    #[test]
    fn parses_arrow_fn_with_types() {
        let m = parse("$f = fn(int $x): int => $x * 2;").unwrap();
        let Stmt::Expr(Expr::Assign { value, .. }, _) = &m.stmts[0] else {
            panic!("expected assignment");
        };
        assert!(matches!(value.as_ref(), Expr::ArrowFn { .. }));
    }

    #[test]
    fn parses_arrow_fn_without_types() {
        let m = parse("$id = fn($x) => $x;").unwrap();
        assert!(matches!(&m.stmts[0], Stmt::Expr(Expr::Assign { .. }, _)));
    }

    // ---------- ++ / -- ----------

    #[test]
    fn parses_postfix_increment() {
        let m = parse("$a = 0; $a++;").unwrap();
        let Stmt::Expr(e, _) = &m.stmts[1] else {
            panic!();
        };
        assert!(matches!(
            e,
            Expr::IncDec {
                op: IncDecOp::Inc,
                fix: IncDecFix::Postfix,
                ..
            }
        ));
    }

    #[test]
    fn parses_prefix_decrement() {
        let m = parse("$a = 0; --$a;").unwrap();
        let Stmt::Expr(e, _) = &m.stmts[1] else {
            panic!();
        };
        assert!(matches!(
            e,
            Expr::IncDec {
                op: IncDecOp::Dec,
                fix: IncDecFix::Prefix,
                ..
            }
        ));
    }

    // ---------- for / do-while / break / continue ----------

    #[test]
    fn parses_c_style_for_loop() {
        let m = parse("for ($i = 0; $i < 5; $i++) { $x = $i; }").unwrap();
        assert!(matches!(&m.stmts[0], Stmt::For { .. }));
    }

    #[test]
    fn parses_for_with_empty_slots() {
        let m = parse("for (;;) { break; }").unwrap();
        let Stmt::For {
            init, cond, step, ..
        } = &m.stmts[0]
        else {
            panic!();
        };
        assert!(init.is_none());
        assert!(cond.is_none());
        assert!(step.is_none());
    }

    #[test]
    fn parses_do_while_loop() {
        let m = parse("do { $i = 1; } while ($i < 10);").unwrap();
        assert!(matches!(&m.stmts[0], Stmt::DoWhile { .. }));
    }

    #[test]
    fn parses_break_and_continue() {
        let m = parse("while (true) { break; continue; }").unwrap();
        let Stmt::While { body, .. } = &m.stmts[0] else {
            panic!();
        };
        let Stmt::Block(stmts, _) = body.as_ref() else {
            panic!();
        };
        assert!(matches!(stmts[0], Stmt::Break(None, _)));
        assert!(matches!(stmts[1], Stmt::Continue(None, _)));
    }

    #[test]
    fn parses_break_with_level() {
        let m = parse("while (true) { while (true) { break 2; } }").unwrap();
        let Stmt::While { body, .. } = &m.stmts[0] else {
            panic!();
        };
        let Stmt::Block(outer_stmts, _) = body.as_ref() else {
            panic!();
        };
        let Stmt::While { body: inner, .. } = &outer_stmts[0] else {
            panic!();
        };
        let Stmt::Block(inner_stmts, _) = inner.as_ref() else {
            panic!();
        };
        assert!(matches!(inner_stmts[0], Stmt::Break(Some(2), _)));
    }

    // ---------- Deprecated constructs rejected ----------

    #[test]
    fn rejects_deprecated_var_keyword_on_property() {
        let err = parse("class C { var $x = 5; }").unwrap_err();
        assert!(matches!(err, ParseError::DeprecatedVarKeyword { .. }));
    }

    #[test]
    fn rejects_deprecated_array_constructor() {
        let err = parse("$xs = array(1, 2, 3);").unwrap_err();
        assert!(matches!(err, ParseError::DeprecatedArrayConstructor { .. }));
    }

    fn first_use_trait_block(m: &Module) -> &UseTraitBlock {
        let Stmt::Class(class) = m.stmts.first().expect("at least one stmt") else {
            panic!("expected a class declaration");
        };
        for mem in &class.members {
            if let ClassMember::UseTrait(block) = mem {
                return block;
            }
        }
        panic!("class has no UseTrait member");
    }

    #[test]
    fn parses_use_trait_with_insteadof_adaptation() {
        let m = parse("class C { use Foo, Bar { Foo::greet insteadof Bar; } }").unwrap();
        let block = first_use_trait_block(&m);
        assert_eq!(block.traits.len(), 2);
        assert_eq!(block.adaptations.len(), 1);
        assert!(matches!(
            &block.adaptations[0],
            TraitAdaptation::InsteadOf { winner_trait, method, losers }
                if winner_trait == "Foo" && method == "greet" && losers == &vec!["Bar".to_string()]
        ));
    }

    #[test]
    fn parses_use_trait_with_insteadof_multiple_losers() {
        let m = parse("class C { use Foo, Bar, Baz { Foo::greet insteadof Bar, Baz; } }").unwrap();
        let block = first_use_trait_block(&m);
        let TraitAdaptation::InsteadOf { losers, .. } = &block.adaptations[0] else {
            panic!("expected InsteadOf");
        };
        assert_eq!(losers, &vec!["Bar".to_string(), "Baz".to_string()]);
    }

    #[test]
    fn parses_use_trait_with_as_alias() {
        let m = parse("class C { use Bar { Bar::greet as legacyGreet; } }").unwrap();
        let block = first_use_trait_block(&m);
        assert!(matches!(
            &block.adaptations[0],
            TraitAdaptation::Alias { source_trait, source_method, new_name, new_visibility }
                if source_trait == "Bar"
                    && source_method == "greet"
                    && new_name == "legacyGreet"
                    && new_visibility.is_none()
        ));
    }

    #[test]
    fn parses_use_trait_with_as_alias_and_visibility() {
        let m = parse("class C { use Bar { Bar::shout as private quietShout; } }").unwrap();
        let block = first_use_trait_block(&m);
        let TraitAdaptation::Alias {
            new_visibility,
            new_name,
            ..
        } = &block.adaptations[0]
        else {
            panic!("expected Alias");
        };
        assert_eq!(new_name, "quietShout");
        assert_eq!(new_visibility.as_ref(), Some(&Visibility::Private));
    }

    #[test]
    fn parses_use_trait_with_multiple_adaptations() {
        let m = parse(
            "class C {
                use Foo, Bar {
                    Foo::greet insteadof Bar;
                    Bar::greet as legacyGreet;
                }
            }",
        )
        .unwrap();
        let block = first_use_trait_block(&m);
        assert_eq!(block.adaptations.len(), 2);
        assert!(matches!(
            block.adaptations[0],
            TraitAdaptation::InsteadOf { .. }
        ));
        assert!(matches!(
            block.adaptations[1],
            TraitAdaptation::Alias { .. }
        ));
    }
}
