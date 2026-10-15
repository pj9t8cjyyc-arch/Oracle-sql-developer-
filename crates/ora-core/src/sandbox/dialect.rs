//! Oracle SQL to SQLite SQL for the sandbox. Only rewrites that are safe and exact are made; anything
//! that has no faithful SQLite equivalent is refused with a hint instead of being run differently.

use crate::guard::{is_block, is_exec};
use crate::lexer::{lex, Tok, Token};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Translation {
    pub sql: String,
    /// Rewrites that were applied, in plain words.
    pub changes: Vec<String>,
    /// Set when the statement cannot run in the sandbox; nothing is executed.
    pub refused: Option<String>,
}

fn word(t: &Token) -> Option<&str> {
    match &t.tok {
        Tok::Word(w) => Some(w.as_str()),
        _ => None,
    }
}

fn is_word(toks: &[Token], i: usize, w: &str) -> bool {
    toks.get(i).and_then(word) == Some(w)
}

fn is_punct(toks: &[Token], i: usize, c: char) -> bool {
    toks.get(i).is_some_and(|t| t.tok == Tok::Punct(c))
}

fn number_at(toks: &[Token], chars: &[char], i: usize) -> Option<String> {
    let t = toks.get(i)?;
    let s: String = chars[t.start..t.end].iter().collect();
    (t.tok == Tok::Other && !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())).then_some(s)
}

pub fn to_sqlite(sql: &str) -> Translation {
    let chars: Vec<char> = sql.chars().collect();
    let toks = lex(sql);
    let refuse = |why: &str| Translation {
        sql: sql.to_string(),
        changes: vec![],
        refused: Some(why.to_string()),
    };
    let plain: Vec<Tok> = toks.iter().map(|t| t.tok.clone()).collect();
    if is_block(&plain) || is_exec(&plain) || is_word(&toks, 0, "CALL") {
        return refuse(
            "PL/SQL cannot run in the sandbox (SQLite has no PL/SQL). Run it on the real database.",
        );
    }
    if is_word(&toks, 0, "MERGE") {
        return refuse("MERGE is not available in SQLite. Use INSERT ... ON CONFLICT DO UPDATE, or an UPDATE plus an INSERT.");
    }
    for (i, t) in toks.iter().enumerate() {
        match word(t) {
            Some("ROWNUM") => {
                return refuse(
                    "ROWNUM does not exist in SQLite. Use LIMIT n, or ROW_NUMBER() OVER (...).",
                )
            }
            Some("CONNECT") if is_word(&toks, i + 1, "BY") => {
                return refuse(
                    "CONNECT BY is not available in SQLite. Rewrite it as WITH RECURSIVE.",
                )
            }
            Some("NEXTVAL") | Some("CURRVAL") if i > 0 && is_punct(&toks, i - 1, '.') => {
                return refuse(
                    "Sequences do not exist in the sandbox. Insert explicit key values instead.",
                )
            }
            Some("LISTAGG") => {
                return refuse(
                    "LISTAGG is not available in SQLite. Use GROUP_CONCAT(expr, separator).",
                )
            }
            _ => {}
        }
        if t.tok == Tok::Punct('(') && is_punct(&toks, i + 1, '+') && is_punct(&toks, i + 2, ')') {
            return refuse("The (+) outer-join operator is not available in SQLite. Use LEFT JOIN / RIGHT JOIN.");
        }
    }

    // (start, end, replacement) in char indices; applied from the back.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut changes: Vec<String> = Vec::new();
    let mut skip_until = 0usize; // token index below which tokens are already part of an edit
    for i in 0..toks.len() {
        if i < skip_until {
            continue;
        }
        let t = &toks[i];
        match word(t) {
            Some("FROM") if is_word(&toks, i + 1, "DUAL") && !is_punct(&toks, i + 2, '.') => {
                edits.push((t.start, toks[i + 1].end, String::new()));
                changes.push("removed FROM DUAL".into());
                skip_until = i + 2;
            }
            Some("SYSDATE") | Some("SYSTIMESTAMP") | Some("LOCALTIMESTAMP") => {
                edits.push((t.start, t.end, "datetime('now')".into()));
                changes.push(format!("{} -> datetime('now')", word(t).unwrap()));
            }
            Some("MINUS") => {
                edits.push((t.start, t.end, "EXCEPT".into()));
                changes.push("MINUS -> EXCEPT".into());
            }
            Some("DATE") | Some("TIMESTAMP")
                if toks
                    .get(i + 1)
                    .is_some_and(|n| n.tok == Tok::Other && chars.get(n.start) == Some(&'\'')) =>
            {
                edits.push((t.start, toks[i + 1].start, String::new()));
                changes.push(format!(
                    "{} literal -> plain text literal",
                    word(t).unwrap()
                ));
            }
            Some("OFFSET") => {
                // OFFSET n ROWS [FETCH FIRST|NEXT m ROWS ONLY]
                if let (Some(n), true) = (
                    number_at(&toks, &chars, i + 1),
                    matches!(toks.get(i + 2).and_then(word), Some("ROW" | "ROWS")),
                ) {
                    if is_word(&toks, i + 3, "FETCH")
                        && matches!(toks.get(i + 4).and_then(word), Some("FIRST" | "NEXT"))
                    {
                        if let (Some(m), true, true) = (
                            number_at(&toks, &chars, i + 5),
                            matches!(toks.get(i + 6).and_then(word), Some("ROW" | "ROWS")),
                            is_word(&toks, i + 7, "ONLY"),
                        ) {
                            edits.push((t.start, toks[i + 7].end, format!("LIMIT {m} OFFSET {n}")));
                            changes.push("OFFSET/FETCH -> LIMIT/OFFSET".into());
                            skip_until = i + 8;
                        }
                    }
                }
            }
            Some("FETCH") if matches!(toks.get(i + 1).and_then(word), Some("FIRST" | "NEXT")) => {
                if let (Some(m), true, true) = (
                    number_at(&toks, &chars, i + 2),
                    matches!(toks.get(i + 3).and_then(word), Some("ROW" | "ROWS")),
                    is_word(&toks, i + 4, "ONLY"),
                ) {
                    edits.push((t.start, toks[i + 4].end, format!("LIMIT {m}")));
                    changes.push("FETCH FIRST n ROWS ONLY -> LIMIT n".into());
                    skip_until = i + 5;
                }
            }
            Some("FOR") if is_word(&toks, i + 1, "UPDATE") => {
                // Row locks mean nothing in a single-user file: drop the clause (and what follows it).
                let end = toks.last().map_or(t.end, |l| l.end);
                edits.push((t.start, end, String::new()));
                changes.push("removed FOR UPDATE".into());
                skip_until = toks.len();
            }
            _ => {}
        }
    }
    if edits.is_empty() {
        return Translation {
            sql: sql.trim().trim_end_matches(';').trim_end().to_string(),
            changes,
            refused: None,
        };
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = chars.clone();
    for (s, e, r) in edits {
        out.splice(s..e, r.chars());
    }
    let s: String = out.into_iter().collect();
    // Removing a clause can leave a trailing ';' or double spaces; tidy both.
    let s = s.trim().trim_end_matches(';').trim_end().to_string();
    changes.dedup();
    Translation {
        sql: s,
        changes,
        refused: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Translation {
        to_sqlite(s)
    }

    #[test]
    fn dual_sysdate_minus_and_date_literals_are_rewritten() {
        assert_eq!(t("select 1 from dual").sql, "select 1");
        let x = t("SELECT SYSDATE FROM dual;");
        assert_eq!(x.sql, "SELECT datetime('now')");
        assert_eq!(
            t("select a from x minus select a from y").sql,
            "select a from x EXCEPT select a from y"
        );
        assert_eq!(
            t("select * from o where d > DATE '2021-01-01'").sql,
            "select * from o where d > '2021-01-01'"
        );
        assert_eq!(
            t("select * from o where d > TIMESTAMP '2021-01-01 10:00:00'").sql,
            "select * from o where d > '2021-01-01 10:00:00'"
        );
        assert!(t("select 1 from dual")
            .changes
            .contains(&"removed FROM DUAL".to_string()));
    }

    #[test]
    fn row_limiting_clauses_become_limit() {
        assert_eq!(
            t("select * from e order by a fetch first 5 rows only").sql,
            "select * from e order by a LIMIT 5"
        );
        assert_eq!(
            t("select * from e order by a offset 10 rows fetch next 20 rows only").sql,
            "select * from e order by a LIMIT 20 OFFSET 10"
        );
        assert_eq!(
            t("select * from e fetch first 1 row only;").sql,
            "select * from e LIMIT 1"
        );
    }

    #[test]
    fn for_update_is_dropped_and_plain_sql_is_left_alone() {
        assert_eq!(
            t("select * from e where id = 1 for update nowait").sql,
            "select * from e where id = 1"
        );
        let x = t("select a, 'x;y' from \"T\" where b = 1;");
        assert_eq!(x.sql, "select a, 'x;y' from \"T\" where b = 1");
        assert!(x.changes.is_empty() && x.refused.is_none());
        // words inside strings and identifiers are untouched
        assert_eq!(
            t("select 'from dual', \"MINUS\" from t").sql,
            "select 'from dual', \"MINUS\" from t"
        );
    }

    #[test]
    fn things_sqlite_cannot_do_are_refused_with_a_hint() {
        for (sql, hint) in [
            ("select * from e where rownum <= 5", "ROWNUM"),
            (
                "select * from e connect by prior id = pid",
                "WITH RECURSIVE",
            ),
            ("select s.nextval from dual", "Sequences"),
            (
                "select listagg(a, ',') within group (order by a) from t",
                "GROUP_CONCAT",
            ),
            ("select * from a, b where a.x = b.x(+)", "LEFT JOIN"),
            (
                "merge into t using s on (t.a = s.a) when matched then update set t.b = s.b",
                "MERGE",
            ),
            ("begin null; end;", "PL/SQL"),
            ("exec p(1)", "PL/SQL"),
            ("call p(1)", "PL/SQL"),
        ] {
            let x = t(sql);
            assert!(
                x.refused.as_deref().is_some_and(|m| m.contains(hint)),
                "{sql}: {:?}",
                x.refused
            );
        }
    }
}
