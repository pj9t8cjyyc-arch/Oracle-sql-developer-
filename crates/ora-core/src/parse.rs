//! Lightweight, error-tolerant extraction of the tables a statement touches.
//! Works on incomplete SQL (as typed in an editor), which a strict parser
//! would reject. Known limits: a table following a parenthesised subquery in
//! the same comma-separated FROM list is not seen, and CTE names are
//! reported like tables (callers resolve against the catalog and ignore
//! unknown names).

use serde::Serialize;

use crate::lexer::{lex, Tok, Token};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

const NOT_ALIAS: &[&str] = &[
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "NATURAL",
    "OUTER",
    "ON",
    "USING",
    "SET",
    "UNION",
    "INTERSECT",
    "MINUS",
    "CONNECT",
    "START",
    "FETCH",
    "OFFSET",
    "FOR",
    "WHEN",
    "VALUES",
    "SELECT",
    "PARTITION",
    "SAMPLE",
    "RETURNING",
    "LATERAL",
    "PIVOT",
    "UNPIVOT",
    "MODEL",
    "WITH",
    "AS",
];

fn ident(t: &Token) -> Option<String> {
    match &t.tok {
        Tok::Word(w) => Some(w.clone()),
        Tok::Quoted(q) => Some(q.clone()),
        _ => None,
    }
}

fn is_word(t: Option<&Token>, w: &str) -> bool {
    matches!(t, Some(Token { tok: Tok::Word(x), .. }) if x == w)
}

fn is_punct(t: Option<&Token>, c: char) -> bool {
    matches!(t, Some(Token { tok: Tok::Punct(x), .. }) if *x == c)
}

/// Parse `[schema.]name [[AS] alias]` starting at `i`. Returns the new index.
fn parse_one(toks: &[Token], mut i: usize, out: &mut Vec<TableRef>) -> usize {
    let Some(first) = toks.get(i).and_then(ident) else {
        return i;
    };
    if matches!(toks[i].tok, Tok::Word(ref w) if NOT_ALIAS.contains(&w.as_str()) && w != "LATERAL")
    {
        return i;
    }
    i += 1;
    let (schema, name) = if is_punct(toks.get(i), '.') {
        match toks.get(i + 1).and_then(ident) {
            Some(n) => {
                i += 2;
                (Some(first), n)
            }
            None => return i + 1, // "schema." typed so far
        }
    } else {
        (None, first)
    };
    // db link: name@link
    if is_punct(toks.get(i), '@') {
        i += 2;
    }
    let mut alias = None;
    let as_kw = is_word(toks.get(i), "AS");
    if as_kw && is_word(toks.get(i + 1), "OF") {
        // flashback query: AS OF TIMESTAMP ...
    } else {
        let at = if as_kw { i + 1 } else { i };
        if let Some(t) = toks.get(at) {
            let reserved = matches!(&t.tok, Tok::Word(w) if NOT_ALIAS.contains(&w.as_str()));
            if let (Some(a), false) = (ident(t), reserved) {
                alias = Some(a);
                i = at + 1;
            }
        }
    }
    if name != "DUAL" {
        let r = TableRef {
            schema,
            name,
            alias,
        };
        if !out.contains(&r) {
            out.push(r);
        }
    }
    i
}

pub fn references(sql: &str) -> Vec<TableRef> {
    references_in(&lex(sql))
}

pub fn references_in(toks: &[Token]) -> Vec<TableRef> {
    let mut out = Vec::new();
    let mut parens: Vec<bool> = Vec::new(); // true: EXTRACT(.. FROM ..) / TRIM(.. FROM ..)
    let mut i = 0;
    while i < toks.len() {
        match &toks[i].tok {
            Tok::Punct('(') => {
                let func = i > 0
                    && matches!(&toks[i - 1].tok, Tok::Word(w) if w == "EXTRACT" || w == "TRIM");
                parens.push(func);
                i += 1;
            }
            Tok::Punct(')') => {
                parens.pop();
                i += 1;
            }
            Tok::Word(w) => {
                let prev = i.checked_sub(1).and_then(|p| toks.get(p));
                let next_i = i + 1;
                match w.as_str() {
                    "FROM" if !parens.last().copied().unwrap_or(false) => {
                        i = parse_one(toks, next_i, &mut out);
                        while is_punct(toks.get(i), ',') {
                            i = parse_one(toks, i + 1, &mut out);
                        }
                        i = i.max(next_i);
                        continue;
                    }
                    "JOIN" => {
                        i = parse_one(toks, next_i, &mut out).max(next_i);
                        continue;
                    }
                    "UPDATE" if !is_word(prev, "FOR") => {
                        i = parse_one(toks, next_i, &mut out).max(next_i);
                        continue;
                    }
                    "INTO" if is_word(prev, "INSERT") || is_word(prev, "MERGE") => {
                        i = parse_one(toks, next_i, &mut out).max(next_i);
                        continue;
                    }
                    "USING" if toks.get(next_i).is_some_and(|t| ident(t).is_some()) => {
                        i = parse_one(toks, next_i, &mut out).max(next_i);
                        continue;
                    }
                    _ => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Char range `[start, end)` of the statement containing `cursor`
/// (statements are separated by top-level `;`).
pub fn statement_range(toks: &[Token], cursor: usize, text_len: usize) -> (usize, usize) {
    let mut depth = 0i32;
    let mut start = 0;
    for t in toks {
        match t.tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            Tok::Punct(';') if depth <= 0 => {
                if t.end <= cursor {
                    start = t.end;
                } else {
                    return (start, t.start);
                }
            }
            _ => {}
        }
    }
    (start, text_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(sql: &str) -> Vec<(Option<&'static str>, &'static str, Option<&'static str>)> {
        // leak to get 'static for compact comparisons in tests
        references(sql)
            .into_iter()
            .map(|t| {
                (
                    t.schema.map(|s| &*Box::leak(s.into_boxed_str())),
                    &*Box::leak(t.name.into_boxed_str()),
                    t.alias.map(|s| &*Box::leak(s.into_boxed_str())),
                )
            })
            .collect()
    }

    #[test]
    fn from_with_and_without_alias() {
        assert_eq!(
            r("select * from emp e where e.id=1"),
            vec![(None, "EMP", Some("E"))]
        );
        assert_eq!(
            r("select * from hr.emp as e"),
            vec![(Some("HR"), "EMP", Some("E"))]
        );
        assert_eq!(r("select * from emp where 1=1"), vec![(None, "EMP", None)]);
    }

    #[test]
    fn comma_list_and_joins() {
        assert_eq!(
            r("select 1 from a x, b y, c where 1=1"),
            vec![
                (None, "A", Some("X")),
                (None, "B", Some("Y")),
                (None, "C", None)
            ]
        );
        assert_eq!(
            r("select * from emp e left outer join dept d on d.id=e.d join loc using (id)"),
            vec![
                (None, "EMP", Some("E")),
                (None, "DEPT", Some("D")),
                (None, "LOC", None)
            ]
        );
    }

    #[test]
    fn dml_targets() {
        assert_eq!(
            r("update emp e set sal=1 where e.id=2"),
            vec![(None, "EMP", Some("E"))]
        );
        assert_eq!(r("delete from emp where id=1"), vec![(None, "EMP", None)]);
        assert_eq!(
            r("insert into hr.emp (a,b) values (1,2)"),
            vec![(Some("HR"), "EMP", None)]
        );
        assert_eq!(
            r("merge into t using s on (t.id=s.id)"),
            vec![(None, "T", None), (None, "S", None)]
        );
    }

    #[test]
    fn false_positives_are_avoided() {
        assert_eq!(
            r("select extract(year from hire_date) from emp"),
            vec![(None, "EMP", None)]
        );
        assert_eq!(
            r("select trim(both 'x' from name) from emp"),
            vec![(None, "EMP", None)]
        );
        assert_eq!(r("select * from emp for update"), vec![(None, "EMP", None)]);
        assert_eq!(r("select sysdate from dual"), vec![]);
        assert_eq!(
            r("select x into v from emp"),
            vec![(None, "EMP", None)],
            "PL/SQL INTO variable is not a table"
        );
    }

    #[test]
    fn subqueries_and_quoted_and_flashback() {
        assert_eq!(
            r("select * from (select * from emp where id in (select id from dept)) x"),
            vec![(None, "EMP", None), (None, "DEPT", None)]
        );
        assert_eq!(
            r("select * from \"My Tab\" t"),
            vec![(None, "My Tab", Some("T"))]
        );
        assert_eq!(
            r("select * from emp as of timestamp sysdate"),
            vec![(None, "EMP", None)]
        );
    }

    #[test]
    fn incomplete_input_does_not_panic() {
        for s in [
            "select * from",
            "select * from hr.",
            "select * from a,",
            "update",
            "insert into",
            "select from where",
            "",
        ] {
            let _ = references(s);
        }
        assert_eq!(r("select * from hr."), vec![]);
    }

    #[test]
    fn statement_range_picks_the_statement_under_cursor() {
        let sql = "select 1 from a; select 2 from b; select 3 from c";
        let toks = lex(sql);
        let n = sql.chars().count();
        assert_eq!(statement_range(&toks, 3, n), (0, 15));
        assert_eq!(statement_range(&toks, 20, n), (16, 32));
        assert_eq!(statement_range(&toks, n, n), (33, n));
    }
}
