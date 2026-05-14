//! In-memory document index: keyed by LSP `Url`, holds the parsed module
//! (or the latest parse error) plus a `LineMap` so handlers can translate
//! between LSP positions and AST byte spans.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use psx_ast::Module;
use psx_parser::{parse, ParseError};
use psx_sourcemap::LineMap;
use tower_lsp::lsp_types::Url;

/// Snapshot of a single open document.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub uri: Url,
    pub source: Arc<String>,
    pub line_map: Arc<LineMap>,
    /// `Some(_)` on success, `None` when the source failed to parse.
    pub module: Option<Arc<Module>>,
    /// `Some(_)` when parse failed. The string is the formatted error; the
    /// byte span is the source range to highlight.
    pub parse_error: Option<(String, (u32, u32))>,
}

impl ParsedDocument {
    pub fn from_source(uri: Url, source: String) -> Self {
        let line_map = Arc::new(LineMap::new(&source));
        let (module, parse_error) = match parse(&source) {
            Ok(m) => (Some(Arc::new(m)), None),
            Err(err) => {
                let span = span_of_error(&err, source.len() as u32);
                let msg = err.to_string();
                (None, Some((msg, span)))
            }
        };
        Self {
            uri,
            source: Arc::new(source),
            line_map,
            module,
            parse_error,
        }
    }
}

/// Compute a byte range to highlight for a parse error. The parser only
/// gives us a single position; we widen by one byte so the diagnostic
/// underline is visible.
fn span_of_error(err: &ParseError, source_len: u32) -> (u32, u32) {
    match err {
        ParseError::UnexpectedToken { pos, .. }
        | ParseError::LooseEqualityRejected { pos, .. }
        | ParseError::AsymVisWriteWiderThanRead { pos, .. }
        | ParseError::DeprecatedVarKeyword { pos }
        | ParseError::DeprecatedArrayConstructor { pos } => {
            let start = *pos;
            let end = (start + 1).min(source_len);
            (start, end)
        }
        ParseError::UnexpectedEof { .. } => {
            let end = source_len;
            let start = end.saturating_sub(1);
            (start, end)
        }
        ParseError::Lex(_) => (0, source_len.min(1)),
    }
}

#[derive(Debug, Default)]
pub struct DocumentIndex {
    pub docs: DashMap<Url, ParsedDocument>,
    /// Workspace root URI as set by the client during `initialize`. Used to
    /// resolve `use App\Foo;` across files in workspace-symbols / goto.
    pub workspace_root: parking_lot_lite::RwLock<Option<PathBuf>>,
}

/// Bare-minimum stand-in for `parking_lot::RwLock`. Avoids pulling in the
/// full crate; we only need `read`/`write` accessors for the workspace
/// root, which is set once and read often.
mod parking_lot_lite {
    use std::sync::RwLock as StdRwLock;

    #[derive(Debug, Default)]
    pub struct RwLock<T>(StdRwLock<T>);

    impl<T: Default> RwLock<T> {
        pub fn read(&self) -> std::sync::RwLockReadGuard<'_, T> {
            self.0.read().expect("rwlock poisoned")
        }
        pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, T> {
            self.0.write().expect("rwlock poisoned")
        }
    }
}
