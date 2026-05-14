//! End-to-end tests for the LSP handler logic that doesn't require a live
//! `Client`. We test against `ParsedDocument` + the pure helper functions
//! exposed through the server module's public surface.

use psx_lsp::index::ParsedDocument;
use tower_lsp::lsp_types::Url;

fn doc(uri: &str, source: &str) -> ParsedDocument {
    ParsedDocument::from_source(Url::parse(uri).unwrap(), source.to_string())
}

#[test]
fn parsed_document_carries_module_for_valid_source() {
    let d = doc("file:///a.psx", "function f(): void {}\n");
    assert!(d.module.is_some());
    assert!(d.parse_error.is_none());
}

#[test]
fn parsed_document_records_parse_error_for_malformed_source() {
    let d = doc("file:///bad.psx", "function f(\n");
    assert!(d.module.is_none());
    let (msg, span) = d.parse_error.expect("expected parse error");
    assert!(!msg.is_empty(), "diagnostic message must not be empty");
    assert!(span.0 <= span.1, "span must be non-decreasing");
}

#[test]
fn formatter_round_trips_basic_module() {
    let d = doc(
        "file:///fmt.psx",
        "namespace App;\n\nfunction add(int $a, int $b): int { return $a + $b; }\n",
    );
    let module = d.module.as_ref().expect("parses");
    let formatted = psx_printer::format_module(module);
    assert!(formatted.contains("namespace App;"));
    assert!(formatted.contains("function add(int $a, int $b): int"));
}

#[test]
fn line_map_round_trips_through_document() {
    let src = "namespace App;\n\nfunction greet(): string {\n    return \"hi\";\n}\n";
    let d = doc("file:///g.psx", src);
    // Line 2 (0-indexed = line 2 ≈ "function greet ...") starts at the
    // offset of 'f' in "function". Round-trip via line_map.
    let off = src.find("function").unwrap() as u32;
    let lc = d.line_map.line_col(off);
    let back = d.line_map.byte_of(lc.line, lc.column);
    assert_eq!(back, off);
}
