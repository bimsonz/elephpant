//! PHPScript lexer.
//!
//! Phase 0 stub. Phase 1 will implement tokenization for the MVP grammar
//! (variables, primitive literals, operators, identifiers, keywords).

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// Decimal integer literal. Numeric separators (`1_000`) and non-decimal
    /// bases (`0x`, `0b`, `0o`) come in later RED cycles.
    Integer(i64),
    /// Decimal float literal. PHP-faithful: any of `<digits>.<digits>`,
    /// `<digits>.`, or `.<digits>` produces a float. A bare `.` (or `.`
    /// surrounded by non-digit context) is the concat operator.
    Float(f64),
    /// String literal with no interpolation. Holds the *processed* contents
    /// (escapes resolved).
    String(String),
    /// Double-quoted string with one or more interpolated segments
    /// (`"hello $name"` or `"score: {$arr[0]}"`). The lexer splits the
    /// content into a sequence of literal text and interpolation segments;
    /// the parser turns each segment into an AST expression.
    InterpolatedString(Vec<InterpolatedSegment>),

    /// Bare identifier (function name, class name, type name, label, etc.).
    Identifier(String),
    /// Variable token — the `$` sigil is consumed; only the name is stored.
    Variable(String),

    // ---------- keywords (modern PHP only — see design.md) ----------
    // Literals.
    True,
    False,
    Null,
    // Functions / control flow.
    Function,
    Fn,
    Return,
    If,
    Else,
    Elseif,
    For,
    Foreach,
    As,
    While,
    Do,
    Break,
    Continue,
    Match,
    Switch,
    Case,
    Default,
    Yield,
    // OOP.
    Class,
    Interface,
    Enum,
    Trait,
    Abstract,
    Final,
    Extends,
    Implements,
    New,
    Clone,
    Instanceof,
    /// `insteadof` — trait method conflict resolution.
    InsteadOf,
    SelfKw,
    Parent,
    // Modifiers.
    Public,
    Private,
    Protected,
    Static,
    Readonly,
    Const,
    // Modules.
    Namespace,
    Use,
    // Errors.
    Try,
    Catch,
    Finally,
    Throw,
    // PHPScript additions (not in PHP).
    Async,
    Await,

    // ---------- punctuation ----------
    LParen,    // (
    RParen,    // )
    LBrace,    // {
    RBrace,    // }
    LBracket,  // [
    RBracket,  // ]
    Semicolon, // ;
    Comma,     // ,
    Colon,     // :
    Question,  // ?
    /// `\` — namespace path separator (e.g., `App\Foo\Bar`).
    Backslash,
    /// `@` — reserved for attributes (`#[Attr]` is also reserved). MVP does
    /// not interpret either; the parser may reject for now.
    At,

    // ---------- arithmetic ----------
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Percent,  // %
    StarStar, // **

    // ---------- comparison ----------
    Lt,        // <
    Gt,        // >
    LtEq,      // <=
    GtEq,      // >=
    Spaceship, // <=>
    /// `==` — discouraged; parser will reject in favor of `===`.
    EqEq,
    EqEqEq, // ===
    /// `!=` — discouraged; parser will reject in favor of `!==`.
    BangEq,
    BangEqEq, // !==

    // ---------- logical ----------
    AmpAmp,           // &&
    PipePipe,         // ||
    Bang,             // !
    QuestionQuestion, // ??

    // ---------- bitwise / type-system ----------
    /// `&` — used for intersection types and (eventually) references.
    Amp,
    /// `|` — used for union types.
    Pipe,

    // ---------- member access / arrows ----------
    Arrow,         // -> (instance member)
    NullSafeArrow, // ?-> (null-safe instance member)
    ColonColon,    // :: (static member)
    FatArrow,      // => (array key=>value, match arms, arrow-fn body)

    // ---------- assignment ----------
    Eq,                 // =
    PlusEq,             // +=
    MinusEq,            // -=
    StarEq,             // *=
    SlashEq,            // /=
    PercentEq,          // %=
    StarStarEq,         // **=
    DotEq,              // .=
    QuestionQuestionEq, // ??=

    // ---------- increment / decrement / spread ----------
    PlusPlus,   // ++
    MinusMinus, // --
    /// `...` — argument unpacking and rest parameters.
    Ellipsis,

    /// `.` — string concatenation operator. (Not a member-access dot — that's `->`.)
    Dot,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// One piece of a `"..."`-with-interpolation token.
#[derive(Debug, Clone, PartialEq)]
pub enum InterpolatedSegment {
    /// Literal text (escapes already resolved).
    Literal(String),
    /// Bare `$name` interpolation. Holds just the variable name (no `$`).
    Var(String),
    /// `{$<expr>}` interpolation. Holds the source text between the `{` and
    /// the matching `}`, INCLUDING the leading `$`. The parser re-runs the
    /// expression parser on this substring.
    Expr(String),
}

#[derive(Debug, thiserror::Error)]
pub enum LexError {
    #[error("unexpected character {ch:?} at byte {pos}")]
    UnexpectedChar { ch: char, pos: u32 },
    #[error("unterminated string starting at byte {start}")]
    UnterminatedString { start: u32 },
    #[error("invalid escape sequence \\{ch:?} at byte {pos}")]
    InvalidEscape { ch: char, pos: u32 },
    #[error("unterminated /* */ block comment starting at byte {start}")]
    UnterminatedBlockComment { start: u32 },
}

/// Map a lowercase identifier to its keyword `TokenKind`, or `None` if it's
/// not a reserved word. Keywords are case-sensitive in PHPScript (modern
/// style), so callers pass the raw identifier slice.
fn keyword_kind(ident: &str) -> Option<TokenKind> {
    Some(match ident {
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "null" => TokenKind::Null,
        "function" => TokenKind::Function,
        "fn" => TokenKind::Fn,
        "return" => TokenKind::Return,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "elseif" => TokenKind::Elseif,
        "for" => TokenKind::For,
        "foreach" => TokenKind::Foreach,
        "as" => TokenKind::As,
        "while" => TokenKind::While,
        "do" => TokenKind::Do,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "match" => TokenKind::Match,
        "switch" => TokenKind::Switch,
        "case" => TokenKind::Case,
        "default" => TokenKind::Default,
        "yield" => TokenKind::Yield,
        "class" => TokenKind::Class,
        "interface" => TokenKind::Interface,
        "enum" => TokenKind::Enum,
        "trait" => TokenKind::Trait,
        "abstract" => TokenKind::Abstract,
        "final" => TokenKind::Final,
        "extends" => TokenKind::Extends,
        "implements" => TokenKind::Implements,
        "new" => TokenKind::New,
        "clone" => TokenKind::Clone,
        "instanceof" => TokenKind::Instanceof,
        "insteadof" => TokenKind::InsteadOf,
        "self" => TokenKind::SelfKw,
        "parent" => TokenKind::Parent,
        "public" => TokenKind::Public,
        "private" => TokenKind::Private,
        "protected" => TokenKind::Protected,
        "static" => TokenKind::Static,
        "readonly" => TokenKind::Readonly,
        "const" => TokenKind::Const,
        "namespace" => TokenKind::Namespace,
        "use" => TokenKind::Use,
        "try" => TokenKind::Try,
        "catch" => TokenKind::Catch,
        "finally" => TokenKind::Finally,
        "throw" => TokenKind::Throw,
        "async" => TokenKind::Async,
        "await" => TokenKind::Await,
        _ => return None,
    })
}

fn is_ident_start(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphabetic()
}

fn is_ident_continue(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

pub fn lex(source: &str) -> Result<Vec<Token>, LexError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut pos: usize = 0;

    while pos < bytes.len() {
        let ch = bytes[pos];

        if matches!(ch, b' ' | b'\t' | b'\r' | b'\n') {
            pos += 1;
            continue;
        }

        // Comments — must precede operator dispatch so `/` doesn't beat `//`/`/*`.
        // - `// ...` until EOL
        // - `# ...`  until EOL  (PHP also allows `#` for line comments)
        // - `/* ... */` block (no nesting, matching PHP)
        if ch == b'/' && bytes.get(pos + 1).copied() == Some(b'/') {
            pos += 2;
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        if ch == b'#' {
            // PHP 8+ uses `#[` for attributes — we don't support attributes
            // in MVP, but to keep the door open we still treat `#[` as a
            // line-comment trigger for now and revisit when attributes land.
            pos += 1;
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        if ch == b'/' && bytes.get(pos + 1).copied() == Some(b'*') {
            let comment_start = pos;
            pos += 2;
            loop {
                if pos + 1 >= bytes.len() {
                    return Err(LexError::UnterminatedBlockComment {
                        start: comment_start as u32,
                    });
                }
                if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                    pos += 2;
                    break;
                }
                pos += 1;
            }
            continue;
        }

        let start = pos;

        // Numeric literal starting with a digit.
        // `42`        -> Integer(42)
        // `42.`       -> Float(42.0)        (PHP-faithful: trailing dot consumed)
        // `42.5`      -> Float(42.5)
        if ch.is_ascii_digit() {
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                pos += 1;
            }

            let is_float = pos < bytes.len() && bytes[pos] == b'.';

            if is_float {
                pos += 1; // consume `.`
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                let raw = &source[start..pos];
                let value: f64 = raw.parse().expect("digits[.digits?] slice parses as f64");
                tokens.push(Token {
                    kind: TokenKind::Float(value),
                    span: Span {
                        start: start as u32,
                        end: pos as u32,
                    },
                });
                continue;
            }

            let raw = &source[start..pos];
            let value: i64 = raw
                .parse()
                .expect("digit-only slice always parses as i64 within range");
            tokens.push(Token {
                kind: TokenKind::Integer(value),
                span: Span {
                    start: start as u32,
                    end: pos as u32,
                },
            });
            continue;
        }

        // Identifier / keyword.
        if is_ident_start(ch) {
            let id_start = pos;
            pos += 1;
            while pos < bytes.len() && is_ident_continue(bytes[pos]) {
                pos += 1;
            }
            let name = &source[id_start..pos];
            let kind =
                keyword_kind(name).unwrap_or_else(|| TokenKind::Identifier(name.to_string()));
            tokens.push(Token {
                kind,
                span: Span {
                    start: id_start as u32,
                    end: pos as u32,
                },
            });
            continue;
        }

        // Variable: `$` followed by identifier-start char then continues.
        if ch == b'$' {
            pos += 1;
            if pos >= bytes.len() || !is_ident_start(bytes[pos]) {
                let bad = if pos < bytes.len() {
                    bytes[pos] as char
                } else {
                    '\0'
                };
                return Err(LexError::UnexpectedChar {
                    ch: bad,
                    pos: pos as u32,
                });
            }
            let name_start = pos;
            pos += 1;
            while pos < bytes.len() && is_ident_continue(bytes[pos]) {
                pos += 1;
            }
            let name = source[name_start..pos].to_string();
            tokens.push(Token {
                kind: TokenKind::Variable(name),
                span: Span {
                    start: start as u32,
                    end: pos as u32,
                },
            });
            continue;
        }

        // String literals: `"..."` (processes escapes; supports
        // `$name` and `{$expr}` interpolation in MVP) or `'...'` (literal,
        // only `\\` and `\'` honored, no interpolation).
        //
        // Implementation note: we slice-copy runs of literal source bytes
        // rather than push char-by-char so multi-byte UTF-8 sequences
        // round-trip correctly.
        if ch == b'"' || ch == b'\'' {
            let quote = ch;
            let interpolation_enabled = quote == b'"';
            pos += 1; // consume opening quote
            let mut buf = String::new();
            let mut segments: Vec<InterpolatedSegment> = Vec::new();
            loop {
                // Copy a run of literal bytes — stop at quote, backslash, or
                // (when in double-quote) `$<ident>` / `{$`.
                let chunk_start = pos;
                while pos < bytes.len() {
                    let b = bytes[pos];
                    if b == quote || b == b'\\' {
                        break;
                    }
                    if interpolation_enabled {
                        if b == b'$' && pos + 1 < bytes.len() && is_ident_start(bytes[pos + 1]) {
                            break;
                        }
                        if b == b'{' && pos + 1 < bytes.len() && bytes[pos + 1] == b'$' {
                            break;
                        }
                    }
                    pos += 1;
                }
                buf.push_str(&source[chunk_start..pos]);

                if pos >= bytes.len() {
                    return Err(LexError::UnterminatedString {
                        start: start as u32,
                    });
                }

                let c = bytes[pos];
                if c == quote {
                    pos += 1; // consume closing quote
                    break;
                }

                if c == b'\\' {
                    let esc_pos = pos as u32;
                    if pos + 1 >= bytes.len() {
                        return Err(LexError::UnterminatedString {
                            start: start as u32,
                        });
                    }
                    let next = bytes[pos + 1];
                    if quote == b'\'' {
                        match next {
                            b'\\' => {
                                buf.push('\\');
                                pos += 2;
                            }
                            b'\'' => {
                                buf.push('\'');
                                pos += 2;
                            }
                            _ => {
                                buf.push('\\');
                                pos += 1;
                            }
                        }
                    } else {
                        match next {
                            b'"' => buf.push('"'),
                            b'\\' => buf.push('\\'),
                            b'n' => buf.push('\n'),
                            b'r' => buf.push('\r'),
                            b't' => buf.push('\t'),
                            b'0' => buf.push('\0'),
                            b'$' => buf.push('$'),
                            other => {
                                return Err(LexError::InvalidEscape {
                                    ch: other as char,
                                    pos: esc_pos,
                                });
                            }
                        }
                        pos += 2;
                    }
                    continue;
                }

                // Interpolation: `$ident` or `{$expr}`.
                debug_assert!(interpolation_enabled);
                // Flush accumulated literal.
                if !buf.is_empty() {
                    segments.push(InterpolatedSegment::Literal(std::mem::take(&mut buf)));
                }
                if c == b'$' {
                    pos += 1; // consume `$`
                    let name_start = pos;
                    while pos < bytes.len() && is_ident_continue(bytes[pos]) {
                        pos += 1;
                    }
                    let name = source[name_start..pos].to_string();
                    segments.push(InterpolatedSegment::Var(name));
                } else {
                    // `{$expr}` form — capture from `$` to matching `}`,
                    // accounting for nested braces inside the expression.
                    debug_assert_eq!(c, b'{');
                    pos += 1; // consume `{`
                    let expr_start = pos; // points at `$`
                    let mut depth = 1usize;
                    while depth > 0 {
                        if pos >= bytes.len() {
                            return Err(LexError::UnterminatedString {
                                start: start as u32,
                            });
                        }
                        match bytes[pos] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        pos += 1;
                    }
                    let expr_src = source[expr_start..pos].to_string();
                    pos += 1; // consume `}`
                    segments.push(InterpolatedSegment::Expr(expr_src));
                }
            }

            // Decide on the final token shape.
            let kind = if segments.is_empty() {
                // No interpolation — emit the regular String form so the
                // existing parser/emitter codepaths keep working.
                TokenKind::String(buf)
            } else {
                if !buf.is_empty() {
                    segments.push(InterpolatedSegment::Literal(buf));
                }
                TokenKind::InterpolatedString(segments)
            };
            tokens.push(Token {
                kind,
                span: Span {
                    start: start as u32,
                    end: pos as u32,
                },
            });
            continue;
        }

        // `.` — leading-dot float (`.5`), `.=`, `...`, or the concat operator.
        if ch == b'.' {
            let next_is_digit = pos + 1 < bytes.len() && bytes[pos + 1].is_ascii_digit();
            if next_is_digit {
                pos += 1; // consume `.`
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                let raw = &source[start..pos];
                let value: f64 = raw.parse().expect("dot-digits slice parses as f64");
                tokens.push(Token {
                    kind: TokenKind::Float(value),
                    span: Span {
                        start: start as u32,
                        end: pos as u32,
                    },
                });
                continue;
            }

            let next = bytes.get(pos + 1).copied();
            let next2 = bytes.get(pos + 2).copied();
            let (kind, len) = if next == Some(b'.') && next2 == Some(b'.') {
                (TokenKind::Ellipsis, 3)
            } else if next == Some(b'=') {
                (TokenKind::DotEq, 2)
            } else {
                (TokenKind::Dot, 1)
            };
            pos += len;
            tokens.push(Token {
                kind,
                span: Span {
                    start: start as u32,
                    end: pos as u32,
                },
            });
            continue;
        }

        // ---------- punctuation & operators ----------
        let next = bytes.get(pos + 1).copied();
        let next2 = bytes.get(pos + 2).copied();
        let result: Option<(TokenKind, usize)> = match ch {
            b'(' => Some((TokenKind::LParen, 1)),
            b')' => Some((TokenKind::RParen, 1)),
            b'{' => Some((TokenKind::LBrace, 1)),
            b'}' => Some((TokenKind::RBrace, 1)),
            b'[' => Some((TokenKind::LBracket, 1)),
            b']' => Some((TokenKind::RBracket, 1)),
            b';' => Some((TokenKind::Semicolon, 1)),
            b',' => Some((TokenKind::Comma, 1)),
            b'\\' => Some((TokenKind::Backslash, 1)),
            b'@' => Some((TokenKind::At, 1)),

            b'+' => match next {
                Some(b'+') => Some((TokenKind::PlusPlus, 2)),
                Some(b'=') => Some((TokenKind::PlusEq, 2)),
                _ => Some((TokenKind::Plus, 1)),
            },
            b'-' => match next {
                Some(b'-') => Some((TokenKind::MinusMinus, 2)),
                Some(b'=') => Some((TokenKind::MinusEq, 2)),
                Some(b'>') => Some((TokenKind::Arrow, 2)),
                _ => Some((TokenKind::Minus, 1)),
            },
            b'*' => match (next, next2) {
                (Some(b'*'), Some(b'=')) => Some((TokenKind::StarStarEq, 3)),
                (Some(b'*'), _) => Some((TokenKind::StarStar, 2)),
                (Some(b'='), _) => Some((TokenKind::StarEq, 2)),
                _ => Some((TokenKind::Star, 1)),
            },
            b'/' => match next {
                Some(b'=') => Some((TokenKind::SlashEq, 2)),
                // `//` and `/*` will be intercepted as comments in task 14.
                _ => Some((TokenKind::Slash, 1)),
            },
            b'%' => match next {
                Some(b'=') => Some((TokenKind::PercentEq, 2)),
                _ => Some((TokenKind::Percent, 1)),
            },

            b'<' => match (next, next2) {
                (Some(b'='), Some(b'>')) => Some((TokenKind::Spaceship, 3)),
                (Some(b'='), _) => Some((TokenKind::LtEq, 2)),
                _ => Some((TokenKind::Lt, 1)),
            },
            b'>' => match next {
                Some(b'=') => Some((TokenKind::GtEq, 2)),
                _ => Some((TokenKind::Gt, 1)),
            },
            b'=' => match (next, next2) {
                (Some(b'='), Some(b'=')) => Some((TokenKind::EqEqEq, 3)),
                (Some(b'='), _) => Some((TokenKind::EqEq, 2)),
                (Some(b'>'), _) => Some((TokenKind::FatArrow, 2)),
                _ => Some((TokenKind::Eq, 1)),
            },
            b'!' => match (next, next2) {
                (Some(b'='), Some(b'=')) => Some((TokenKind::BangEqEq, 3)),
                (Some(b'='), _) => Some((TokenKind::BangEq, 2)),
                _ => Some((TokenKind::Bang, 1)),
            },

            b'&' => match next {
                Some(b'&') => Some((TokenKind::AmpAmp, 2)),
                _ => Some((TokenKind::Amp, 1)),
            },
            b'|' => match next {
                Some(b'|') => Some((TokenKind::PipePipe, 2)),
                _ => Some((TokenKind::Pipe, 1)),
            },

            b'?' => match (next, next2) {
                (Some(b'?'), Some(b'=')) => Some((TokenKind::QuestionQuestionEq, 3)),
                (Some(b'?'), _) => Some((TokenKind::QuestionQuestion, 2)),
                (Some(b'-'), Some(b'>')) => Some((TokenKind::NullSafeArrow, 3)),
                _ => Some((TokenKind::Question, 1)),
            },
            b':' => match next {
                Some(b':') => Some((TokenKind::ColonColon, 2)),
                _ => Some((TokenKind::Colon, 1)),
            },

            _ => None,
        };

        if let Some((kind, len)) = result {
            pos += len;
            tokens.push(Token {
                kind,
                span: Span {
                    start: start as u32,
                    end: pos as u32,
                },
            });
            continue;
        }

        return Err(LexError::UnexpectedChar {
            ch: ch as char,
            pos: start as u32,
        });
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span {
            start: bytes.len() as u32,
            end: bytes.len() as u32,
        },
    });
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lex_empty_source_returns_eof() {
        let toks = lex("").unwrap();
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, TokenKind::Eof);
    }

    #[test]
    fn lexes_single_integer_literal() {
        assert_eq!(kinds("42"), vec![TokenKind::Integer(42), TokenKind::Eof]);
    }

    #[test]
    fn integer_literal_has_correct_span() {
        let toks = lex("42").unwrap();
        assert_eq!(toks[0].span, Span { start: 0, end: 2 });
    }

    #[test]
    fn skips_whitespace_between_tokens() {
        assert_eq!(
            kinds("42  100"),
            vec![
                TokenKind::Integer(42),
                TokenKind::Integer(100),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn skips_newlines_and_tabs() {
        assert_eq!(
            kinds("42\n\t100\r\n7"),
            vec![
                TokenKind::Integer(42),
                TokenKind::Integer(100),
                TokenKind::Integer(7),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn span_after_whitespace_points_at_token_start() {
        let toks = lex("  42").unwrap();
        assert_eq!(toks[0].span, Span { start: 2, end: 4 });
    }

    #[test]
    fn lexes_simple_float_literal() {
        assert_eq!(kinds("3.14"), vec![TokenKind::Float(3.14), TokenKind::Eof]);
    }

    #[test]
    fn lexes_zero_dot_float() {
        assert_eq!(kinds("0.5"), vec![TokenKind::Float(0.5), TokenKind::Eof]);
    }

    /// PHP fidelity: `42.` (digits then dot, no following digit) IS a float
    /// (`42.0`). The `.` is consumed as the fractional separator. Concat with
    /// a number on the left requires whitespace: `42 . $x`. PHP behaves
    /// identically — `42.$x` is a parse error in PHP too.
    #[test]
    fn trailing_dot_after_digits_is_float() {
        assert_eq!(kinds("42."), vec![TokenKind::Float(42.0), TokenKind::Eof]);
    }

    /// PHP fidelity: `.5` is Float(0.5).
    #[test]
    fn leading_dot_before_digits_is_float() {
        assert_eq!(kinds(".5"), vec![TokenKind::Float(0.5), TokenKind::Eof]);
    }

    /// A bare `.` (not adjacent to digits) is the concat operator.
    #[test]
    fn standalone_dot_is_concat_operator() {
        assert_eq!(kinds("."), vec![TokenKind::Dot, TokenKind::Eof]);
    }

    /// `.` followed by whitespace then a digit is concat, not a float.
    #[test]
    fn dot_then_space_then_digit_is_concat() {
        assert_eq!(
            kinds(". 5"),
            vec![TokenKind::Dot, TokenKind::Integer(5), TokenKind::Eof]
        );
    }

    // ---------- string literals ----------

    fn s(text: &str) -> TokenKind {
        TokenKind::String(text.to_string())
    }

    #[test]
    fn lexes_simple_double_quoted_string() {
        assert_eq!(kinds(r#""hello""#), vec![s("hello"), TokenKind::Eof]);
    }

    #[test]
    fn lexes_empty_double_quoted_string() {
        assert_eq!(kinds(r#""""#), vec![s(""), TokenKind::Eof]);
    }

    #[test]
    fn lexes_simple_single_quoted_string() {
        assert_eq!(kinds(r#"'hello'"#), vec![s("hello"), TokenKind::Eof]);
    }

    #[test]
    fn double_quoted_string_processes_escapes() {
        // \" \\ \n \t — the four MVP escapes.
        let src = r#""he said \"hi\"\n\\\there""#;
        assert_eq!(
            kinds(src),
            vec![s("he said \"hi\"\n\\\there"), TokenKind::Eof]
        );
    }

    /// Single-quoted strings only honor `\\` and `\'` per PHP. Other
    /// backslash sequences are preserved literally.
    #[test]
    fn single_quoted_string_only_escapes_backslash_and_quote() {
        assert_eq!(kinds(r"'a\nb'"), vec![s(r"a\nb"), TokenKind::Eof]);
        assert_eq!(kinds(r"'it\'s'"), vec![s("it's"), TokenKind::Eof]);
        assert_eq!(kinds(r"'a\\b'"), vec![s(r"a\b"), TokenKind::Eof]);
    }

    /// `\$` is a literal `$` and does NOT trigger interpolation.
    #[test]
    fn escaped_dollar_does_not_interpolate() {
        assert_eq!(
            kinds(r#""price: \$5""#),
            vec![s("price: $5"), TokenKind::Eof]
        );
    }

    /// `$ident` inside a double-quoted string lexes as an interpolated
    /// segment (`Var(...)`).
    #[test]
    fn dollar_ident_in_double_quoted_lexes_as_var_segment() {
        let toks = lex(r#""hello $name""#).unwrap();
        match &toks[0].kind {
            TokenKind::InterpolatedString(segs) => {
                assert_eq!(segs.len(), 2);
                assert_eq!(segs[0], InterpolatedSegment::Literal("hello ".into()));
                assert_eq!(segs[1], InterpolatedSegment::Var("name".into()));
            }
            other => panic!("expected InterpolatedString, got {other:?}"),
        }
    }

    /// `{$expr}` form captures the expression source verbatim (incl. the `$`).
    #[test]
    fn brace_dollar_expr_in_double_quoted_lexes_as_expr_segment() {
        let toks = lex(r#""score: {$arr[0]}""#).unwrap();
        match &toks[0].kind {
            TokenKind::InterpolatedString(segs) => {
                assert_eq!(segs.len(), 2);
                assert_eq!(segs[0], InterpolatedSegment::Literal("score: ".into()));
                assert_eq!(segs[1], InterpolatedSegment::Expr("$arr[0]".into()));
            }
            other => panic!("expected InterpolatedString, got {other:?}"),
        }
    }

    /// Single-quoted strings DO NOT interpolate — `$name` stays literal.
    #[test]
    fn single_quoted_string_does_not_interpolate() {
        assert_eq!(
            kinds(r#"'hello $name'"#),
            vec![s("hello $name"), TokenKind::Eof]
        );
    }

    /// A double-quoted string with no `$` or `{$` lexes as a regular
    /// `String(...)` for back-compat with all the existing snapshots.
    #[test]
    fn plain_double_quoted_remains_regular_string() {
        assert_eq!(
            kinds(r#""hello world""#),
            vec![s("hello world"), TokenKind::Eof]
        );
    }

    #[test]
    fn unterminated_double_quoted_string_errors() {
        let err = lex(r#""oops"#).unwrap_err();
        assert!(matches!(err, LexError::UnterminatedString { .. }));
    }

    // ---------- identifiers, keywords, variables ----------

    fn ident(name: &str) -> TokenKind {
        TokenKind::Identifier(name.to_string())
    }
    fn var(name: &str) -> TokenKind {
        TokenKind::Variable(name.to_string())
    }

    #[test]
    fn lexes_simple_identifier() {
        assert_eq!(kinds("foo"), vec![ident("foo"), TokenKind::Eof]);
    }

    #[test]
    fn lexes_identifier_with_underscores_and_digits() {
        assert_eq!(kinds("_my_var2"), vec![ident("_my_var2"), TokenKind::Eof]);
    }

    #[test]
    fn lexes_multiple_identifiers_separated_by_whitespace() {
        assert_eq!(
            kinds("foo bar"),
            vec![ident("foo"), ident("bar"), TokenKind::Eof]
        );
    }

    /// Identifiers cannot start with a digit. `42foo` lexes as Integer + Identifier;
    /// the parser will reject the adjacency.
    #[test]
    fn digit_then_letters_lexes_as_two_tokens() {
        assert_eq!(
            kinds("42foo"),
            vec![TokenKind::Integer(42), ident("foo"), TokenKind::Eof]
        );
    }

    /// PHP keyword recognition is case-insensitive (e.g., `IF` and `if` both
    /// reserved). Modern style uses lowercase, and PHPScript follows suit:
    /// keywords are recognized only in lowercase. `IF` is just an identifier.
    #[test]
    fn keywords_are_case_sensitive() {
        assert_eq!(kinds("IF"), vec![ident("IF"), TokenKind::Eof]);
    }

    #[test]
    fn keyword_table_recognition() {
        // Sample of keywords spanning each category. Full table-driven coverage
        // for every keyword would be repetition; this proves the dispatch works.
        let cases: &[(&str, TokenKind)] = &[
            ("null", TokenKind::Null),
            ("true", TokenKind::True),
            ("false", TokenKind::False),
            ("function", TokenKind::Function),
            ("fn", TokenKind::Fn),
            ("return", TokenKind::Return),
            ("if", TokenKind::If),
            ("else", TokenKind::Else),
            ("elseif", TokenKind::Elseif),
            ("foreach", TokenKind::Foreach),
            ("as", TokenKind::As),
            ("for", TokenKind::For),
            ("while", TokenKind::While),
            ("do", TokenKind::Do),
            ("break", TokenKind::Break),
            ("continue", TokenKind::Continue),
            ("match", TokenKind::Match),
            ("class", TokenKind::Class),
            ("interface", TokenKind::Interface),
            ("enum", TokenKind::Enum),
            ("trait", TokenKind::Trait),
            ("abstract", TokenKind::Abstract),
            ("final", TokenKind::Final),
            ("extends", TokenKind::Extends),
            ("implements", TokenKind::Implements),
            ("new", TokenKind::New),
            ("clone", TokenKind::Clone),
            ("instanceof", TokenKind::Instanceof),
            ("insteadof", TokenKind::InsteadOf),
            ("public", TokenKind::Public),
            ("private", TokenKind::Private),
            ("protected", TokenKind::Protected),
            ("static", TokenKind::Static),
            ("readonly", TokenKind::Readonly),
            ("const", TokenKind::Const),
            ("namespace", TokenKind::Namespace),
            ("use", TokenKind::Use),
            ("self", TokenKind::SelfKw),
            ("parent", TokenKind::Parent),
            ("try", TokenKind::Try),
            ("catch", TokenKind::Catch),
            ("finally", TokenKind::Finally),
            ("throw", TokenKind::Throw),
            ("async", TokenKind::Async),
            ("await", TokenKind::Await),
            ("yield", TokenKind::Yield),
            ("case", TokenKind::Case),
            ("default", TokenKind::Default),
            ("switch", TokenKind::Switch),
        ];
        for (src, expected) in cases {
            assert_eq!(
                lex(src).unwrap()[0].kind,
                *expected,
                "keyword `{src}` failed to lex"
            );
        }
    }

    #[test]
    fn lexes_simple_variable() {
        assert_eq!(kinds("$name"), vec![var("name"), TokenKind::Eof]);
    }

    #[test]
    fn variable_with_underscore_and_digit() {
        assert_eq!(kinds("$_x2"), vec![var("_x2"), TokenKind::Eof]);
    }

    /// A `$` that is not followed by an identifier-start character is invalid.
    #[test]
    fn dollar_followed_by_digit_errors() {
        assert!(matches!(
            lex("$1"),
            Err(LexError::UnexpectedChar { ch: '1', .. })
        ));
    }

    /// PHP allows superglobals like `$_GET` — same rule (ident-start can be `_`).
    #[test]
    fn lexes_underscore_prefixed_variable() {
        assert_eq!(kinds("$_GET"), vec![var("_GET"), TokenKind::Eof]);
    }

    // ---------- operators and punctuation ----------

    #[test]
    fn punctuation_recognition() {
        let cases: &[(&str, TokenKind)] = &[
            ("(", TokenKind::LParen),
            (")", TokenKind::RParen),
            ("{", TokenKind::LBrace),
            ("}", TokenKind::RBrace),
            ("[", TokenKind::LBracket),
            ("]", TokenKind::RBracket),
            (";", TokenKind::Semicolon),
            (",", TokenKind::Comma),
            (":", TokenKind::Colon),
            ("?", TokenKind::Question),
            ("\\", TokenKind::Backslash),
            ("@", TokenKind::At),
        ];
        for (src, expected) in cases {
            assert_eq!(lex(src).unwrap()[0].kind, *expected, "punct `{src}` failed");
        }
    }

    #[test]
    fn arithmetic_operators() {
        let cases: &[(&str, TokenKind)] = &[
            ("+", TokenKind::Plus),
            ("-", TokenKind::Minus),
            ("*", TokenKind::Star),
            ("/", TokenKind::Slash),
            ("%", TokenKind::Percent),
            ("**", TokenKind::StarStar),
        ];
        for (src, expected) in cases {
            assert_eq!(lex(src).unwrap()[0].kind, *expected, "op `{src}` failed");
        }
    }

    #[test]
    fn comparison_operators() {
        let cases: &[(&str, TokenKind)] = &[
            ("<", TokenKind::Lt),
            (">", TokenKind::Gt),
            ("<=", TokenKind::LtEq),
            (">=", TokenKind::GtEq),
            ("<=>", TokenKind::Spaceship),
            ("==", TokenKind::EqEq),
            ("===", TokenKind::EqEqEq),
            ("!=", TokenKind::BangEq),
            ("!==", TokenKind::BangEqEq),
        ];
        for (src, expected) in cases {
            assert_eq!(lex(src).unwrap()[0].kind, *expected, "cmp `{src}` failed");
        }
    }

    #[test]
    fn logical_and_null_coalesce_operators() {
        let cases: &[(&str, TokenKind)] = &[
            ("&&", TokenKind::AmpAmp),
            ("||", TokenKind::PipePipe),
            ("!", TokenKind::Bang),
            ("??", TokenKind::QuestionQuestion),
        ];
        for (src, expected) in cases {
            assert_eq!(
                lex(src).unwrap()[0].kind,
                *expected,
                "logical `{src}` failed"
            );
        }
    }

    /// Bare `&` and `|` lex as their own tokens (used for intersection /
    /// union types and references). `&&` / `||` still take precedence via
    /// max-munch.
    #[test]
    fn bare_amp_and_pipe_tokens() {
        assert_eq!(kinds("&"), vec![TokenKind::Amp, TokenKind::Eof]);
        assert_eq!(kinds("|"), vec![TokenKind::Pipe, TokenKind::Eof]);
        // `&&` still beats `&`+`&`.
        assert_eq!(kinds("&&"), vec![TokenKind::AmpAmp, TokenKind::Eof]);
        assert_eq!(kinds("||"), vec![TokenKind::PipePipe, TokenKind::Eof]);
        // `Foo|Bar` lexes as ident, pipe, ident.
        assert_eq!(
            kinds("Foo|Bar"),
            vec![ident("Foo"), TokenKind::Pipe, ident("Bar"), TokenKind::Eof]
        );
    }

    #[test]
    fn member_access_and_arrows() {
        let cases: &[(&str, TokenKind)] = &[
            ("->", TokenKind::Arrow),
            ("?->", TokenKind::NullSafeArrow),
            ("::", TokenKind::ColonColon),
            ("=>", TokenKind::FatArrow),
            ("=", TokenKind::Eq),
        ];
        for (src, expected) in cases {
            assert_eq!(
                lex(src).unwrap()[0].kind,
                *expected,
                "arrow/eq `{src}` failed"
            );
        }
    }

    #[test]
    fn compound_assignment_operators() {
        let cases: &[(&str, TokenKind)] = &[
            ("+=", TokenKind::PlusEq),
            ("-=", TokenKind::MinusEq),
            ("*=", TokenKind::StarEq),
            ("/=", TokenKind::SlashEq),
            ("%=", TokenKind::PercentEq),
            ("**=", TokenKind::StarStarEq),
            (".=", TokenKind::DotEq),
            ("??=", TokenKind::QuestionQuestionEq),
        ];
        for (src, expected) in cases {
            assert_eq!(
                lex(src).unwrap()[0].kind,
                *expected,
                "compound `{src}` failed"
            );
        }
    }

    #[test]
    fn increment_decrement_and_spread() {
        let cases: &[(&str, TokenKind)] = &[
            ("++", TokenKind::PlusPlus),
            ("--", TokenKind::MinusMinus),
            ("...", TokenKind::Ellipsis),
        ];
        for (src, expected) in cases {
            assert_eq!(
                lex(src).unwrap()[0].kind,
                *expected,
                "incr/spread `{src}` failed"
            );
        }
    }

    #[test]
    fn assignment_in_assignment_expression() {
        // Sanity: full token stream for `$x = 42;`
        assert_eq!(
            kinds("$x = 42;"),
            vec![
                var("x"),
                TokenKind::Eq,
                TokenKind::Integer(42),
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn function_call_token_stream() {
        // Sanity: full token stream for `foo(1, 2);`
        assert_eq!(
            kinds("foo(1, 2);"),
            vec![
                ident("foo"),
                TokenKind::LParen,
                TokenKind::Integer(1),
                TokenKind::Comma,
                TokenKind::Integer(2),
                TokenKind::RParen,
                TokenKind::Semicolon,
                TokenKind::Eof,
            ]
        );
    }

    // ---------- comments ----------

    #[test]
    fn skips_double_slash_line_comment() {
        assert_eq!(
            kinds("42 // ignore me\n7"),
            vec![
                TokenKind::Integer(42),
                TokenKind::Integer(7),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn skips_hash_line_comment() {
        assert_eq!(
            kinds("42 # also a comment\n7"),
            vec![
                TokenKind::Integer(42),
                TokenKind::Integer(7),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn skips_block_comment() {
        assert_eq!(
            kinds("1 /* mid line */ 2"),
            vec![TokenKind::Integer(1), TokenKind::Integer(2), TokenKind::Eof,]
        );
    }

    #[test]
    fn skips_multiline_block_comment() {
        assert_eq!(
            kinds("1 /* line1\nline2 */\n2"),
            vec![TokenKind::Integer(1), TokenKind::Integer(2), TokenKind::Eof,]
        );
    }

    #[test]
    fn line_comment_at_eof_with_no_newline() {
        assert_eq!(
            kinds("42 // trailing"),
            vec![TokenKind::Integer(42), TokenKind::Eof]
        );
    }

    #[test]
    fn unterminated_block_comment_errors() {
        let err = lex("1 /* never closed").unwrap_err();
        assert!(matches!(err, LexError::UnterminatedBlockComment { .. }));
    }

    /// `/` and `/=` still lex correctly when not followed by `/` or `*`.
    /// Regression guard for the comment-vs-divide disambiguation.
    #[test]
    fn slash_alone_still_lexes_as_divide() {
        assert_eq!(kinds("/"), vec![TokenKind::Slash, TokenKind::Eof]);
        assert_eq!(kinds("/="), vec![TokenKind::SlashEq, TokenKind::Eof]);
    }

    // ---------- error cases & spans (task 15 polish) ----------

    #[test]
    fn string_span_includes_quotes() {
        let toks = lex(r#""hi""#).unwrap();
        assert_eq!(toks[0].span, Span { start: 0, end: 4 });
    }

    #[test]
    fn variable_span_includes_dollar_sigil() {
        let toks = lex("$foo").unwrap();
        assert_eq!(toks[0].span, Span { start: 0, end: 4 });
    }

    #[test]
    fn span_after_block_comment_is_correct() {
        // `/* hi */ 42`: the `42` token starts at byte 9.
        let toks = lex("/* hi */ 42").unwrap();
        assert_eq!(toks[0].kind, TokenKind::Integer(42));
        assert_eq!(toks[0].span, Span { start: 9, end: 11 });
    }

    #[test]
    fn span_across_multiple_lines() {
        // `42\n7\n100`: tokens at byte offsets 0..2, 3..4, 5..8.
        let toks = lex("42\n7\n100").unwrap();
        assert_eq!(toks[0].span, Span { start: 0, end: 2 });
        assert_eq!(toks[1].span, Span { start: 3, end: 4 });
        assert_eq!(toks[2].span, Span { start: 5, end: 8 });
    }

    #[test]
    fn eof_span_points_at_source_end() {
        let src = "42";
        let toks = lex(src).unwrap();
        let eof = toks.last().unwrap();
        assert_eq!(eof.kind, TokenKind::Eof);
        assert_eq!(eof.span, Span { start: 2, end: 2 });
    }

    #[test]
    fn eof_span_for_empty_source_is_zero_zero() {
        let toks = lex("").unwrap();
        assert_eq!(toks[0].span, Span { start: 0, end: 0 });
    }

    #[test]
    fn multibyte_utf8_in_string_keeps_byte_spans() {
        // "café" — `é` is two bytes (0xc3 0xa9). String literal spans 7 bytes
        // (`"`, `c`, `a`, `f`, 0xc3, 0xa9, `"`).
        let toks = lex("\"café\"").unwrap();
        assert_eq!(toks[0].kind, TokenKind::String("café".into()));
        assert_eq!(toks[0].span, Span { start: 0, end: 7 });
    }

    #[test]
    fn unexpected_char_error_carries_byte_position() {
        // `^` is not a token in MVP. Expect UnexpectedChar with pos 3.
        let err = lex("42 ^ 5").unwrap_err();
        match err {
            LexError::UnexpectedChar { ch, pos } => {
                assert_eq!(ch, '^');
                assert_eq!(pos, 3);
            }
            other => panic!("expected UnexpectedChar, got {other:?}"),
        }
    }

    #[test]
    fn invalid_escape_error_carries_byte_position() {
        // `\q` is not a recognized escape inside a double-quoted string.
        let err = lex(r#""x\qy""#).unwrap_err();
        match err {
            LexError::InvalidEscape { ch, pos } => {
                assert_eq!(ch, 'q');
                assert_eq!(pos, 2); // position of the `\`
            }
            other => panic!("expected InvalidEscape, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_block_comment_carries_start_position() {
        let err = lex("ok /* never closed").unwrap_err();
        match err {
            LexError::UnterminatedBlockComment { start } => {
                assert_eq!(start, 3);
            }
            other => panic!("expected UnterminatedBlockComment, got {other:?}"),
        }
    }
}
