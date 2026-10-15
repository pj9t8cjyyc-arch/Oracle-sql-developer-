//! Context-aware SQL completion over a [`MetaCache`]. A pure function of
//! (text, cursor, catalog), so it is fast and fully unit-testable.

use serde::Serialize;

use crate::lexer::{lex, utf16_to_char_index, Tok, Token};
use crate::meta::{FkEdge, MetaCache, TableMeta};
use crate::parse::{references_in, statement_range, TableRef};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Table,
    View,
    Column,
    Schema,
    Alias,
    Keyword,
    /// A ready-made `JOIN ... ON ...` derived from a foreign key.
    Join,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub kind: Kind,
    pub detail: Option<String>,
    pub score: i32,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Completions {
    /// UTF-16 offset where the replaced word starts (CodeMirror coordinates).
    pub from: usize,
    pub items: Vec<Suggestion>,
}

const MAX_ITEMS: usize = 60;

const STATEMENT_KW: &[&str] = &[
    "SELECT",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "MERGE INTO",
    "WITH",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "TRUNCATE TABLE",
    "COMMENT ON",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "EXPLAIN PLAN FOR",
];
const AFTER_TABLE_KW: &[&str] = &[
    "WHERE",
    "JOIN",
    "INNER JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "FULL JOIN",
    "CROSS JOIN",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "UNION",
    "UNION ALL",
    "FETCH FIRST",
    "ON",
    "FOR UPDATE",
];
const SELECT_KW: &[&str] = &[
    "DISTINCT", "FROM", "AS", "CASE", "COUNT(*)", "NVL", "COALESCE",
];
const PREDICATE_KW: &[&str] = &[
    "AND",
    "OR",
    "NOT",
    "IN",
    "LIKE",
    "BETWEEN",
    "IS NULL",
    "IS NOT NULL",
    "EXISTS",
    "ORDER BY",
    "GROUP BY",
];

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '#')
}

/// Prefix score: higher is better; `None` filters the candidate out.
fn match_score(cand: &str, prefix: &str) -> Option<i32> {
    if prefix.is_empty() {
        return Some(0);
    }
    let (c, p) = (cand.to_uppercase(), prefix.to_uppercase());
    if c.starts_with(&p) {
        Some(100 - (c.len() as i32 - p.len() as i32).min(50))
    } else if c.split(['_', ' ']).skip(1).any(|w| w.starts_with(&p)) {
        Some(60)
    } else if c.contains(&p) {
        Some(30)
    } else {
        None
    }
}

#[derive(Clone)]
struct Scope<'a> {
    /// Name used to qualify columns: alias if present, else table name.
    qualifier: String,
    owner: String,
    table: &'a TableMeta,
}

fn resolve<'a>(cache: &'a MetaCache, default_owner: &str, r: &TableRef) -> Option<Scope<'a>> {
    let owner = r
        .schema
        .clone()
        .unwrap_or_else(|| default_owner.to_uppercase());
    let table = cache.table(&owner, &r.name)?;
    Some(Scope {
        qualifier: r.alias.clone().unwrap_or_else(|| r.name.clone()),
        owner,
        table,
    })
}

fn tok_name(t: &Token) -> Option<(&str, bool)> {
    match &t.tok {
        Tok::Word(w) => Some((w, false)),
        Tok::Quoted(q) => Some((q, true)),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Clause {
    Tables(&'static str),
    Columns(&'static str),
}

/// Nearest clause keyword before the cursor, skipping closed parentheses.
fn clause_before(toks: &[&Token]) -> Option<(Clause, usize)> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().rev() {
        match &t.tok {
            Tok::Punct(')') => depth += 1,
            Tok::Punct('(') => depth = (depth - 1).max(0),
            Tok::Word(w) if depth == 0 => {
                let c = match w.as_str() {
                    "FROM" => Clause::Tables("FROM"),
                    "JOIN" => Clause::Tables("JOIN"),
                    "UPDATE" => Clause::Tables("UPDATE"),
                    "USING" => Clause::Tables("USING"),
                    "INTO" => Clause::Tables("INTO"),
                    "SELECT" => Clause::Columns("SELECT"),
                    "WHERE" => Clause::Columns("WHERE"),
                    "ON" => Clause::Columns("ON"),
                    "HAVING" => Clause::Columns("HAVING"),
                    "GROUP" => Clause::Columns("GROUP"),
                    "ORDER" => Clause::Columns("ORDER"),
                    "SET" => Clause::Columns("SET"),
                    _ => continue,
                };
                return Some((c, i));
            }
            _ => {}
        }
    }
    None
}

fn alias_for(table: &str, taken: &[String]) -> String {
    let base: String = table
        .split('_')
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_uppercase();
    let base = if base.is_empty() {
        "T".to_string()
    } else {
        base
    };
    let mut cand = base.clone();
    let mut n = 1;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&cand)) || cand.eq_ignore_ascii_case(table) {
        n += 1;
        cand = format!("{base}{n}");
    }
    cand
}

fn join_suggestions(
    cache: &MetaCache,
    owner: &str,
    scopes: &[Scope],
    prefix: &str,
    out: &mut Vec<Suggestion>,
) {
    let Some(schema) = cache.schemas.get(owner) else {
        return;
    };
    let mut taken: Vec<String> = scopes.iter().map(|s| s.qualifier.clone()).collect();
    let on = |a: &str, ac: &[String], b: &str, bc: &[String]| {
        ac.iter()
            .zip(bc)
            .map(|(x, y)| format!("{a}.{x} = {b}.{y}"))
            .collect::<Vec<_>>()
            .join(" AND ")
    };
    let mut push = |cand: &str, cols_on: &dyn Fn(&str) -> String, from: &FkEdge| {
        let Some(score) = match_score(cand, prefix) else {
            return;
        };
        let alias = alias_for(cand, &taken);
        let insert = format!("{cand} {alias} ON {}", cols_on(&alias));
        taken.push(alias);
        out.push(Suggestion {
            label: format!("{cand} (via {})", from.name),
            insert,
            kind: Kind::Join,
            detail: Some("join on foreign key".into()),
            score: 200 + score,
        });
    };
    for sc in scopes.iter().filter(|s| s.owner == owner) {
        // scope table is the child: join to its parent
        for fk in schema
            .fks
            .iter()
            .filter(|f| f.child_table == sc.table.name && f.parent_owner == owner)
        {
            push(
                &fk.parent_table,
                &|al| on(al, &fk.parent_cols, &sc.qualifier, &fk.child_cols),
                fk,
            );
        }
        // scope table is the parent: join to children
        for fk in schema
            .fks
            .iter()
            .filter(|f| f.parent_table == sc.table.name && f.parent_owner == owner)
        {
            push(
                &fk.child_table,
                &|al| on(al, &fk.child_cols, &sc.qualifier, &fk.parent_cols),
                fk,
            );
        }
    }
}

fn column_detail(t: &TableMeta, c: &crate::meta::ColMeta, fks: &[FkEdge]) -> String {
    let mut d = format!("{} · {}", c.data_type, t.name);
    if c.pk {
        d.push_str(" · PK");
    }
    if let Some(f) = fks
        .iter()
        .find(|f| f.child_table == t.name && f.child_cols.contains(&c.name))
    {
        d.push_str(&format!(" · FK→{}", f.parent_table));
    }
    d
}

fn add(
    items: &mut Vec<Suggestion>,
    label: &str,
    insert: &str,
    kind: Kind,
    detail: Option<String>,
    base: i32,
    prefix: &str,
) {
    if let Some(s) = match_score(label, prefix) {
        items.push(Suggestion {
            label: label.into(),
            insert: insert.into(),
            kind,
            detail,
            score: base + s,
        });
    }
}

fn add_tables(items: &mut Vec<Suggestion>, cache: &MetaCache, owner: &str, prefix: &str) {
    if let Some(sc) = cache.schemas.get(owner) {
        for t in sc.tables.values() {
            let kind = if t.is_view { Kind::View } else { Kind::Table };
            add(
                items,
                &t.name,
                &t.name,
                kind,
                t.comment
                    .clone()
                    .or_else(|| Some(format!("{} columns", t.columns.len()))),
                50,
                prefix,
            );
        }
    }
}

pub fn complete(
    cache: &MetaCache,
    default_owner: &str,
    text: &str,
    cursor_utf16: usize,
) -> Completions {
    let default_owner = default_owner.to_uppercase();
    let chars: Vec<char> = text.chars().collect();
    let cur = utf16_to_char_index(text, cursor_utf16).min(chars.len());
    let mut ws = cur;
    while ws > 0 && is_ident_char(chars[ws - 1]) {
        ws -= 1;
    }
    let prefix: String = chars[ws..cur].iter().collect();
    let from = chars[..ws].iter().map(|c| c.len_utf16()).sum();

    let all = lex(text);
    let (ss, se) = statement_range(&all, cur, chars.len());
    let in_stmt: Vec<&Token> = all
        .iter()
        .filter(|t| t.start >= ss && t.end <= se)
        .collect();
    let before: Vec<&Token> = in_stmt.iter().copied().filter(|t| t.end <= ws).collect();
    // References are computed with the word being typed removed so that a
    // half-typed alias is not mistaken for a real one.
    let refs_tokens: Vec<Token> = in_stmt
        .iter()
        .copied()
        .filter(|t| !(ws < cur && t.start == ws))
        .cloned()
        .collect();
    let refs = references_in(&refs_tokens);
    let scopes: Vec<Scope> = refs
        .iter()
        .filter_map(|r| resolve(cache, &default_owner, r))
        .collect();
    // If the cursor is inside `( SELECT ... )`, columns come from that
    // subquery's own tables; the outer query stays reachable via aliases.
    let inner_scopes: Vec<Scope> = {
        let mut depth = 0i32;
        let open = before.iter().rposition(|t| match t.tok {
            Tok::Punct(')') => {
                depth += 1;
                false
            }
            Tok::Punct('(') if depth > 0 => {
                depth -= 1;
                false
            }
            Tok::Punct('(') => true,
            _ => false,
        });
        match open.filter(
            |&k| matches!(before.get(k + 1).map(|t| &t.tok), Some(Tok::Word(w)) if w == "SELECT"),
        ) {
            Some(k) => {
                let start = before[k].end;
                let mut d = 0i32;
                let end = in_stmt
                    .iter()
                    .filter(|t| t.start >= ws)
                    .find(|t| match t.tok {
                        Tok::Punct('(') => {
                            d += 1;
                            false
                        }
                        Tok::Punct(')') if d > 0 => {
                            d -= 1;
                            false
                        }
                        Tok::Punct(')') => true,
                        _ => false,
                    });
                let end = end.map(|t| t.start).unwrap_or(se);
                let toks: Vec<Token> = refs_tokens
                    .iter()
                    .filter(|t| t.start >= start && t.end <= end)
                    .cloned()
                    .collect();
                references_in(&toks)
                    .iter()
                    .filter_map(|r| resolve(cache, &default_owner, r))
                    .collect()
            }
            None => Vec::new(),
        }
    };
    let fks: Vec<FkEdge> = cache
        .schemas
        .get(&default_owner)
        .map(|s| s.fks.clone())
        .unwrap_or_default();

    let mut items: Vec<Suggestion> = Vec::new();

    // 1. Dot context: `qualifier.|`
    let dot = before.len() >= 2 && matches!(before[before.len() - 1].tok, Tok::Punct('.'));
    if dot {
        let (q, quoted) = tok_name(before[before.len() - 2])
            .map(|(n, q)| (n.to_string(), q))
            .unwrap_or_default();
        let same = |a: &str| {
            if quoted {
                a == q
            } else {
                a.eq_ignore_ascii_case(&q)
            }
        };
        let scope = scopes
            .iter()
            .find(|s| s.qualifier != s.table.name && same(&s.qualifier))
            .or_else(|| scopes.iter().find(|s| same(&s.table.name)));
        let by_name = || {
            let owner = default_owner.clone();
            cache.table(&owner, &q.to_uppercase()).map(|t| (owner, t))
        };
        if let Some(sc) = scope {
            for c in &sc.table.columns {
                add(
                    &mut items,
                    &c.name,
                    &c.name,
                    Kind::Column,
                    Some(column_detail(sc.table, c, &fks)),
                    100,
                    &prefix,
                );
            }
        } else if let Some(sc) = cache.schemas.get(&q.to_uppercase()) {
            let owner = sc.owner.clone();
            add_tables(&mut items, cache, &owner, &prefix);
        } else if let Some((_, t)) = by_name() {
            for c in &t.columns {
                add(
                    &mut items,
                    &c.name,
                    &c.name,
                    Kind::Column,
                    Some(column_detail(t, c, &fks)),
                    100,
                    &prefix,
                );
            }
        }
        return finish(from, items);
    }

    // 2. Statement start.
    if before.is_empty() {
        for k in STATEMENT_KW {
            add(&mut items, k, k, Kind::Keyword, None, 0, &prefix);
        }
        return finish(from, items);
    }

    let clause = clause_before(&before);
    let last = before.last().unwrap();
    match clause {
        Some((Clause::Tables(kw), idx)) => {
            let after: &[&Token] = &before[idx + 1..];
            let has_paren = after.iter().any(|t| matches!(t.tok, Tok::Punct('(')));
            // `INSERT INTO t (` => column list of the target table
            if kw == "INTO" && has_paren {
                for sc in &scopes {
                    for c in &sc.table.columns {
                        add(
                            &mut items,
                            &c.name,
                            &c.name,
                            Kind::Column,
                            Some(column_detail(sc.table, c, &fks)),
                            100,
                            &prefix,
                        );
                    }
                }
                return finish(from, items);
            }
            let just_after_kw = std::ptr::eq(*last, before[idx]);
            let after_comma = matches!(last.tok, Tok::Punct(','));
            if just_after_kw || after_comma {
                if kw == "JOIN" {
                    join_suggestions(cache, &default_owner, &scopes, &prefix, &mut items);
                }
                add_tables(&mut items, cache, &default_owner, &prefix);
                for o in &cache.owners {
                    add(
                        &mut items,
                        o,
                        &format!("{o}."),
                        Kind::Schema,
                        None,
                        10,
                        &prefix,
                    );
                }
            } else {
                // a table (and maybe alias) has been written: suggest what follows
                let kws: &[&str] = if kw == "JOIN" {
                    &["ON", "USING"]
                } else {
                    AFTER_TABLE_KW
                };
                for k in kws {
                    add(&mut items, k, k, Kind::Keyword, None, 20, &prefix);
                }
            }
        }
        Some((Clause::Columns(kw), _)) => {
            let col_scopes = if inner_scopes.is_empty() {
                &scopes
            } else {
                &inner_scopes
            };
            let qualify = col_scopes.len() > 1;
            for sc in col_scopes {
                for c in &sc.table.columns {
                    let insert = if qualify {
                        format!("{}.{}", sc.qualifier, c.name)
                    } else {
                        c.name.clone()
                    };
                    add(
                        &mut items,
                        &c.name,
                        &insert,
                        Kind::Column,
                        Some(column_detail(sc.table, c, &fks)),
                        100,
                        &prefix,
                    );
                }
            }
            for sc in &scopes {
                let in_inner = inner_scopes.iter().any(|i| i.qualifier == sc.qualifier);
                if qualify || !prefix.is_empty() || (!inner_scopes.is_empty() && !in_inner) {
                    add(
                        &mut items,
                        &sc.qualifier,
                        &sc.qualifier,
                        Kind::Alias,
                        Some(format!("table {}", sc.table.name)),
                        40,
                        &prefix,
                    );
                }
            }
            if !prefix.is_empty() || scopes.is_empty() {
                let kws: &[&str] = if kw == "SELECT" {
                    SELECT_KW
                } else {
                    PREDICATE_KW
                };
                for k in kws {
                    add(&mut items, k, k, Kind::Keyword, None, 0, &prefix);
                }
            }
        }
        None => {
            for k in STATEMENT_KW.iter().chain(AFTER_TABLE_KW) {
                add(&mut items, k, k, Kind::Keyword, None, 0, &prefix);
            }
        }
    }
    finish(from, items)
}

fn finish(from: usize, mut items: Vec<Suggestion>) -> Completions {
    items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
    items.dedup_by(|a, b| a.insert == b.insert && a.kind == b.kind);
    items.truncate(MAX_ITEMS);
    Completions { from, items }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::fixtures::hr;

    /// `|` marks the cursor.
    fn run(sql: &str) -> Completions {
        let pos = sql.find('|').expect("cursor marker");
        let text = sql.replace('|', "");
        let utf16 = text[..pos].encode_utf16().count();
        complete(&hr(), "HR", &text, utf16)
    }
    fn labels(c: &Completions) -> Vec<String> {
        c.items.iter().map(|i| i.label.clone()).collect()
    }

    #[test]
    fn alias_dot_lists_that_tables_columns_even_before_from_is_typed_after_it() {
        let c = run("select e.| from emp e join dept d on d.id = e.dept_id");
        assert_eq!(labels(&c), vec!["DEPT_ID", "ID", "MGR_ID", "NAME"]);
        assert!(c.items.iter().all(|i| i.kind == Kind::Column));
        let id = c.items.iter().find(|i| i.label == "ID").unwrap();
        assert_eq!(id.detail.as_deref(), Some("NUMBER(6) · EMP · PK"));
        let dep = c.items.iter().find(|i| i.label == "DEPT_ID").unwrap();
        assert!(dep.detail.as_ref().unwrap().contains("FK→DEPT"));
    }

    #[test]
    fn dot_with_prefix_filters_and_reports_replace_start() {
        let c = run("select d.na| from emp e, dept d");
        assert_eq!(labels(&c), vec!["NAME"]);
        assert_eq!(c.from, "select d.".len());
    }

    #[test]
    fn table_name_and_schema_dot() {
        assert_eq!(labels(&run("select dept.| from dept")), vec!["ID", "NAME"]);
        let c = run("select * from hr.| ");
        assert!(
            labels(&c).contains(&"EMP".to_string())
                && c.items.iter().all(|i| i.kind != Kind::Column)
        );
    }

    #[test]
    fn from_clause_suggests_tables_views_and_schemas() {
        let c = run("select * from |");
        let l = labels(&c);
        assert!(
            l.contains(&"EMP".into())
                && l.contains(&"VW_EMP".into())
                && l.contains(&"SALES".into())
        );
        assert_eq!(
            c.items.iter().find(|i| i.label == "VW_EMP").unwrap().kind,
            Kind::View
        );
        assert_eq!(labels(&run("select * from em|"))[0], "EMP");
        // substring / word-start matches rank below prefix matches
        let l = labels(&run("select * from emp|"));
        assert_eq!(l[0], "EMP");
        assert!(l.contains(&"VW_EMP".to_string()));
    }

    #[test]
    fn join_suggests_fk_based_on_clauses() {
        let c = run("select * from emp e join |");
        let top = &c.items[0];
        assert_eq!(top.kind, Kind::Join);
        let inserts: Vec<_> = c
            .items
            .iter()
            .filter(|i| i.kind == Kind::Join)
            .map(|i| i.insert.as_str())
            .collect();
        assert!(
            inserts.contains(&"DEPT D ON D.ID = E.DEPT_ID"),
            "{inserts:?}"
        );
        // self-referencing manager FK: alias must not clash with E
        assert!(
            inserts
                .iter()
                .any(|s| s.starts_with("EMP E2 ON E2.ID = E.MGR_ID")),
            "{inserts:?}"
        );
    }

    #[test]
    fn join_from_the_parent_side_goes_to_children() {
        let c = run("select * from dept d join |");
        let inserts: Vec<_> = c
            .items
            .iter()
            .filter(|i| i.kind == Kind::Join)
            .map(|i| i.insert.clone())
            .collect();
        assert!(
            inserts.contains(&"EMP E ON E.DEPT_ID = D.ID".to_string()),
            "{inserts:?}"
        );
    }

    #[test]
    fn columns_in_where_are_qualified_only_when_ambiguous() {
        let one = run("select * from dept where na|");
        assert_eq!(one.items[0].insert, "NAME");
        let two = run("select * from emp e join dept d on d.id=e.dept_id where na|");
        let ins: Vec<_> = two
            .items
            .iter()
            .filter(|i| i.kind == Kind::Column)
            .map(|i| i.insert.as_str())
            .collect();
        assert!(
            ins.contains(&"E.NAME") && ins.contains(&"D.NAME"),
            "{ins:?}"
        );
    }

    #[test]
    fn select_list_resolves_tables_that_come_later() {
        let c = run("select | from dept");
        assert_eq!(labels(&c), vec!["ID", "NAME"]);
    }

    #[test]
    fn keywords_after_a_completed_table() {
        let c = run("select * from emp e |");
        assert!(
            labels(&c).contains(&"WHERE".to_string())
                && labels(&c).contains(&"LEFT JOIN".to_string())
        );
        assert!(c.items.iter().all(|i| i.kind == Kind::Keyword));
        assert_eq!(labels(&run("select * from emp e wh|"))[0], "WHERE");
    }

    #[test]
    fn statement_start_and_second_statement_are_independent() {
        assert!(labels(&run("|")).contains(&"SELECT".to_string()));
        let c = run("select * from emp; select | from dept");
        assert_eq!(
            labels(&c),
            vec!["ID", "NAME"],
            "scope is the current statement only"
        );
        assert!(labels(&run("select 1 from dual; |")).contains(&"UPDATE".to_string()));
    }

    #[test]
    fn insert_column_list_and_subquery_scope() {
        assert_eq!(labels(&run("insert into dept (|"))[0..2], ["ID", "NAME"]);
        let c = run("select * from emp where dept_id in (select | from dept)");
        let cols: Vec<_> = c
            .items
            .iter()
            .filter(|i| i.kind == Kind::Column)
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(cols, vec!["ID", "NAME"], "only the subquery's own columns");
        assert!(
            c.items
                .iter()
                .any(|i| i.kind == Kind::Alias && i.label == "EMP"),
            "outer table stays reachable"
        );
    }

    #[test]
    fn robust_on_unknown_tables_garbage_and_unicode() {
        assert!(run("select x.| from nosuch x").items.is_empty());
        for s in [
            "|",
            "|)))",
            "select ( | ",
            "select '|'",
            "select \"|",
            "select 😀 e.| from emp e",
            "select * from emp where (a in (|",
        ] {
            let _ = run(s);
        }
        // cursor offsets are UTF-16: the emoji counts as two units
        let c = run("select '😀', e.| from emp e");
        assert_eq!(c.from, "select '😀', e.".encode_utf16().count());
        assert!(!c.items.is_empty());
    }

    #[test]
    fn result_is_capped_and_deterministic() {
        let a = run("select * from |");
        let b = run("select * from |");
        assert_eq!(a, b);
        assert!(a.items.len() <= MAX_ITEMS);
    }
}
