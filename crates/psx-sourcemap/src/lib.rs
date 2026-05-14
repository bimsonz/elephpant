//! Source-map-v3 builder for PHPScript.
//!
//! Three pieces:
//! - [`LineMap`] precomputes byte-offset → (line, column) lookups for a
//!   source file (used both by the parser and at SM serialisation time).
//! - [`vlq`] encodes signed integers as base-64 VLQ per the source-map spec.
//! - [`SourceMapBuilder`] records `(generated_line, generated_col, source_idx,
//!   source_line, source_col)` tuples and serialises them to v3 JSON.
//!
//! Mappings are stored as deltas internally so the public API can stay
//! "absolute positions in, absolute positions out".

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

/// Precomputed byte-offset → (0-indexed line, 0-indexed column) table for a
/// single source file. Built once per file; lookups are O(log n) via binary
/// search against the line-start offsets.
#[derive(Debug, Clone)]
pub struct LineMap {
    /// Byte offset of the start of each line. `line_starts[0]` is always 0.
    /// `line_starts[i]` is the byte offset of the first character of line i.
    line_starts: Vec<u32>,
    /// Total byte length of the source (used to clamp out-of-range offsets).
    source_len: u32,
}

impl LineMap {
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                let next = (i as u32) + 1;
                line_starts.push(next);
            }
        }
        Self {
            line_starts,
            source_len: source.len() as u32,
        }
    }

    pub fn line_col(&self, byte_offset: u32) -> LineCol {
        let off = byte_offset.min(self.source_len);
        // Binary-search for the greatest line_starts[i] <= off.
        let idx = match self.line_starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        LineCol {
            line: idx as u32,
            column: off - self.line_starts[idx],
        }
    }

    /// Number of lines (`= line_starts.len()`); always at least 1.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Inverse of [`LineMap::line_col`]: convert a 0-indexed (line, column)
    /// pair back to the byte offset of that position in the source.
    /// Out-of-range inputs clamp to the end of the relevant line / end of
    /// the source.
    pub fn byte_of(&self, line: u32, column: u32) -> u32 {
        let li = (line as usize).min(self.line_starts.len() - 1);
        let line_start = self.line_starts[li];
        let next_line_start = self
            .line_starts
            .get(li + 1)
            .copied()
            .unwrap_or(self.source_len + 1);
        // Each line ends at the byte BEFORE the next line's start (which is
        // the position of the newline). Treat the newline itself as part of
        // the line for clamping purposes.
        let line_end = next_line_start.saturating_sub(1).max(line_start);
        (line_start + column).min(line_end).min(self.source_len)
    }
}

/// Base-64 VLQ encoding per the source-map v3 spec.
///
/// Values are sign-magnitude encoded: the low bit of the first 5-bit group is
/// the sign, and each 5-bit group has a continuation bit (the high bit) set
/// if more groups follow.
pub mod vlq {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(value: i64, out: &mut String) {
        // Sign-magnitude: low bit of the first group is sign; the rest is the
        // absolute value, 5 bits at a time.
        let mut v = if value < 0 {
            ((-value as u64) << 1) | 1
        } else {
            (value as u64) << 1
        };
        loop {
            let mut digit = (v & 0b11111) as u8;
            v >>= 5;
            if v != 0 {
                digit |= 0b100000; // continuation bit
            }
            out.push(ALPHABET[digit as usize] as char);
            if v == 0 {
                break;
            }
        }
    }

    pub fn encode_all(values: &[i64], out: &mut String) {
        for &v in values {
            encode(v, out);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Mapping {
    /// 0-indexed line in the generated file this mapping applies to.
    generated_line: u32,
    /// 0-indexed column in the generated file (start of the segment).
    generated_column: u32,
    /// Index into `sources` array.
    source_idx: u32,
    /// 0-indexed line in the source file.
    source_line: u32,
    /// 0-indexed column in the source file.
    source_column: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SourceMapBuilder {
    /// Path of the generated file (used for the `file` field).
    pub file: Option<String>,
    sources: Vec<String>,
    /// Optional inline source text for each entry in `sources`.
    sources_content: Vec<Option<String>>,
    mappings: Vec<Mapping>,
}

impl SourceMapBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source file. Returns its index for later use in
    /// [`SourceMapBuilder::record`].
    pub fn add_source(&mut self, name: impl Into<String>, content: Option<String>) -> u32 {
        let idx = self.sources.len() as u32;
        self.sources.push(name.into());
        self.sources_content.push(content);
        idx
    }

    /// Record a mapping from a position in the generated file back to a
    /// position in a registered source. All positions are 0-indexed.
    pub fn record(&mut self, generated: LineCol, source_idx: u32, source: LineCol) {
        self.mappings.push(Mapping {
            generated_line: generated.line,
            generated_column: generated.column,
            source_idx,
            source_line: source.line,
            source_column: source.column,
        });
    }

    /// Serialise to source-map-v3 JSON.
    pub fn to_json(&self) -> String {
        // Sort mappings by (generated_line, generated_column) to satisfy the
        // spec, which requires ordered segments per line.
        let mut sorted = self.mappings.clone();
        sorted.sort_by_key(|m| (m.generated_line, m.generated_column));

        let mappings = encode_mappings(&sorted);

        #[derive(Serialize)]
        struct V3<'a> {
            version: u32,
            #[serde(skip_serializing_if = "Option::is_none")]
            file: Option<&'a str>,
            sources: &'a [String],
            #[serde(rename = "sourcesContent", skip_serializing_if = "is_all_none")]
            sources_content: &'a [Option<String>],
            names: &'a [&'a str],
            mappings: String,
        }

        fn is_all_none(v: &&[Option<String>]) -> bool {
            v.iter().all(|x| x.is_none())
        }

        let v3 = V3 {
            version: 3,
            file: self.file.as_deref(),
            sources: &self.sources,
            sources_content: &self.sources_content,
            names: &[],
            mappings,
        };
        serde_json::to_string(&v3).expect("source map v3 always serialises")
    }
}

fn encode_mappings(sorted: &[Mapping]) -> String {
    // Spec rules for the `mappings` string:
    // - One "group" per generated line, separated by `;`.
    // - Within a group, segments separated by `,`.
    // - A segment is 4 or 5 VLQ-encoded numbers, each value delta-encoded
    //   against the previous segment for fields 1–5 (generated_col resets
    //   to 0 at the start of each new line; the others persist across lines).
    let mut out = String::new();

    let mut prev_gen_col: i64 = 0;
    let mut prev_source_idx: i64 = 0;
    let mut prev_source_line: i64 = 0;
    let mut prev_source_col: i64 = 0;
    let mut current_line: u32 = 0;
    let mut first_segment_on_line = true;

    for m in sorted {
        while current_line < m.generated_line {
            out.push(';');
            current_line += 1;
            prev_gen_col = 0;
            first_segment_on_line = true;
        }
        if !first_segment_on_line {
            out.push(',');
        }
        first_segment_on_line = false;

        let gen_col_delta = m.generated_column as i64 - prev_gen_col;
        let source_idx_delta = m.source_idx as i64 - prev_source_idx;
        let source_line_delta = m.source_line as i64 - prev_source_line;
        let source_col_delta = m.source_column as i64 - prev_source_col;

        vlq::encode_all(
            &[
                gen_col_delta,
                source_idx_delta,
                source_line_delta,
                source_col_delta,
            ],
            &mut out,
        );

        prev_gen_col = m.generated_column as i64;
        prev_source_idx = m.source_idx as i64;
        prev_source_line = m.source_line as i64;
        prev_source_col = m.source_column as i64;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- LineMap ----------------

    #[test]
    fn linemap_single_line() {
        let lm = LineMap::new("hello");
        assert_eq!(lm.line_col(0), LineCol { line: 0, column: 0 });
        assert_eq!(lm.line_col(4), LineCol { line: 0, column: 4 });
    }

    #[test]
    fn linemap_two_lines() {
        let lm = LineMap::new("ab\ncd");
        assert_eq!(lm.line_col(0), LineCol { line: 0, column: 0 });
        assert_eq!(lm.line_col(2), LineCol { line: 0, column: 2 }); // the \n
        assert_eq!(lm.line_col(3), LineCol { line: 1, column: 0 });
        assert_eq!(lm.line_col(4), LineCol { line: 1, column: 1 });
    }

    #[test]
    fn linemap_clamps_out_of_range() {
        let lm = LineMap::new("ab");
        // Offset past the end clamps to source_len → line 0, col 2.
        assert_eq!(lm.line_col(99), LineCol { line: 0, column: 2 });
    }

    #[test]
    fn linemap_byte_of_roundtrips_line_col() {
        let lm = LineMap::new("ab\ncde\nf");
        // Round-trip the line-col positions we computed earlier.
        for off in 0..=8u32 {
            let lc = lm.line_col(off);
            let back = lm.byte_of(lc.line, lc.column);
            assert_eq!(back, off, "byte_of round-trip failed at offset {off}");
        }
    }

    #[test]
    fn linemap_byte_of_clamps_column_to_line_end() {
        let lm = LineMap::new("ab\ncde\nf");
        // Asking for column 99 on line 1 (which only has 3 chars + newline)
        // clamps to the byte BEFORE the newline at the end of line 1.
        assert_eq!(lm.byte_of(1, 99), 6); // '\n' is at offset 6
    }

    #[test]
    fn linemap_handles_trailing_newline() {
        let lm = LineMap::new("ab\n");
        assert_eq!(lm.line_count(), 2);
        assert_eq!(lm.line_col(3), LineCol { line: 1, column: 0 });
    }

    // ---------------- VLQ ----------------

    #[test]
    fn vlq_zero() {
        let mut s = String::new();
        vlq::encode(0, &mut s);
        assert_eq!(s, "A");
    }

    #[test]
    fn vlq_small_positive() {
        // 1 -> 'C', 2 -> 'E' (each value is doubled because of the sign bit).
        let mut s = String::new();
        vlq::encode(1, &mut s);
        assert_eq!(s, "C");
        let mut s = String::new();
        vlq::encode(2, &mut s);
        assert_eq!(s, "E");
    }

    #[test]
    fn vlq_small_negative() {
        // -1 -> 'D' (sign bit set; magnitude 1 → low 5 bits = 0b00011).
        let mut s = String::new();
        vlq::encode(-1, &mut s);
        assert_eq!(s, "D");
    }

    #[test]
    fn vlq_multi_group() {
        // 16 -> two groups: low 5 bits = 0b00000, continuation, then 1 more.
        // 16 << 1 = 32 = 0b100000 -> first 5 bits = 0, continuation set;
        // remaining = 1.
        let mut s = String::new();
        vlq::encode(16, &mut s);
        assert_eq!(s, "gB");
    }

    #[test]
    fn vlq_roundtrip_against_reference() {
        // Known fixtures from the source-map spec / mozilla source-map lib.
        let cases: &[(i64, &str)] = &[
            (0, "A"),
            (1, "C"),
            (-1, "D"),
            (16, "gB"),
            (123, "2H"),
            (1000, "w+B"),
            (-1000, "x+B"),
        ];
        for (n, expected) in cases {
            let mut s = String::new();
            vlq::encode(*n, &mut s);
            assert_eq!(s, *expected, "vlq({n}) should be {expected}");
        }
    }

    // ---------------- SourceMapBuilder ----------------

    #[test]
    fn smbuilder_empty_emits_valid_v3() {
        let b = SourceMapBuilder::new();
        let json: serde_json::Value = serde_json::from_str(&b.to_json()).unwrap();
        assert_eq!(json["version"], 3);
        assert_eq!(json["mappings"], "");
        assert!(json["sources"].as_array().unwrap().is_empty());
    }

    #[test]
    fn smbuilder_one_mapping_per_line() {
        // Three TS lines, each maps to its corresponding source line at col 0.
        let mut b = SourceMapBuilder::new();
        let s = b.add_source("a.psx", None);
        b.record(
            LineCol { line: 0, column: 0 },
            s,
            LineCol { line: 5, column: 0 },
        );
        b.record(
            LineCol { line: 1, column: 0 },
            s,
            LineCol { line: 6, column: 0 },
        );
        b.record(
            LineCol { line: 2, column: 0 },
            s,
            LineCol { line: 7, column: 0 },
        );
        let json: serde_json::Value = serde_json::from_str(&b.to_json()).unwrap();
        // Each `;` starts a new generated line. Three groups -> two `;`s.
        let mappings = json["mappings"].as_str().unwrap();
        assert_eq!(mappings.matches(';').count(), 2);
    }

    #[test]
    fn smbuilder_includes_sources_content_when_set() {
        let mut b = SourceMapBuilder::new();
        b.add_source("a.psx", Some("hello\n".to_string()));
        let json: serde_json::Value = serde_json::from_str(&b.to_json()).unwrap();
        let content = json["sourcesContent"].as_array().unwrap();
        assert_eq!(content[0].as_str().unwrap(), "hello\n");
    }

    #[test]
    fn smbuilder_omits_sources_content_when_all_none() {
        let mut b = SourceMapBuilder::new();
        b.add_source("a.psx", None);
        let json: serde_json::Value = serde_json::from_str(&b.to_json()).unwrap();
        assert!(json.get("sourcesContent").is_none());
    }

    #[test]
    fn smbuilder_segments_delta_encoded_per_line() {
        // Two mappings on the same generated line at columns 0 and 4.
        // Both point at the same source position.
        let mut b = SourceMapBuilder::new();
        let s = b.add_source("a.psx", None);
        b.record(
            LineCol { line: 0, column: 0 },
            s,
            LineCol { line: 0, column: 0 },
        );
        b.record(
            LineCol { line: 0, column: 4 },
            s,
            LineCol { line: 0, column: 0 },
        );
        let json: serde_json::Value = serde_json::from_str(&b.to_json()).unwrap();
        let mappings = json["mappings"].as_str().unwrap();
        // Comma separates segments within a generated line.
        assert!(
            mappings.contains(','),
            "expected comma-separated segments, got {mappings}"
        );
        // No `;` since both are on the same line.
        assert!(!mappings.contains(';'));
    }
}
