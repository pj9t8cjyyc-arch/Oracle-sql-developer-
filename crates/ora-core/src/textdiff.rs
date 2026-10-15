//! Text comparison for SQL, PL/SQL, DDL and plain files: a line diff (patience algorithm) with
//! word-level highlights, optional normalisation (whitespace, case, SQL comments) and a
//! unified-patch writer. Normalisation only decides what counts as *equal*; the text shown is
//! always the original.

use serde::{Deserialize, Serialize};
use similar::{capture_diff_slices, Algorithm, ChangeTag, DiffOp, TextDiff};

use crate::lexer::lex;
use crate::{Error, Result};

/// Guard against comparing something that is not text-sized.
pub const MAX_LINES: usize = 200_000;
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct TextOptions {
    /// Treat runs of spaces/tabs as one space and ignore leading/trailing space.
    pub ignore_whitespace: bool,
    pub ignore_case: bool,
    /// Ignore `--` and `/* */` comments (SQL aware: not inside strings).
    pub ignore_sql_comments: bool,
    /// Lines that are empty after normalisation are not compared at all.
    pub ignore_blank_lines: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Equal,
    Delete,
    Insert,
    Change,
    /// `skipped` unchanged rows were left out.
    Skip,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffRow {
    pub kind: RowKind,
    pub a_no: Option<usize>,
    pub b_no: Option<usize>,
    pub a: Option<String>,
    pub b: Option<String>,
    /// Word-level highlights, only for `Change` rows.
    pub a_spans: Vec<Span>,
    pub b_spans: Vec<Span>,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TextStats {
    pub equal: usize,
    pub deleted: usize,
    pub inserted: usize,
    pub changed: usize,
    pub lines_a: usize,
    pub lines_b: usize,
    /// No difference once the chosen normalisation is applied.
    pub identical: bool,
    /// Exactly the same bytes.
    pub byte_identical: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextDiffResult {
    pub rows: Vec<DiffRow>,
    pub stats: TextStats,
}

/// Replaces SQL comments with spaces, keeping line breaks so line numbers stay valid.
pub fn mask_sql_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut keep = vec![false; chars.len()];
    for t in lex(text) {
        for k in keep.iter_mut().take(t.end.min(chars.len())).skip(t.start) {
            *k = true;
        }
    }
    chars
        .iter()
        .zip(keep)
        .map(|(c, k)| if k || c.is_whitespace() { *c } else { ' ' })
        .collect()
}

fn lines_of(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    // A final line break ends the last line; it does not start another one.
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

fn key_lines(text: &str, o: &TextOptions) -> Vec<String> {
    let masked;
    let src = if o.ignore_sql_comments {
        masked = mask_sql_comments(text);
        masked.as_str()
    } else {
        text
    };
    lines_of(src)
        .into_iter()
        .map(|l| {
            let mut k = if o.ignore_whitespace {
                l.split_whitespace().collect::<Vec<_>>().join(" ")
            } else {
                l.to_string()
            };
            if o.ignore_case {
                k = k.to_lowercase();
            }
            if o.ignore_sql_comments {
                // What a removed comment leaves behind at the end of a line is not a difference.
                k = k.trim_end().to_string();
            }
            k
        })
        .collect()
}

fn word_spans(a: &str, b: &str) -> (Vec<Span>, Vec<Span>) {
    let d = TextDiff::from_words(a, b);
    let (mut sa, mut sb): (Vec<Span>, Vec<Span>) = (vec![], vec![]);
    let push = |v: &mut Vec<Span>, text: &str, changed: bool| match v.last_mut() {
        Some(l) if l.changed == changed => l.text.push_str(text),
        _ => v.push(Span {
            text: text.to_string(),
            changed,
        }),
    };
    for c in d.iter_all_changes() {
        match c.tag() {
            ChangeTag::Equal => {
                push(&mut sa, c.value(), false);
                push(&mut sb, c.value(), false);
            }
            ChangeTag::Delete => push(&mut sa, c.value(), true),
            ChangeTag::Insert => push(&mut sb, c.value(), true),
        }
    }
    (sa, sb)
}

/// Compares two texts. `context` collapses long unchanged stretches into `Skip` rows
/// (`None` keeps every row).
pub fn diff_text(
    a: &str,
    b: &str,
    o: &TextOptions,
    context: Option<usize>,
) -> Result<TextDiffResult> {
    let (la, lb) = (lines_of(a), lines_of(b));
    if la.len() > MAX_LINES || lb.len() > MAX_LINES {
        return Err(Error::Invalid(format!(
            "too many lines to compare (more than {MAX_LINES})"
        )));
    }
    let (mut ka, mut kb) = (key_lines(a, o), key_lines(b, o));
    // Blank lines are dropped from the comparison but remembered by their original position.
    let (mut ia, mut ib): (Vec<usize>, Vec<usize>) =
        ((0..la.len()).collect(), (0..lb.len()).collect());
    if o.ignore_blank_lines {
        let keep = |k: &Vec<String>| {
            k.iter()
                .enumerate()
                .filter(|(_, s)| !s.trim().is_empty())
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        };
        ia = keep(&ka);
        ib = keep(&kb);
        ka = ia.iter().map(|&i| ka[i].clone()).collect();
        kb = ib.iter().map(|&i| kb[i].clone()).collect();
    }
    let ops = capture_diff_slices(Algorithm::Patience, &ka, &kb);
    let mut rows: Vec<DiffRow> = Vec::new();
    let mut st = TextStats {
        lines_a: la.len(),
        lines_b: lb.len(),
        byte_identical: a == b,
        ..Default::default()
    };
    let row = |kind, ai: Option<usize>, bi: Option<usize>| DiffRow {
        kind,
        a_no: ai.map(|i| i + 1),
        b_no: bi.map(|i| i + 1),
        a: ai.map(|i| la[i].to_string()),
        b: bi.map(|i| lb[i].to_string()),
        a_spans: vec![],
        b_spans: vec![],
        skipped: 0,
    };
    for op in ops {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    rows.push(row(
                        RowKind::Equal,
                        Some(ia[old_index + k]),
                        Some(ib[new_index + k]),
                    ));
                    st.equal += 1;
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    rows.push(row(RowKind::Delete, Some(ia[old_index + k]), None));
                    st.deleted += 1;
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    rows.push(row(RowKind::Insert, None, Some(ib[new_index + k])));
                    st.inserted += 1;
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let paired = old_len.min(new_len);
                for k in 0..paired {
                    let (i, j) = (ia[old_index + k], ib[new_index + k]);
                    let mut r = row(RowKind::Change, Some(i), Some(j));
                    let (sa, sb) = word_spans(la[i], lb[j]);
                    r.a_spans = sa;
                    r.b_spans = sb;
                    rows.push(r);
                    st.changed += 1;
                }
                for k in paired..old_len {
                    rows.push(row(RowKind::Delete, Some(ia[old_index + k]), None));
                    st.deleted += 1;
                }
                for k in paired..new_len {
                    rows.push(row(RowKind::Insert, None, Some(ib[new_index + k])));
                    st.inserted += 1;
                }
            }
        }
    }
    st.identical = st.deleted + st.inserted + st.changed == 0;
    if let Some(c) = context {
        rows = collapse(rows, c);
    }
    Ok(TextDiffResult { rows, stats: st })
}

fn collapse(rows: Vec<DiffRow>, context: usize) -> Vec<DiffRow> {
    let n = rows.len();
    let mut near = vec![false; n];
    for (i, r) in rows.iter().enumerate() {
        if r.kind != RowKind::Equal {
            let hi = (i + context).min(n.saturating_sub(1));
            near[i.saturating_sub(context)..=hi].fill(true);
        }
    }
    let mut out = Vec::new();
    let mut skipped = 0;
    for (r, keep) in rows.into_iter().zip(near) {
        if keep {
            if skipped > 0 {
                out.push(DiffRow {
                    kind: RowKind::Skip,
                    a_no: None,
                    b_no: None,
                    a: None,
                    b: None,
                    a_spans: vec![],
                    b_spans: vec![],
                    skipped,
                });
                skipped = 0;
            }
            out.push(r);
        } else {
            skipped += 1;
        }
    }
    if skipped > 0 {
        out.push(DiffRow {
            kind: RowKind::Skip,
            a_no: None,
            b_no: None,
            a: None,
            b: None,
            a_spans: vec![],
            b_spans: vec![],
            skipped,
        });
    }
    out
}

/// A unified patch (`diff -u` style) of the original texts, using the same notion of equality.
pub fn unified_patch(
    a: &str,
    b: &str,
    o: &TextOptions,
    a_name: &str,
    b_name: &str,
    context: usize,
) -> Result<String> {
    let r = diff_text(a, b, o, None)?;
    if r.stats.identical {
        return Ok(String::new());
    }
    let rows = &r.rows;
    let mut out = format!("--- {a_name}\n+++ {b_name}\n");
    let n = rows.len();
    let mut i = 0;
    while i < n {
        if rows[i].kind == RowKind::Equal {
            i += 1;
            continue;
        }
        // Start a hunk `context` rows before the first change and extend while changes are close.
        let start = i.saturating_sub(context);
        let mut end = i;
        let mut last_change = i;
        while end < n && end <= last_change + context * 2 {
            if rows[end].kind != RowKind::Equal {
                last_change = end;
            }
            end += 1;
        }
        let end = (last_change + context + 1).min(n);
        let slice = &rows[start..end];
        let a_start = slice.iter().find_map(|r| r.a_no).unwrap_or(0);
        let b_start = slice.iter().find_map(|r| r.b_no).unwrap_or(0);
        let a_len = slice.iter().filter(|r| r.a_no.is_some()).count();
        let b_len = slice.iter().filter(|r| r.b_no.is_some()).count();
        out.push_str(&format!("@@ -{a_start},{a_len} +{b_start},{b_len} @@\n"));
        for r in slice {
            match r.kind {
                RowKind::Equal => out.push_str(&format!(" {}\n", r.a.as_deref().unwrap_or(""))),
                RowKind::Delete => out.push_str(&format!("-{}\n", r.a.as_deref().unwrap_or(""))),
                RowKind::Insert => out.push_str(&format!("+{}\n", r.b.as_deref().unwrap_or(""))),
                RowKind::Change => {
                    out.push_str(&format!(
                        "-{}\n+{}\n",
                        r.a.as_deref().unwrap_or(""),
                        r.b.as_deref().unwrap_or("")
                    ));
                }
                RowKind::Skip => {}
            }
        }
        i = end;
    }
    Ok(out)
}

/// Reads a text file: honours a UTF-8/UTF-16 byte-order mark, otherwise UTF-8 with a Windows-1252
/// fallback, so old scripts do not fail on an accented character.
pub fn read_text_file(path: &std::path::Path) -> Result<String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::Invalid(format!(
            "{} is larger than {} MB",
            path.display(),
            MAX_FILE_BYTES / 1024 / 1024
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
    Ok(decode_text(&bytes))
}

pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode(rest).0.into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE.decode(rest).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(r: &TextDiffResult) -> Vec<RowKind> {
        r.rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn insertions_deletions_and_changes_carry_line_numbers() {
        let a = "SELECT 1\nFROM dual\nWHERE 1 = 1\n";
        let b = "SELECT 1\nFROM dual d\nWHERE 1 = 1\nAND 2 = 2\n";
        let r = diff_text(a, b, &TextOptions::default(), None).unwrap();
        assert_eq!(
            kinds(&r),
            vec![
                RowKind::Equal,
                RowKind::Change,
                RowKind::Equal,
                RowKind::Insert
            ]
        );
        assert_eq!(
            (
                r.stats.changed,
                r.stats.inserted,
                r.stats.deleted,
                r.stats.equal
            ),
            (1, 1, 0, 2)
        );
        let c = &r.rows[1];
        assert_eq!((c.a_no, c.b_no), (Some(2), Some(2)));
        assert_eq!(r.rows[3].b_no, Some(4));
        assert!(!r.stats.identical && !r.stats.byte_identical);
    }

    #[test]
    fn word_level_highlights_mark_only_what_changed() {
        let r = diff_text(
            "SELECT a, b FROM t\n",
            "SELECT a, c FROM t\n",
            &TextOptions::default(),
            None,
        )
        .unwrap();
        let row = &r.rows[0];
        let changed_a: Vec<&str> = row
            .a_spans
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.as_str())
            .collect();
        let changed_b: Vec<&str> = row
            .b_spans
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!((changed_a, changed_b), (vec!["b"], vec!["c"]));
        let joined: String = row.a_spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "SELECT a, b FROM t");
    }

    #[test]
    fn normalisation_decides_equality_but_the_original_text_is_shown() {
        let a = "select  a,b\n  from t -- old note\n";
        let b = "SELECT a,b\nFROM t /* new note */\n";
        let none = diff_text(a, b, &TextOptions::default(), None).unwrap();
        assert!(!none.stats.identical);
        let o = TextOptions {
            ignore_whitespace: true,
            ignore_case: true,
            ignore_sql_comments: true,
            ignore_blank_lines: false,
        };
        let r = diff_text(a, b, &o, None).unwrap();
        assert!(r.stats.identical, "{:?}", kinds(&r));
        assert_eq!(r.rows[0].a.as_deref(), Some("select  a,b"));
        assert_eq!(r.rows[0].b.as_deref(), Some("SELECT a,b"));
    }

    #[test]
    fn comment_markers_inside_strings_are_not_comments() {
        let a = "SELECT '-- not a comment' FROM t\n";
        let b = "SELECT '-- different' FROM t\n";
        let o = TextOptions {
            ignore_sql_comments: true,
            ..Default::default()
        };
        assert!(!diff_text(a, b, &o, None).unwrap().stats.identical);
        assert_eq!(
            mask_sql_comments("a /* x\ny */ b -- z\nc"),
            "a     \n     b     \nc"
        );
    }

    #[test]
    fn ignoring_blank_lines_keeps_original_line_numbers() {
        let a = "A\n\n\nB\n";
        let b = "A\nB\nC\n";
        let o = TextOptions {
            ignore_blank_lines: true,
            ..Default::default()
        };
        let r = diff_text(a, b, &o, None).unwrap();
        assert_eq!(
            (r.stats.inserted, r.stats.deleted, r.stats.changed),
            (1, 0, 0)
        );
        let b_row = r.rows.iter().find(|r| r.a.as_deref() == Some("B")).unwrap();
        assert_eq!((b_row.a_no, b_row.b_no), (Some(4), Some(2)));
    }

    #[test]
    fn long_unchanged_stretches_collapse_to_a_skip_row() {
        let a: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let b = a.replace("line 50\n", "line fifty\n");
        let r = diff_text(&a, &b, &TextOptions::default(), Some(2)).unwrap();
        assert_eq!(
            r.rows.len(),
            1 + 2 + 1 + 2 + 1,
            "skip, 2 context, change, 2 context, skip"
        );
        assert_eq!(r.rows[0].kind, RowKind::Skip);
        assert_eq!(r.rows[0].skipped, 48);
        assert_eq!(r.rows.last().unwrap().skipped, 47);
        assert_eq!(r.stats.equal, 99);
    }

    #[test]
    fn identical_and_empty_inputs() {
        let r = diff_text("x\ny\n", "x\ny\n", &TextOptions::default(), None).unwrap();
        assert!(r.stats.identical && r.stats.byte_identical);
        let r = diff_text("", "a\nb", &TextOptions::default(), None).unwrap();
        assert_eq!(
            (r.stats.inserted, r.stats.lines_a, r.stats.lines_b),
            (2, 0, 2)
        );
        let r = diff_text("a\r\nb\r\n", "a\nb\n", &TextOptions::default(), None).unwrap();
        assert!(r.stats.identical, "line endings alone are not a difference");
    }

    #[test]
    fn the_unified_patch_has_hunks_with_context() {
        let a: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let b = a.replace("l10\n", "ten\n").replace("l18\n", "");
        let p = unified_patch(&a, &b, &TextOptions::default(), "a.sql", "b.sql", 2).unwrap();
        assert!(
            p.starts_with("--- a.sql\n+++ b.sql\n@@ -8,5 +8,5 @@\n"),
            "{p}"
        );
        assert!(p.contains("-l10\n+ten\n") && p.contains("-l18\n"), "{p}");
        assert_eq!(p.matches("@@ -").count(), 2, "two separate hunks: {p}");
        assert_eq!(
            unified_patch("x", "x", &TextOptions::default(), "a", "b", 3).unwrap(),
            ""
        );
    }

    #[test]
    fn text_files_decode_by_bom_and_fall_back_for_old_encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(decode_text(b"caf\xE9"), "café");
        assert_eq!(decode_text("café".as_bytes()), "café");
    }

    #[test]
    fn huge_inputs_are_refused_not_attempted() {
        let big: String = "x\n".repeat(MAX_LINES + 1);
        assert!(diff_text(&big, "x", &TextOptions::default(), None).is_err());
    }

    #[test]
    fn ignoring_comments_ignores_the_space_they_leave_behind() {
        let a = "UPDATE t SET x = 1; -- short\n";
        let b = "UPDATE t SET x = 1; /* a much longer comment */\n";
        let o = TextOptions {
            ignore_sql_comments: true,
            ..Default::default()
        };
        assert!(diff_text(a, b, &o, None).unwrap().stats.identical);
        assert!(
            !diff_text(a, b, &TextOptions::default(), None)
                .unwrap()
                .stats
                .identical
        );
    }
}
