//! Ranked full-text search over objects, columns, comments and PL/SQL source,
//! backed by an SQLite FTS5 index (local file or in-memory).
//!
//! Identifiers such as `EMP_DEPT_ID` are tokenised on `_`, so `dept` finds
//! it; every typed word is a prefix match and all words must match.

use std::path::Path;

use rusqlite::{params_from_iter, Connection};
use serde::Serialize;

use crate::db::Database;
use crate::meta::SchemaMeta;
use crate::{Error, Result};

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Database(format!("search index: {e}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    /// `table`, `view`, `column` or `source`.
    pub kind: &'static str,
    pub owner: String,
    pub name: String,
    /// Table of a column, or object type of a source line.
    pub parent: String,
    pub line: Option<u32>,
    /// Comment text or source line.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hit {
    pub kind: String,
    pub owner: String,
    pub name: String,
    pub parent: String,
    pub line: Option<u32>,
    /// Matching text with `[`..`]` around matched words.
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Default, Clone)]
pub struct SearchQuery {
    pub text: String,
    pub owner: Option<String>,
    pub kinds: Vec<String>,
    pub limit: usize,
}

pub struct SearchIndex {
    conn: Connection,
}

const SCHEMA: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS docs USING fts5(\
    name, parent, text, kind UNINDEXED, owner UNINDEXED, line UNINDEXED, \
    tokenize = 'unicode61 remove_diacritics 2');";

impl SearchIndex {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Atomically replace everything indexed for `owner`.
    pub fn replace_owner(&mut self, owner: &str, docs: &[Doc]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM docs WHERE owner = ?1", [owner])?;
        {
            let mut ins = tx.prepare("INSERT INTO docs(name, parent, text, kind, owner, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
            for d in docs {
                ins.execute(rusqlite::params![
                    d.name, d.parent, d.text, d.kind, d.owner, d.line
                ])?;
            }
        }
        tx.commit()?;
        Ok(docs.len())
    }

    pub fn count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM docs", [], |r| r.get::<_, i64>(0))? as u64)
    }

    pub fn search(&self, q: &SearchQuery) -> Result<Vec<Hit>> {
        let Some(fts) = fts_query(&q.text) else {
            return Ok(vec![]);
        };
        let mut sql = String::from(
            "SELECT kind, owner, name, parent, line, snippet(docs, 2, '[', ']', '…', 10), \
                    bm25(docs, 10.0, 3.0, 1.0) + CASE kind WHEN 'table' THEN 0.0 WHEN 'view' THEN 0.0 WHEN 'column' THEN 0.5 ELSE 1.0 END AS score \
             FROM docs WHERE docs MATCH ?1",
        );
        let mut args: Vec<String> = vec![fts];
        if let Some(o) = &q.owner {
            args.push(o.to_uppercase());
            sql.push_str(&format!(" AND owner = ?{}", args.len()));
        }
        if !q.kinds.is_empty() {
            let marks: Vec<String> = q
                .kinds
                .iter()
                .map(|k| {
                    args.push(k.to_lowercase());
                    format!("?{}", args.len())
                })
                .collect();
            sql.push_str(&format!(" AND kind IN ({})", marks.join(",")));
        }
        args.push(q.text.trim().to_string());
        // Exact (case-insensitive) name match first, then BM25 (lower is better).
        sql.push_str(&format!(
            " ORDER BY (name = ?{} COLLATE NOCASE) DESC, score ASC, name LIMIT {}",
            args.len(),
            q.limit.clamp(1, 200)
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok(Hit {
                kind: r.get(0)?,
                owner: r.get(1)?,
                name: r.get(2)?,
                parent: r.get(3)?,
                line: r.get::<_, Option<i64>>(4)?.map(|l| l as u32),
                snippet: r.get(5)?,
                score: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// `emp dep` -> `"emp"* AND "dep"*`. Everything except letters/digits is dropped,
/// so user input can never inject FTS5 operators.
pub(crate) fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" AND "))
}

pub fn docs_from_meta(m: &SchemaMeta) -> Vec<Doc> {
    let mut out = Vec::new();
    for t in m.tables.values() {
        out.push(Doc {
            kind: if t.is_view { "view" } else { "table" },
            owner: m.owner.clone(),
            name: t.name.clone(),
            parent: String::new(),
            line: None,
            text: t.comment.clone().unwrap_or_default(),
        });
        for c in &t.columns {
            out.push(Doc {
                kind: "column",
                owner: m.owner.clone(),
                name: c.name.clone(),
                parent: t.name.clone(),
                line: None,
                text: c.comment.clone().unwrap_or_default(),
            });
        }
    }
    out
}

const SQL_SOURCE: &str = "SELECT /*search:source*/ name, type, line, text FROM all_source WHERE owner = :1 \
     AND type IN ('PACKAGE','PACKAGE BODY','PROCEDURE','FUNCTION','TRIGGER','TYPE','TYPE BODY') ORDER BY name, type, line";

/// PL/SQL source lines (non-blank), capped so a huge schema cannot exhaust memory.
pub fn load_source_docs(db: &dyn Database, owner: &str, cap: usize) -> Result<Vec<Doc>> {
    use crate::db::Outcome;
    let mut out = Vec::new();
    if let Outcome::Rows(mut c) = db.run(SQL_SOURCE, &[owner.to_string()], 5000)? {
        'outer: loop {
            let batch = c.next_batch(5000)?;
            if batch.is_empty() {
                break;
            }
            for r in batch {
                let cell = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
                let text = cell(3);
                if text.trim().is_empty() {
                    continue;
                }
                out.push(Doc {
                    kind: "source",
                    owner: owner.to_string(),
                    name: cell(0),
                    parent: cell(1),
                    line: cell(2).parse().ok(),
                    text: text.trim_end().to_string(),
                });
                if out.len() >= cap {
                    break 'outer;
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::fixtures::hr;

    fn idx() -> SearchIndex {
        let mut i = SearchIndex::in_memory().expect("FTS5 must be available");
        let mut m = hr();
        {
            let hr_schema = m.schemas.get_mut("HR").unwrap();
            hr_schema.tables.get_mut("EMP").unwrap().comment = Some("Employee master data".into());
            hr_schema.tables.get_mut("EMP").unwrap().columns[2].comment =
                Some("Department the employee belongs to".into());
        }
        let mut docs = docs_from_meta(&m.schemas["HR"]);
        docs.push(Doc {
            kind: "source",
            owner: "HR".into(),
            name: "PKG_PAY".into(),
            parent: "PACKAGE BODY".into(),
            line: Some(42),
            text: "update emp set salary = salary * 1.1 where dept_id = p_dept;".into(),
        });
        i.replace_owner("HR", &docs).unwrap();
        i
    }

    fn q(i: &SearchIndex, t: &str) -> Vec<(String, String, String)> {
        i.search(&SearchQuery {
            text: t.into(),
            limit: 20,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|h| (h.kind, h.name, h.parent))
        .collect()
    }

    #[test]
    fn exact_name_ranks_first_and_prefix_matches_work() {
        let i = idx();
        let r = q(&i, "emp");
        assert_eq!(r[0], ("table".into(), "EMP".into(), "".into()), "{r:?}");
        assert!(
            r.iter().any(|x| x.1 == "VW_EMP"),
            "underscore-separated word prefix: {r:?}"
        );
        assert_eq!(q(&i, "dep")[0].1, "DEPT");
    }

    #[test]
    fn identifiers_split_on_underscore_and_words_are_anded() {
        let i = idx();
        let r = q(&i, "emp dept");
        assert!(
            r.contains(&("column".into(), "DEPT_ID".into(), "EMP".into())),
            "{r:?}"
        );
        assert!(q(&i, "id dept").iter().any(|x| x.1 == "DEPT_ID"));
        assert!(q(&i, "zzz").is_empty());
    }

    #[test]
    fn comments_and_source_are_searchable_with_snippets() {
        let i = idx();
        let h = i
            .search(&SearchQuery {
                text: "belongs".into(),
                limit: 5,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            (h[0].kind.as_str(), h[0].name.as_str(), h[0].parent.as_str()),
            ("column", "DEPT_ID", "EMP")
        );
        assert!(h[0].snippet.contains("[belongs]"), "{}", h[0].snippet);
        let s = i
            .search(&SearchQuery {
                text: "salary".into(),
                kinds: vec!["source".into()],
                limit: 5,
                ..Default::default()
            })
            .unwrap();
        assert_eq!((s[0].name.as_str(), s[0].line), ("PKG_PAY", Some(42)));
    }

    #[test]
    fn filters_by_owner_and_kind() {
        let mut i = idx();
        i.replace_owner(
            "SALES",
            &[Doc {
                kind: "table",
                owner: "SALES".into(),
                name: "EMP_TARGETS".into(),
                parent: String::new(),
                line: None,
                text: String::new(),
            }],
        )
        .unwrap();
        let all = q(&i, "emp");
        assert!(all.iter().any(|x| x.1 == "EMP_TARGETS"));
        let only_sales = i
            .search(&SearchQuery {
                text: "emp".into(),
                owner: Some("sales".into()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert!(only_sales.iter().all(|h| h.owner == "SALES"));
        let cols = i
            .search(&SearchQuery {
                text: "id".into(),
                kinds: vec!["column".into()],
                limit: 50,
                ..Default::default()
            })
            .unwrap();
        assert!(!cols.is_empty() && cols.iter().all(|h| h.kind == "column"));
    }

    #[test]
    fn reindexing_an_owner_replaces_not_duplicates() {
        let mut i = idx();
        let n = i.count().unwrap();
        let docs = docs_from_meta(&hr().schemas["HR"]);
        i.replace_owner("HR", &docs).unwrap();
        assert_eq!(i.count().unwrap(), docs.len() as u64);
        assert!(
            i.count().unwrap() < n,
            "source doc was dropped by the replace"
        );
    }

    #[test]
    fn hostile_input_cannot_break_or_inject_the_query() {
        let i = idx();
        for t in [
            "",
            "   ",
            "\"",
            "emp\" OR \"x",
            "emp*",
            "NEAR(a b)",
            "emp AND",
            "kind:table",
            "'; DROP TABLE docs; --",
            "name:emp -dept",
            "(((",
            "😀",
        ] {
            assert!(
                i.search(&SearchQuery {
                    text: t.into(),
                    limit: 5,
                    ..Default::default()
                })
                .is_ok(),
                "input {t:?}"
            );
        }
        assert!(i.count().unwrap() > 0);
        assert_eq!(
            fts_query("emp\" OR \"x").unwrap(),
            "\"emp\"* AND \"OR\"* AND \"x\"*"
        );
    }

    #[test]
    fn file_index_persists() {
        let dir = std::env::temp_dir().join(format!("ora-ide-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("i.sqlite");
        {
            let mut i = SearchIndex::open(&path).unwrap();
            i.replace_owner("HR", &docs_from_meta(&hr().schemas["HR"]))
                .unwrap();
        }
        let i = SearchIndex::open(&path).unwrap();
        assert!(!q(&i, "dept").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn source_loader_skips_blank_lines_and_respects_cap() {
        use crate::fake::{FakeDb, FakeTable};
        let rows: Vec<Vec<Option<&str>>> = vec![
            vec![
                Some("P"),
                Some("PROCEDURE"),
                Some("1"),
                Some("procedure p is\n"),
            ],
            vec![Some("P"), Some("PROCEDURE"), Some("2"), Some("   \n")],
            vec![
                Some("P"),
                Some("PROCEDURE"),
                Some("3"),
                Some("begin null; end;"),
            ],
            vec![Some("P"), Some("PROCEDURE"), Some("4"), Some("-- more")],
        ];
        let db =
            FakeDb::new().with_table("search:source", FakeTable::new(&["n", "t", "l", "x"], rows));
        let d = load_source_docs(&db, "HR", 100).unwrap();
        assert_eq!(
            d.iter().map(|x| x.line.unwrap()).collect::<Vec<_>>(),
            vec![1, 3, 4]
        );
        assert_eq!(d[0].text, "procedure p is");
        assert_eq!(load_source_docs(&db, "HR", 2).unwrap().len(), 2);
    }
}
