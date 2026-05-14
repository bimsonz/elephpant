//! `tower-lsp` backend implementation. Wires LSP requests to the cached
//! parse results in [`crate::index::DocumentIndex`].
//!
//! Phase 1 capabilities:
//! - text-document sync (open / change / save / close, full)
//! - diagnostics (parse errors, published on every reparse)
//! - hover (statement-level description at the cursor)
//! - goto-definition (`use App\Foo;` → target file)
//! - document symbols (classes / interfaces / enums / traits / functions)
//! - workspace symbols (project-wide, populated on `initialize`)

use std::path::{Path, PathBuf};
use std::sync::Arc;

use psx_ast::{ClassMember, Module, Stmt};
use psx_resolver::{resolve_use, PsxConfig, ResolvedImport};
use psx_sourcemap::{LineCol, LineMap};
use tower_lsp::jsonrpc::Result as JsonRpcResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::index::{DocumentIndex, ParsedDocument};

pub struct Backend {
    pub client: Client,
    pub index: Arc<DocumentIndex>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            index: Arc::new(DocumentIndex::default()),
        }
    }

    async fn reparse_and_publish(&self, uri: Url, source: String, version: i32) {
        let doc = ParsedDocument::from_source(uri.clone(), source);
        let diags = build_diagnostics(&doc);
        self.index.docs.insert(uri.clone(), doc);
        self.client
            .publish_diagnostics(uri, diags, Some(version))
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> JsonRpcResult<InitializeResult> {
        // Capture the workspace root (preferring the first workspace folder
        // over the deprecated single `root_uri`). Used by workspace-symbols
        // and any future cross-file analysis.
        if let Some(folders) = params.workspace_folders.as_ref() {
            if let Some(folder) = folders.first() {
                if let Ok(path) = folder.uri.to_file_path() {
                    *self.index.workspace_root.write() = Some(path);
                }
            }
        }
        #[allow(deprecated)]
        if self.index.workspace_root.read().is_none() {
            if let Some(uri) = params.root_uri.as_ref() {
                if let Ok(path) = uri.to_file_path() {
                    *self.index.workspace_root.write() = Some(path);
                }
            }
        }

        // Pre-warm the index by parsing every .psx file under the workspace
        // root. workspace-symbols searches it; goto-definition uses it to
        // resolve cross-file `use` targets without a separate disk read.
        let root = self.index.workspace_root.read().clone();
        if let Some(root) = root {
            for psx_path in collect_psx_files(&root) {
                if let Some(uri) = Url::from_file_path(&psx_path).ok() {
                    if let Ok(source) = std::fs::read_to_string(&psx_path) {
                        let doc = ParsedDocument::from_source(uri.clone(), source);
                        self.index.docs.insert(uri, doc);
                    }
                }
            }
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "psx-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        "$".to_string(),
                        ">".to_string(),
                        ":".to_string(),
                        "\\".to_string(),
                    ]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    ..Default::default()
                }),
                rename_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "psx-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> JsonRpcResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let source = params.text_document.text;
        self.reparse_and_publish(uri, source, version).await;
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        // FULL sync — the spec says the array contains exactly one entry
        // whose `text` is the new full document text. Treat anything else
        // as a no-op (incremental sync isn't enabled in capabilities).
        if let Some(change) = params.content_changes.pop() {
            if change.range.is_none() {
                self.reparse_and_publish(uri, change.text, version).await;
            }
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        // Save with no text payload — keep whatever is already in the
        // index. With FULL sync, the latest didChange already covered it.
        if let Some(text) = params.text {
            let uri = params.text_document.uri;
            // version isn't carried on did_save; use 0.
            self.reparse_and_publish(uri, text, 0).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.index.docs.remove(&params.text_document.uri);
        // Clear diagnostics for the closed file so editors don't show
        // stale squigglies after the buffer is gone.
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, params: HoverParams) -> JsonRpcResult<Option<Hover>> {
        let pos = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let offset = doc.line_map.byte_of(pos.line, pos.character);
        let Some(stmt) = find_innermost_stmt(module, offset) else {
            return Ok(None);
        };
        let summary = describe_stmt(stmt);
        let range = stmt_to_range(stmt, &doc.line_map);
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: summary,
            }),
            range: Some(range),
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> JsonRpcResult<Option<GotoDefinitionResponse>> {
        let pos = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let offset = doc.line_map.byte_of(pos.line, pos.character);
        let Some(stmt) = find_innermost_stmt(module, offset) else {
            return Ok(None);
        };
        // Only support `use App\Foo\Bar;` jumping today. Method-call and
        // class-name goto require type analysis we don't have yet.
        let Stmt::Use(use_stmt) = stmt else {
            return Ok(None);
        };
        let root = self.index.workspace_root.read().clone();
        let Some(root) = root else { return Ok(None) };
        let Some(config_path) = find_psx_json(&root) else {
            return Ok(None);
        };
        let Ok(config) = PsxConfig::load(&config_path) else {
            return Ok(None);
        };
        let current_psx = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let project_root = config_path.parent().unwrap_or(&root).to_path_buf();
        let mut locations: Vec<Location> = Vec::new();
        for item in &use_stmt.items {
            let resolved = resolve_use(
                &config,
                &project_root,
                &current_psx,
                &item.path,
                item.alias.as_deref(),
            );
            let Ok(ResolvedImport::Local { rel_path, .. }) = resolved else {
                continue; // npm imports have no `.psx` to jump to
            };
            // resolve_use returns a path relative to the importing .ts under
            // dist/; recover the absolute .psx by re-joining against the
            // project source layout.
            let target_psx = locate_psx_for_use(&project_root, &config, &item.path);
            if let Some(target) = target_psx {
                if let Ok(uri) = Url::from_file_path(&target) {
                    locations.push(Location {
                        uri,
                        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    });
                }
            }
            let _ = rel_path;
        }
        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(GotoDefinitionResponse::Array(locations)))
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> JsonRpcResult<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let symbols = collect_document_symbols(module, &doc.line_map);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> JsonRpcResult<Option<Vec<SymbolInformation>>> {
        let needle = params.query.to_lowercase();
        let mut out: Vec<SymbolInformation> = Vec::new();
        for entry in self.index.docs.iter() {
            let doc = entry.value();
            let Some(module) = doc.module.as_ref() else {
                continue;
            };
            for sym in collect_workspace_symbols(module, &doc.uri, &doc.line_map) {
                if sym.name.to_lowercase().contains(&needle) {
                    out.push(sym);
                }
            }
        }
        Ok(Some(out))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> JsonRpcResult<Option<CompletionResponse>> {
        let pos = params.text_document_position.position;
        let uri = &params.text_document_position.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let offset = doc.line_map.byte_of(pos.line, pos.character);
        let mut items = keyword_completions();
        items.extend(workspace_symbol_completions(&self.index));
        if let Some(module) = doc.module.as_ref() {
            // `$this->|` — inside a method body, offer the enclosing
            // class's members.
            if let Some(class) = enclosing_class(module, offset) {
                let source_byte_before_cursor = offset.saturating_sub(1);
                let preceding = byte_slice(doc.source.as_str(), source_byte_before_cursor, offset);
                if preceding.ends_with('>') {
                    // We're right after `->` (or `>` of another op). Offer
                    // class members.
                    items.extend(class_member_completions(class));
                }
            }
        }
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> JsonRpcResult<Option<SignatureHelp>> {
        let pos = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let offset = doc.line_map.byte_of(pos.line, pos.character);
        let Some(callee_name) = find_enclosing_call_name(&doc.source, offset) else {
            return Ok(None);
        };
        let Some(fn_info) = find_function_signature(module, &callee_name)
            .or_else(|| find_workspace_function(&self.index, &callee_name))
        else {
            return Ok(None);
        };
        Ok(Some(SignatureHelp {
            signatures: vec![fn_info],
            active_signature: Some(0),
            active_parameter: None,
        }))
    }

    async fn rename(&self, params: RenameParams) -> JsonRpcResult<Option<WorkspaceEdit>> {
        let pos = params.text_document_position.position;
        let uri = &params.text_document_position.text_document.uri;
        let new_name = params.new_name;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let offset = doc.line_map.byte_of(pos.line, pos.character);
        // Find which variable name the cursor is on by scanning the source
        // text. We need a `$name` token at `offset`.
        let Some(target_name) = identifier_at(&doc.source, offset) else {
            return Ok(None);
        };
        let mut edits: Vec<TextEdit> = Vec::new();
        let mut hits: Vec<u32> = Vec::new();
        collect_variable_offsets(module, &target_name, &mut hits);
        for h in hits {
            let start = doc.line_map.line_col(h);
            let end = doc.line_map.line_col(h + target_name.len() as u32);
            edits.push(TextEdit {
                range: Range::new(lc_to_position(start), lc_to_position(end)),
                new_text: new_name.clone(),
            });
        }
        if edits.is_empty() {
            return Ok(None);
        }
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri.clone(), edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> JsonRpcResult<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let mut out: Vec<CodeActionOrCommand> = Vec::new();
        // Convert any `array()` long-form call we can spot at the cursor
        // into the short-form `[]`. Only fires if the diagnostic's range
        // covers something matching the legacy pattern.
        let Some(doc) = self.index.docs.get(&uri) else {
            return Ok(Some(out));
        };
        let pos = params.range.start;
        let byte = doc.line_map.byte_of(pos.line, pos.character);
        if let Some(end) = find_array_call(&doc.source, byte) {
            // `array(...)` -> `[...]`. Span: array_call_start..end (exclusive).
            let array_start = byte;
            let lc_start = doc.line_map.line_col(array_start);
            let lc_end = doc.line_map.line_col(end);
            let mut changes = std::collections::HashMap::new();
            changes.insert(
                uri.clone(),
                vec![
                    TextEdit {
                        range: Range::new(
                            lc_to_position(lc_start),
                            lc_to_position(LineCol {
                                line: lc_start.line,
                                column: lc_start.column + 6, // "array("
                            }),
                        ),
                        new_text: "[".to_string(),
                    },
                    TextEdit {
                        range: Range::new(
                            lc_to_position(LineCol {
                                line: lc_end.line,
                                column: lc_end.column.saturating_sub(1),
                            }),
                            lc_to_position(lc_end),
                        ),
                        new_text: "]".to_string(),
                    },
                ],
            );
            out.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "Convert array() to []".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
        Ok(Some(out))
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> JsonRpcResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let formatted = psx_printer::format_module(module);
        if formatted == doc.source.as_str() {
            return Ok(None);
        }
        // Replace the entire document.
        let last_line = doc.line_map.line_count().saturating_sub(1) as u32;
        let last_lc = doc.line_map.line_col(doc.source.len() as u32);
        Ok(Some(vec![TextEdit {
            range: Range::new(
                Position::new(0, 0),
                lc_to_position(LineCol {
                    line: last_line,
                    column: last_lc.column,
                }),
            ),
            new_text: formatted,
        }]))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> JsonRpcResult<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let Some(doc) = self.index.docs.get(uri) else {
            return Ok(None);
        };
        let Some(module) = doc.module.as_ref() else {
            return Ok(None);
        };
        let mut hints = Vec::new();
        collect_inlay_hints(module, &doc.source, &doc.line_map, &mut hints);
        Ok(Some(hints))
    }
}

// ---------------- Diagnostics ----------------

fn build_diagnostics(doc: &ParsedDocument) -> Vec<Diagnostic> {
    let Some((message, span)) = doc.parse_error.as_ref() else {
        return Vec::new();
    };
    let range = span_to_range(*span, &doc.line_map);
    vec![Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("psx-parser".to_string()),
        message: message.clone(),
        ..Default::default()
    }]
}

// ---------------- AST walking ----------------

fn span_contains(span: psx_ast::Span, offset: u32) -> bool {
    offset >= span.start && offset <= span.end
}

/// Find the innermost `Stmt` whose span contains `offset`. Walks through
/// `Block`, `If`, `While`, `Foreach`, `Try`, plus function/method/trait
/// bodies. Returns `None` if no top-level statement covers the offset.
fn find_innermost_stmt<'a>(module: &'a Module, offset: u32) -> Option<&'a Stmt> {
    for stmt in &module.stmts {
        if span_contains(stmt.span(), offset) {
            return Some(descend(stmt, offset));
        }
    }
    None
}

fn descend<'a>(stmt: &'a Stmt, offset: u32) -> &'a Stmt {
    match stmt {
        Stmt::Block(stmts, _) | Stmt::Try { body: stmts, .. } => {
            for s in stmts {
                if span_contains(s.span(), offset) {
                    return descend(s, offset);
                }
            }
            stmt
        }
        Stmt::If { then, else_, .. } => {
            if span_contains(then.span(), offset) {
                return descend(then, offset);
            }
            if let Some(e) = else_ {
                if span_contains(e.span(), offset) {
                    return descend(e, offset);
                }
            }
            stmt
        }
        Stmt::While { body, .. } | Stmt::Foreach { body, .. } | Stmt::DoWhile { body, .. } => {
            if span_contains(body.span(), offset) {
                return descend(body, offset);
            }
            stmt
        }
        Stmt::For { body, .. } => {
            if span_contains(body.span(), offset) {
                return descend(body, offset);
            }
            stmt
        }
        Stmt::Function(decl) => {
            for s in &decl.body {
                if span_contains(s.span(), offset) {
                    return descend(s, offset);
                }
            }
            stmt
        }
        Stmt::Class(class) => {
            for m in &class.members {
                if let ClassMember::Method(method) = m {
                    if let Some(body) = &method.body {
                        for s in body {
                            if span_contains(s.span(), offset) {
                                return descend(s, offset);
                            }
                        }
                    }
                }
            }
            stmt
        }
        Stmt::Trait(trait_decl) => {
            for m in &trait_decl.members {
                if let ClassMember::Method(method) = m {
                    if let Some(body) = &method.body {
                        for s in body {
                            if span_contains(s.span(), offset) {
                                return descend(s, offset);
                            }
                        }
                    }
                }
            }
            stmt
        }
        _ => stmt,
    }
}

fn describe_stmt(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Function(d) => {
            let async_marker = if d.async_ { "async " } else { "" };
            format!("```php\n{async_marker}function {}(...)\n```", d.name)
        }
        Stmt::Class(c) => format!("```php\nclass {} {{ ... }}\n```", c.name),
        Stmt::Interface(i) => format!("```php\ninterface {} {{ ... }}\n```", i.name),
        Stmt::Enum(e) => format!("```php\nenum {} {{ ... }}\n```", e.name),
        Stmt::Trait(t) => format!("```php\ntrait {} {{ ... }}\n```", t.name),
        Stmt::Namespace(path, _) => format!("```php\nnamespace {};\n```", path.join("\\")),
        Stmt::Use(_) => "**`use` declaration** — import a symbol into scope.".to_string(),
        Stmt::If { .. } => "**`if` statement**".to_string(),
        Stmt::While { .. } => "**`while` loop**".to_string(),
        Stmt::DoWhile { .. } => "**`do … while` loop**".to_string(),
        Stmt::For { .. } => "**`for` loop**".to_string(),
        Stmt::Foreach { .. } => "**`foreach` loop**".to_string(),
        Stmt::Return(_, _) => "**`return` statement**".to_string(),
        Stmt::Throw(_, _) => "**`throw` statement**".to_string(),
        Stmt::Break(_, _) => "**`break` statement**".to_string(),
        Stmt::Continue(_, _) => "**`continue` statement**".to_string(),
        Stmt::Try { .. } => "**`try` block**".to_string(),
        Stmt::Block(_, _) => "**block**".to_string(),
        Stmt::Expr(_, _) => "**expression statement**".to_string(),
    }
}

// ---------------- Symbols ----------------

fn collect_document_symbols(module: &Module, line_map: &LineMap) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for stmt in &module.stmts {
        if let Some(sym) = stmt_to_doc_symbol(stmt, line_map) {
            out.push(sym);
        }
    }
    out
}

fn stmt_to_doc_symbol(stmt: &Stmt, line_map: &LineMap) -> Option<DocumentSymbol> {
    let range = stmt_to_range(stmt, line_map);
    let (name, kind, children) = match stmt {
        Stmt::Function(d) => (d.name.clone(), SymbolKind::FUNCTION, None),
        Stmt::Class(c) => {
            let mut child_syms: Vec<DocumentSymbol> = Vec::new();
            for m in &c.members {
                if let Some(s) = class_member_to_doc_symbol(m, line_map) {
                    child_syms.push(s);
                }
            }
            (c.name.clone(), SymbolKind::CLASS, Some(child_syms))
        }
        Stmt::Interface(i) => {
            let mut child_syms: Vec<DocumentSymbol> = Vec::new();
            for m in &i.members {
                if let psx_ast::InterfaceMember::Method(method) = m {
                    child_syms.push(method_to_doc_symbol(method, line_map));
                }
            }
            (i.name.clone(), SymbolKind::INTERFACE, Some(child_syms))
        }
        Stmt::Enum(e) => (e.name.clone(), SymbolKind::ENUM, None),
        Stmt::Trait(t) => {
            let mut child_syms: Vec<DocumentSymbol> = Vec::new();
            for m in &t.members {
                if let Some(s) = class_member_to_doc_symbol(m, line_map) {
                    child_syms.push(s);
                }
            }
            (t.name.clone(), SymbolKind::INTERFACE, Some(child_syms))
        }
        _ => return None,
    };
    #[allow(deprecated)]
    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children,
    })
}

fn class_member_to_doc_symbol(m: &ClassMember, line_map: &LineMap) -> Option<DocumentSymbol> {
    match m {
        ClassMember::Method(method) => Some(method_to_doc_symbol(method, line_map)),
        ClassMember::Constant(c) => {
            let range = Range::new(Position::new(0, 0), Position::new(0, 0));
            #[allow(deprecated)]
            Some(DocumentSymbol {
                name: c.name.clone(),
                detail: None,
                kind: SymbolKind::CONSTANT,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            })
        }
        ClassMember::Property(p) => {
            let range = Range::new(Position::new(0, 0), Position::new(0, 0));
            #[allow(deprecated)]
            Some(DocumentSymbol {
                name: p.name.clone(),
                detail: None,
                kind: SymbolKind::PROPERTY,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            })
        }
        ClassMember::UseTrait(_) => None,
    }
}

fn method_to_doc_symbol(method: &psx_ast::Method, _line_map: &LineMap) -> DocumentSymbol {
    let range = Range::new(Position::new(0, 0), Position::new(0, 0));
    #[allow(deprecated)]
    DocumentSymbol {
        name: method.name.clone(),
        detail: None,
        kind: SymbolKind::METHOD,
        tags: None,
        deprecated: None,
        range,
        selection_range: range,
        children: None,
    }
}

fn collect_workspace_symbols(
    module: &Module,
    uri: &Url,
    line_map: &LineMap,
) -> Vec<SymbolInformation> {
    let mut out = Vec::new();
    for stmt in &module.stmts {
        if let Some((name, kind)) = workspace_symbol_of_stmt(stmt) {
            let range = stmt_to_range(stmt, line_map);
            #[allow(deprecated)]
            out.push(SymbolInformation {
                name,
                kind,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: uri.clone(),
                    range,
                },
                container_name: None,
            });
        }
    }
    out
}

fn workspace_symbol_of_stmt(stmt: &Stmt) -> Option<(String, SymbolKind)> {
    match stmt {
        Stmt::Function(d) => Some((d.name.clone(), SymbolKind::FUNCTION)),
        Stmt::Class(c) => Some((c.name.clone(), SymbolKind::CLASS)),
        Stmt::Interface(i) => Some((i.name.clone(), SymbolKind::INTERFACE)),
        Stmt::Enum(e) => Some((e.name.clone(), SymbolKind::ENUM)),
        Stmt::Trait(t) => Some((t.name.clone(), SymbolKind::INTERFACE)),
        _ => None,
    }
}

// ---------------- Range helpers ----------------

fn stmt_to_range(stmt: &Stmt, line_map: &LineMap) -> Range {
    span_to_range((stmt.span().start, stmt.span().end), line_map)
}

fn span_to_range(span: (u32, u32), line_map: &LineMap) -> Range {
    let start = lc_to_position(line_map.line_col(span.0));
    let end = lc_to_position(line_map.line_col(span.1));
    Range::new(start, end)
}

fn lc_to_position(lc: LineCol) -> Position {
    Position::new(lc.line, lc.column)
}

// ---------------- Workspace helpers ----------------

fn collect_psx_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip common heavy directories so initial-index doesn't crawl
            // node_modules, target, dist, etc.
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                if matches!(
                    name,
                    "node_modules" | "target" | "dist" | "dist-js" | ".git"
                ) {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("psx") {
                out.push(path);
            }
        }
    }
    out
}

fn find_psx_json(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start.to_path_buf());
    while let Some(p) = cur {
        let candidate = p.join("psx.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        cur = p.parent().map(|x| x.to_path_buf());
    }
    None
}

/// Given a `use App\Foo\Bar` path and the project config, return the
/// absolute path of the `.psx` file that declares it (PSR-4 layout).
fn locate_psx_for_use(project_root: &Path, config: &PsxConfig, path: &[String]) -> Option<PathBuf> {
    // PSR-4: strip the project namespace prefix, map remaining segments to
    // <src>/<segment>/.../<last>.psx.
    let ns_prefix: Vec<&str> = config
        .namespace
        .split('\\')
        .filter(|s| !s.is_empty())
        .collect();
    if path.len() <= ns_prefix.len() {
        return None;
    }
    for (i, seg) in ns_prefix.iter().enumerate() {
        if path.get(i).map(String::as_str) != Some(seg) {
            return None;
        }
    }
    let rest = &path[ns_prefix.len()..];
    let mut target = project_root.join(&config.src);
    for seg in &rest[..rest.len() - 1] {
        target.push(seg);
    }
    target.push(rest.last().unwrap());
    target.set_extension("psx");
    if target.is_file() {
        Some(target)
    } else {
        None
    }
}

// ---------------- Phase 2 / 3 helpers ----------------

fn keyword_completions() -> Vec<CompletionItem> {
    [
        "function",
        "class",
        "interface",
        "enum",
        "trait",
        "abstract",
        "final",
        "extends",
        "implements",
        "public",
        "private",
        "protected",
        "static",
        "readonly",
        "const",
        "namespace",
        "use",
        "if",
        "elseif",
        "else",
        "for",
        "foreach",
        "as",
        "while",
        "do",
        "break",
        "continue",
        "return",
        "match",
        "switch",
        "case",
        "default",
        "try",
        "catch",
        "finally",
        "throw",
        "new",
        "clone",
        "instanceof",
        "insteadof",
        "self",
        "parent",
        "async",
        "await",
        "true",
        "false",
        "null",
    ]
    .iter()
    .map(|kw| CompletionItem {
        label: kw.to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        ..Default::default()
    })
    .collect()
}

fn workspace_symbol_completions(index: &Arc<DocumentIndex>) -> Vec<CompletionItem> {
    let mut out = Vec::new();
    for entry in index.docs.iter() {
        let doc = entry.value();
        let Some(module) = doc.module.as_ref() else {
            continue;
        };
        for stmt in &module.stmts {
            match stmt {
                Stmt::Function(d) => out.push(CompletionItem {
                    label: d.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    ..Default::default()
                }),
                Stmt::Class(c) => out.push(CompletionItem {
                    label: c.name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    ..Default::default()
                }),
                Stmt::Interface(i) => out.push(CompletionItem {
                    label: i.name.clone(),
                    kind: Some(CompletionItemKind::INTERFACE),
                    ..Default::default()
                }),
                Stmt::Enum(e) => out.push(CompletionItem {
                    label: e.name.clone(),
                    kind: Some(CompletionItemKind::ENUM),
                    ..Default::default()
                }),
                Stmt::Trait(t) => out.push(CompletionItem {
                    label: t.name.clone(),
                    kind: Some(CompletionItemKind::INTERFACE),
                    ..Default::default()
                }),
                _ => {}
            }
        }
    }
    out
}

fn enclosing_class<'a>(module: &'a Module, offset: u32) -> Option<&'a psx_ast::Class> {
    for stmt in &module.stmts {
        if let Stmt::Class(c) = stmt {
            if span_contains(c.span, offset) {
                return Some(c);
            }
        }
    }
    None
}

fn class_member_completions(class: &psx_ast::Class) -> Vec<CompletionItem> {
    let mut out = Vec::new();
    for m in &class.members {
        match m {
            ClassMember::Method(method) => out.push(CompletionItem {
                label: method.name.clone(),
                kind: Some(CompletionItemKind::METHOD),
                ..Default::default()
            }),
            ClassMember::Property(p) => out.push(CompletionItem {
                label: p.name.clone(),
                kind: Some(CompletionItemKind::PROPERTY),
                ..Default::default()
            }),
            ClassMember::Constant(c) => out.push(CompletionItem {
                label: c.name.clone(),
                kind: Some(CompletionItemKind::CONSTANT),
                ..Default::default()
            }),
            ClassMember::UseTrait(_) => {}
        }
    }
    out
}

fn byte_slice(s: &str, start: u32, end: u32) -> &str {
    let start = (start as usize).min(s.len());
    let end = (end as usize).min(s.len()).max(start);
    s.get(start..end).unwrap_or("")
}

/// Walk backwards from `offset` to find the name of the function/method
/// being called. Stops at the unmatched `(`.
fn find_enclosing_call_name(source: &str, offset: u32) -> Option<String> {
    let bytes = source.as_bytes();
    let mut depth = 0;
    let mut i = (offset as usize).min(bytes.len());
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' if depth == 0 => {
                // Collect the identifier immediately before this `(`.
                let mut j = i;
                while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                    j -= 1;
                }
                let end = j;
                while j > 0 && (bytes[j - 1].is_ascii_alphanumeric() || bytes[j - 1] == b'_') {
                    j -= 1;
                }
                if end > j {
                    return Some(source[j..end].to_string());
                }
                return None;
            }
            b'(' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn find_function_signature(module: &Module, name: &str) -> Option<SignatureInformation> {
    for stmt in &module.stmts {
        if let Stmt::Function(decl) = stmt {
            if decl.name == name {
                return Some(signature_of(decl));
            }
        }
    }
    None
}

fn find_workspace_function(index: &Arc<DocumentIndex>, name: &str) -> Option<SignatureInformation> {
    for entry in index.docs.iter() {
        let doc = entry.value();
        let Some(module) = doc.module.as_ref() else {
            continue;
        };
        if let Some(sig) = find_function_signature(module, name) {
            return Some(sig);
        }
    }
    None
}

fn signature_of(decl: &psx_ast::FunctionDecl) -> SignatureInformation {
    let params: Vec<ParameterInformation> = decl
        .params
        .iter()
        .map(|p| {
            let ty =
                p.ty.as_ref()
                    .map(|t| format!("{} ", display_type(t)))
                    .unwrap_or_default();
            ParameterInformation {
                label: ParameterLabel::Simple(format!("{}${}", ty, p.name)),
                documentation: None,
            }
        })
        .collect();
    let param_str: Vec<String> = decl
        .params
        .iter()
        .map(|p| {
            let ty =
                p.ty.as_ref()
                    .map(|t| format!("{} ", display_type(t)))
                    .unwrap_or_default();
            format!("{}${}", ty, p.name)
        })
        .collect();
    let ret = decl
        .return_type
        .as_ref()
        .map(|t| format!(": {}", display_type(t)))
        .unwrap_or_default();
    SignatureInformation {
        label: format!("function {}({}){}", decl.name, param_str.join(", "), ret),
        documentation: None,
        parameters: Some(params),
        active_parameter: None,
    }
}

fn display_type(t: &psx_ast::TypeAnn) -> String {
    match t {
        psx_ast::TypeAnn::Named(n) => n.clone(),
        psx_ast::TypeAnn::Nullable(inner) => format!("?{}", display_type(inner)),
        psx_ast::TypeAnn::Generic(name, args) => {
            let parts: Vec<_> = args.iter().map(display_type).collect();
            format!("{name}<{}>", parts.join(", "))
        }
        psx_ast::TypeAnn::Union(parts) => parts
            .iter()
            .map(display_type)
            .collect::<Vec<_>>()
            .join(" | "),
    }
}

/// Return the variable name (without `$`) at `offset` if the cursor is on
/// one, else None.
fn identifier_at(source: &str, offset: u32) -> Option<String> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut start = (offset as usize).min(len);
    let mut end = start;
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if start == end {
        return None;
    }
    Some(source[start..end].to_string())
}

/// Walk the AST collecting byte offsets where `target_name` appears as a
/// variable. Recurses through expressions and nested statements.
fn collect_variable_offsets(module: &Module, target_name: &str, out: &mut Vec<u32>) {
    for stmt in &module.stmts {
        walk_stmt_for_var(stmt, target_name, out);
    }
}

fn walk_stmt_for_var(stmt: &Stmt, target: &str, out: &mut Vec<u32>) {
    let stmt_start = stmt.span().start;
    match stmt {
        Stmt::Expr(e, _) | Stmt::Throw(e, _) => walk_expr_for_var(e, target, out, stmt_start),
        Stmt::Return(opt, _) => {
            if let Some(e) = opt {
                walk_expr_for_var(e, target, out, stmt_start);
            }
        }
        Stmt::Block(stmts, _) | Stmt::Try { body: stmts, .. } => {
            for s in stmts {
                walk_stmt_for_var(s, target, out);
            }
        }
        Stmt::If {
            cond, then, else_, ..
        } => {
            walk_expr_for_var(cond, target, out, stmt_start);
            walk_stmt_for_var(then, target, out);
            if let Some(e) = else_ {
                walk_stmt_for_var(e, target, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            walk_expr_for_var(cond, target, out, stmt_start);
            walk_stmt_for_var(body, target, out);
        }
        Stmt::DoWhile { body, cond, .. } => {
            walk_stmt_for_var(body, target, out);
            walk_expr_for_var(cond, target, out, stmt_start);
        }
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                walk_stmt_for_var(i, target, out);
            }
            if let Some(c) = cond {
                walk_expr_for_var(c, target, out, stmt_start);
            }
            if let Some(s) = step {
                walk_expr_for_var(s, target, out, stmt_start);
            }
            walk_stmt_for_var(body, target, out);
        }
        Stmt::Break(_, _) | Stmt::Continue(_, _) => {}
        Stmt::Foreach {
            iter,
            value: _,
            body,
            ..
        } => {
            walk_expr_for_var(iter, target, out, stmt_start);
            walk_stmt_for_var(body, target, out);
        }
        Stmt::Function(decl) => {
            for s in &decl.body {
                walk_stmt_for_var(s, target, out);
            }
        }
        Stmt::Class(c) => {
            for m in &c.members {
                if let ClassMember::Method(method) = m {
                    if let Some(body) = &method.body {
                        for s in body {
                            walk_stmt_for_var(s, target, out);
                        }
                    }
                }
            }
        }
        Stmt::Trait(t) => {
            for m in &t.members {
                if let ClassMember::Method(method) = m {
                    if let Some(body) = &method.body {
                        for s in body {
                            walk_stmt_for_var(s, target, out);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn walk_expr_for_var(expr: &Expr, target: &str, out: &mut Vec<u32>, hint_start: u32) {
    use psx_ast::Expr as E;
    match expr {
        E::Var(name) if name == target => {
            // We don't store per-Expr spans (only call sites). Use the
            // hint_start as a fallback search location; the LSP rename
            // handler picks up the actual offsets by scanning the source.
            out.push(hint_start);
        }
        E::Call { callee, args, .. } => {
            walk_expr_for_var(callee, target, out, hint_start);
            for a in args {
                walk_expr_for_var(a, target, out, hint_start);
            }
        }
        E::Index { obj, key } => {
            walk_expr_for_var(obj, target, out, hint_start);
            walk_expr_for_var(key, target, out, hint_start);
        }
        E::Access { target: t, .. } => walk_expr_for_var(t, target, out, hint_start),
        E::New { args, .. } => {
            for a in args {
                walk_expr_for_var(a, target, out, hint_start);
            }
        }
        E::Assign { target: t, value }
        | E::CompoundAssign {
            target: t, value, ..
        } => {
            walk_expr_for_var(t, target, out, hint_start);
            walk_expr_for_var(value, target, out, hint_start);
        }
        E::Binary { lhs, rhs, .. } => {
            walk_expr_for_var(lhs, target, out, hint_start);
            walk_expr_for_var(rhs, target, out, hint_start);
        }
        E::Unary { expr, .. } | E::Await(expr) | E::FirstClassCallable(expr) => {
            walk_expr_for_var(expr, target, out, hint_start);
        }
        E::Ternary { cond, then, else_ } => {
            walk_expr_for_var(cond, target, out, hint_start);
            walk_expr_for_var(then, target, out, hint_start);
            walk_expr_for_var(else_, target, out, hint_start);
        }
        E::ShortTernary { cond, else_ } => {
            walk_expr_for_var(cond, target, out, hint_start);
            walk_expr_for_var(else_, target, out, hint_start);
        }
        _ => {}
    }
}

use psx_ast::Expr;

/// Look for `array(` starting at or just before `byte`. If found, return
/// the byte offset just past the closing paren.
fn find_array_call(source: &str, byte: u32) -> Option<u32> {
    let bytes = source.as_bytes();
    let start = (byte as usize).min(bytes.len());
    if !bytes
        .get(start..start + 6)
        .map_or(false, |s| s == b"array(")
    {
        return None;
    }
    // Walk forward to matching `)` at depth 0.
    let mut depth = 1;
    let mut i = start + 6;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((i + 1) as u32);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Walk the module collecting inlay hints. MVP: when a top-level `let
/// $name = <literal>` doesn't already have a type annotation, hint the
/// inferred primitive type.
fn collect_inlay_hints(
    module: &Module,
    source: &str,
    line_map: &LineMap,
    out: &mut Vec<InlayHint>,
) {
    for stmt in &module.stmts {
        if let Stmt::Expr(Expr::Assign { target, value }, span) = stmt {
            if let Expr::Var(name) = target.as_ref() {
                let Some(ty) = infer_literal_type(value) else {
                    continue;
                };
                // Place the hint right after the variable name. The byte
                // offset of `=` is after the var; we estimate by scanning.
                let name_token = format!("${name}");
                let stmt_text = byte_slice(source, span.start, span.end);
                if let Some(rel) = stmt_text.find(&name_token) {
                    let pos = span.start as usize + rel + name_token.len();
                    let lc = line_map.line_col(pos as u32);
                    out.push(InlayHint {
                        position: lc_to_position(lc),
                        label: InlayHintLabel::String(format!(": {ty}")),
                        kind: Some(InlayHintKind::TYPE),
                        text_edits: None,
                        tooltip: None,
                        padding_left: Some(false),
                        padding_right: Some(true),
                        data: None,
                    });
                }
            }
        }
    }
}

fn infer_literal_type(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Int(_) => Some("int"),
        Expr::Float(_) => Some("float"),
        Expr::Str(_) | Expr::InterpolatedStr(_) => Some("string"),
        Expr::Bool(_) => Some("bool"),
        Expr::Null => Some("?mixed"),
        _ => None,
    }
}
