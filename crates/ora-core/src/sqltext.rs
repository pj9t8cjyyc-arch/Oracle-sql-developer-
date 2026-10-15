//! Text-level helpers for stored SQL: statement splitting, canonical form and
//! fingerprint (for de-duplication and "same kind of query"), secret
//! redaction and automatic titles. All built on the shared lexer, so
//! comments, strings and quoted identifiers are handled properly.

use sha2::{Digest, Sha256};

use crate::guard::{analyze, is_plsql, StatementKind};
use crate::lexer::{lex, Tok, Token};
use crate::parse::references;
use crate::profile::Environment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stmt {
    /// Char range in the source text.
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub kind: StatementKind,
}

/// Whether a line holds only `/` (the SQL*Plus terminator for PL/SQL blocks).
fn slash_alone(chars: &[char], tok: &Token) -> bool {
    if tok.tok != Tok::Punct('/') {
        return false;
    }
    let before = chars[..tok.start]
        .iter()
        .rev()
        .take_while(|c| **c != '\n')
        .all(|c| c.is_whitespace());
    let after = chars[tok.end..]
        .iter()
        .take_while(|c| **c != '\n')
        .all(|c| c.is_whitespace());
    before && after
}

/// Split a script into statements. Plain statements end at a top-level `;`.
/// PL/SQL blocks and stored-unit DDL (BEGIN/DECLARE/CREATE PROCEDURE …) contain
/// `;` themselves, so they end at a line holding only `/` (SQL*Plus
/// convention) or at the end of the text. Comment-only fragments are dropped;
/// comments attached to a statement stay with it.
pub fn split_statements(text: &str) -> Vec<Stmt> {
    let chars: Vec<char> = text.chars().collect();
    let toks = lex(text);
    let mut out = Vec::new();
    let mut seg_start = 0usize; // char index where the current statement's text begins
    let mut first_tok: Option<usize> = None; // index of the first token of the current statement
    let mut depth = 0i32;
    let mut plsql = false;

    let flush = |from: usize, to: usize, out: &mut Vec<Stmt>| {
        let slice: String = chars[from..to.min(chars.len())].iter().collect();
        let trimmed = slice.trim();
        if trimmed.is_empty() {
            return;
        }
        let lead = slice.chars().take_while(|c| c.is_whitespace()).count();
        let kind = analyze(trimmed, Environment::Dev).kind;
        if kind == StatementKind::Empty {
            return;
        }
        out.push(Stmt {
            start: from + lead,
            end: from + lead + trimmed.chars().count(),
            text: trimmed.to_string(),
            kind,
        });
    };

    for (i, t) in toks.iter().enumerate() {
        if first_tok.is_none() {
            first_tok = Some(i);
            let rest: Vec<Tok> = toks[i..].iter().take(8).map(|x| x.tok.clone()).collect();
            plsql = is_plsql(&rest);
            depth = 0;
        }
        match &t.tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            Tok::Punct(';') if !plsql && depth <= 0 => {
                flush(seg_start, t.start, &mut out);
                seg_start = t.end;
                first_tok = None;
            }
            Tok::Punct('/') if plsql && slash_alone(&chars, t) => {
                flush(seg_start, t.start, &mut out);
                seg_start = t.end;
                first_tok = None;
            }
            _ => {}
        }
    }
    flush(seg_start, chars.len(), &mut out);
    out
}

fn push_tok(parts: &mut Vec<String>, tok: &Tok, src: &str, literals: bool) {
    match tok {
        Tok::Word(w) => parts.push(w.clone()),
        Tok::Quoted(q) => parts.push(format!("\"{q}\"")),
        Tok::Punct(c) => parts.push(c.to_string()),
        Tok::Other => {
            if literals {
                parts.push(src.to_string());
            } else {
                // `? , ?` runs (IN-lists, VALUES) collapse to a single `?`
                let n = parts.len();
                if n >= 2 && parts[n - 1] == "," && parts[n - 2] == "?" {
                    parts.truncate(n - 2);
                }
                parts.push("?".into());
            }
        }
    }
}

fn normalised(sql: &str, literals: bool) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut toks = lex(sql);
    while matches!(toks.last().map(|t| &t.tok), Some(Tok::Punct(';'))) {
        toks.pop();
    }
    let mut parts = Vec::new();
    for t in &toks {
        let src: String = chars[t.start..t.end].iter().collect();
        push_tok(&mut parts, &t.tok, &src, literals);
    }
    parts.join(" ")
}

/// Whitespace-, case- and comment-insensitive form; literals are kept, so two
/// runs with different values are different queries.
pub fn canonical(sql: &str) -> String {
    normalised(sql, true)
}

/// Like [`canonical`] but literals become `?`: groups "the same kind of query".
pub fn fingerprint(sql: &str) -> String {
    normalised(sql, false)
}

/// Stable 128-bit identity of a query's canonical form.
pub fn sql_hash(sql: &str) -> String {
    let d = Sha256::digest(canonical(sql).as_bytes());
    d[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Space-separated, upper-cased table names a statement touches.
pub fn table_names(sql: &str) -> String {
    let mut names: Vec<String> = references(sql)
        .into_iter()
        .map(|r| {
            r.schema
                .map(|s| format!("{s}.{}", r.name))
                .unwrap_or(r.name)
        })
        .collect();
    names.sort();
    names.dedup();
    names.join(" ")
}

/// Stronger masking for text that leaves the application (exports, AI context): everything
/// [`redact_secrets`] does, plus `IDENTIFIED BY <x>` and credential-looking string literals
/// (`password := '...'`, `api_key => '...'`) **even inside comments**, which a statement lexer skips.
pub fn redact_for_export(text: &str) -> String {
    let t = redact_secrets(text);
    let mut out = String::with_capacity(t.len());
    for line in t.split_inclusive('\n') {
        out.push_str(&mask_line(line));
    }
    out
}

const SECRET_KEYS: [&str; 8] = [
    "password",
    "passwd",
    "pwd",
    "secret",
    "api_key",
    "apikey",
    "token",
    "passphrase",
];

fn mask_line(line: &str) -> String {
    let mut s = line.to_string();
    // IDENTIFIED BY <token | "quoted">
    let mut from = 0;
    loop {
        let lower = s.to_ascii_lowercase();
        let Some(p) = lower[from..].find("identified") else {
            break;
        };
        let at = from + p;
        let mut i = at + "identified".len();
        let b = s.as_bytes();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if !lower[i..].starts_with("by") {
            from = at + 1;
            continue;
        }
        i += 2;
        let ws = i;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == ws || i >= b.len() {
            from = at + 1;
            continue;
        }
        let start = i;
        let end = if b[i] == b'"' {
            s[i + 1..].find('"').map(|q| i + 2 + q).unwrap_or(b.len())
        } else {
            let mut e = i;
            while e < b.len() && !b[e].is_ascii_whitespace() && !matches!(b[e], b';' | b',' | b')')
            {
                e += 1;
            }
            e
        };
        if lower[start..end].starts_with("values") {
            from = at + 1;
            continue;
        }
        s.replace_range(start..end, "********");
        from = start + 8;
    }
    // key := 'literal'  /  key => 'literal'  /  key = 'literal'
    for key in SECRET_KEYS {
        let mut from = 0;
        loop {
            let lower = s.to_ascii_lowercase();
            let Some(p) = lower[from..].find(key) else {
                break;
            };
            let mut i = from + p + key.len();
            let b = s.as_bytes();
            let skip = |mut i: usize| {
                while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                i
            };
            i = skip(i);
            let op = if lower[i..].starts_with(":=") || lower[i..].starts_with("=>") {
                2
            } else if lower[i..].starts_with('=') {
                1
            } else {
                0
            };
            if op == 0 {
                from += p + key.len();
                continue;
            }
            i = skip(i + op);
            if i < b.len() && b[i] == b'\'' {
                if let Some(q) = s[i + 1..].find('\'') {
                    s.replace_range(i + 1..i + 1 + q, "********");
                    from = i + 10;
                    continue;
                }
            }
            from += p + key.len();
        }
    }
    s
}

/// Mask passwords before anything is stored: `IDENTIFIED BY [VALUES] x [REPLACE y]`
/// and SQL*Plus `CONNECT user/password`.
pub fn redact_secrets(sql: &str) -> String {
    let mut chars: Vec<char> = sql.chars().collect();
    let toks = lex(sql);
    let word =
        |i: usize, w: &str| matches!(toks.get(i).map(|t| &t.tok), Some(Tok::Word(x)) if x == w);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let secret = |i: usize| {
        toks.get(i)
            .filter(|t| !matches!(t.tok, Tok::Punct(_)))
            .map(|t| (t.start, t.end))
    };
    for i in 0..toks.len() {
        if word(i, "IDENTIFIED") && word(i + 1, "BY") {
            let mut j = i + 2;
            if word(j, "VALUES") {
                j += 1;
            }
            if let Some(s) = secret(j) {
                spans.push(s);
                if word(j + 1, "REPLACE") {
                    if let Some(s) = secret(j + 2) {
                        spans.push(s);
                    }
                }
            }
        }
        // CONNECT user/password[@db] at the start of a statement
        let stmt_start = i == 0 || matches!(toks[i - 1].tok, Tok::Punct(';'));
        if stmt_start
            && (word(i, "CONNECT") || word(i, "CONN"))
            && matches!(toks.get(i + 2).map(|t| &t.tok), Some(Tok::Punct('/')))
        {
            if let Some(s) = secret(i + 3) {
                spans.push(s);
            }
        }
    }
    spans.sort();
    for (s, e) in spans.into_iter().rev() {
        chars.splice(s..e, "***".chars());
    }
    chars.into_iter().collect()
}

/// A readable title from the first statements: `SELECT HR.EMPLOYEES (+2 more)`.
pub fn auto_title(script: &str) -> String {
    let stmts = split_statements(script);
    let Some(first) = stmts.first() else {
        return "Untitled".into();
    };
    let kind = match first.kind {
        StatementKind::PlSql => "PL/SQL".to_string(),
        StatementKind::Ddl => first
            .text
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .to_uppercase(),
        k => format!("{k:?}").to_uppercase(),
    };
    let tables = table_names(&first.text);
    let mut title = if tables.is_empty() {
        kind
    } else {
        format!(
            "{kind} {}",
            tables.split(' ').take(2).collect::<Vec<_>>().join(", ")
        )
    };
    if stmts.len() > 1 {
        title.push_str(&format!(" (+{} more)", stmts.len() - 1));
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<String> {
        split_statements(s).into_iter().map(|x| x.text).collect()
    }

    #[test]
    fn splits_plain_statements_and_keeps_attached_comments() {
        assert_eq!(
            texts("select 1 from dual; select 2 from dual;"),
            vec!["select 1 from dual", "select 2 from dual"]
        );
        assert_eq!(
            texts("-- employees\nselect * from emp;\n\n/* x */ update t set a = 1 where b = 2"),
            vec![
                "-- employees\nselect * from emp",
                "/* x */ update t set a = 1 where b = 2"
            ]
        );
        assert_eq!(
            texts("select ';' from dual; select 'a;b' from dual"),
            vec!["select ';' from dual", "select 'a;b' from dual"],
            "semicolons in strings"
        );
        assert_eq!(
            texts("select (1;2)"),
            vec!["select (1;2)"],
            "semicolon inside parentheses is not a terminator"
        );
    }

    #[test]
    fn plsql_blocks_end_at_slash_not_semicolon() {
        let s = "select 1 from dual;\nbegin\n  update t set a = 1;\n  commit;\nend;\n/\nselect 2 from dual;";
        let v = texts(s);
        assert_eq!(v.len(), 3, "{v:?}");
        assert!(v[1].starts_with("begin") && v[1].ends_with("end;"));
        // a division sign is not a terminator
        let v = texts("begin\n  x := 10 / 2;\nend;\n/");
        assert_eq!(v.len(), 1, "{v:?}");
        // stored unit without terminating slash runs to the end
        assert_eq!(
            texts("create or replace procedure p as begin null; end;").len(),
            1
        );
    }

    #[test]
    fn drops_empty_and_comment_only_fragments_and_reports_ranges() {
        assert!(split_statements("  ;; -- nothing\n ; /* c */").is_empty());
        let src = "select 1 from a;  select 2 from b";
        let v = split_statements(src);
        assert_eq!(v[1].text, "select 2 from b");
        assert_eq!(
            src.chars()
                .skip(v[1].start)
                .take(v[1].end - v[1].start)
                .collect::<String>(),
            "select 2 from b"
        );
        assert_eq!(v[0].kind, StatementKind::Select);
    }

    #[test]
    fn canonical_ignores_whitespace_case_comments_and_semicolons_but_not_literals() {
        let a = "SELECT  *\n  FROM emp -- hr\n WHERE id = 5;";
        let b = "select * from EMP where id = 5";
        assert_eq!(canonical(a), canonical(b));
        assert_eq!(sql_hash(a), sql_hash(b));
        assert_ne!(sql_hash(a), sql_hash("select * from emp where id = 6"));
        assert_ne!(
            sql_hash("select \"Emp\" from t"),
            sql_hash("select emp from t"),
            "quoted identifiers are case-sensitive"
        );
        assert_eq!(sql_hash(a).len(), 32);
    }

    #[test]
    fn fingerprint_groups_queries_that_differ_only_in_literals() {
        assert_eq!(
            fingerprint("select * from emp where id = 5 and name = 'Bob'"),
            fingerprint("SELECT * FROM emp WHERE id = 99 AND name = 'Al'")
        );
        assert_eq!(
            fingerprint("select * from t where a in (1, 2, 3)"),
            fingerprint("select * from t where a in (7)"),
            "IN lists collapse"
        );
        assert_ne!(
            fingerprint("select a from t"),
            fingerprint("select b from t")
        );
    }

    #[test]
    fn table_names_are_sorted_unique_and_schema_qualified() {
        assert_eq!(
            table_names("select * from hr.emp e join dept d on d.id = e.d join hr.emp x on 1=1"),
            "DEPT HR.EMP"
        );
        assert_eq!(table_names("select 1 from dual"), "");
    }

    #[test]
    fn redacts_passwords_everywhere_they_can_appear() {
        assert_eq!(
            redact_secrets("create user bob identified by Secret123 default tablespace users"),
            "create user bob identified by *** default tablespace users"
        );
        assert_eq!(
            redact_secrets("ALTER USER bob IDENTIFIED BY \"p@ss w0rd\" REPLACE \"old\""),
            "ALTER USER bob IDENTIFIED BY *** REPLACE ***"
        );
        assert_eq!(
            redact_secrets("create user u identified by values 'S:ABCDEF'"),
            "create user u identified by values ***"
        );
        assert_eq!(
            redact_secrets("create database link l connect to a identified by pw using 'x'"),
            "create database link l connect to a identified by *** using 'x'"
        );
        assert_eq!(
            redact_secrets("connect scott/tiger@orcl"),
            "connect scott/***@orcl"
        );
        // nothing to hide: unchanged, formatting preserved
        let plain = "select *\n  from emp   where a = 'identified by x'";
        assert_eq!(
            redact_secrets(plain),
            plain,
            "text inside a string literal is not a password clause"
        );
    }

    #[test]
    fn auto_titles() {
        assert_eq!(
            auto_title("select * from hr.employees where id = 1"),
            "SELECT HR.EMPLOYEES"
        );
        assert_eq!(
            auto_title("select * from a;\nupdate b set x=1 where y=2;\ndelete from c where z=1"),
            "SELECT A (+2 more)"
        );
        assert_eq!(auto_title("create table t (a number)"), "CREATE TABLE");
        assert_eq!(auto_title("begin null; end;"), "PL/SQL");
        assert_eq!(auto_title("  -- just a note\n"), "Untitled");
    }

    #[test]
    fn export_redaction_masks_secrets_even_in_comments_and_literals() {
        let t = "-- ALTER USER app IDENTIFIED BY hunter2;\nCREATE USER u identified   by \"Pa ss\" quota unlimited;\nv_password := 'abc123';\nx(api_key => 'k-999', n => 1);\nIDENTIFIED BY VALUES 'abc'\nSELECT 'password is fine' FROM dual;";
        let r = redact_for_export(t);
        for secret in ["hunter2", "Pa ss", "abc123", "k-999"] {
            assert!(!r.contains(secret), "{secret} leaked: {r}");
        }
        assert!(
            r.contains("quota unlimited")
                && r.contains("n => 1")
                && r.contains("'password is fine'"),
            "{r}"
        );
        assert_eq!(
            redact_for_export("SELECT 1 FROM dual"),
            "SELECT 1 FROM dual"
        );
    }
}
