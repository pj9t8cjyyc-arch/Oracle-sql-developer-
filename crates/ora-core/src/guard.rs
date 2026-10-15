//! Safe-mode analysis of SQL text. Pure lexical analysis (no DB round trip):
//! a small tokenizer that understands comments, quoted strings, q'[..]'
//! literals and quoted identifiers, so keywords inside them are ignored.

use serde::Serialize;

use crate::lexer::{lex, Tok};
use crate::profile::Environment;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StatementKind {
    Select,
    Insert,
    Update,
    Delete,
    Merge,
    Ddl,
    PlSql,
    Transaction,
    Other,
    Empty,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Warning {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Analysis {
    pub kind: StatementKind,
    /// SQL ready to send to the driver (trailing `;` removed, trimmed).
    pub sql: String,
    pub warnings: Vec<Warning>,
    /// The UI must ask the user before running this.
    pub requires_confirmation: bool,
    /// Refused outright; never executed even if confirmed.
    pub blocked: Option<String>,
    pub read_only: bool,
}

fn tokenize(src: &str) -> Vec<Tok> {
    lex(src).into_iter().map(|t| t.tok).collect()
}

fn is_word(t: &Tok, w: &str) -> bool {
    matches!(t, Tok::Word(x) if x == w)
}

fn first_word(toks: &[Tok]) -> Option<&str> {
    toks.iter().find_map(|t| {
        if let Tok::Word(w) = t {
            Some(w.as_str())
        } else {
            None
        }
    })
}

pub(crate) fn is_plsql(toks: &[Tok]) -> bool {
    match first_word(toks) {
        Some("BEGIN") | Some("DECLARE") | Some("CALL") | Some("EXEC") | Some("EXECUTE") => true,
        Some("CREATE") => {
            // CREATE [OR REPLACE] [EDITIONABLE|NONEDITIONABLE] <stored unit>
            toks.iter()
                .filter_map(|t| {
                    if let Tok::Word(w) = t {
                        Some(w.as_str())
                    } else {
                        None
                    }
                })
                .skip(1)
                .find(|w| !matches!(*w, "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE"))
                .is_some_and(|w| {
                    matches!(w, "FUNCTION" | "PROCEDURE" | "PACKAGE" | "TRIGGER" | "TYPE")
                })
        }
        _ => false,
    }
}

/// Number of top-level (outside parentheses) statements separated by `;`.
fn top_level_statements(toks: &[Tok]) -> usize {
    let mut depth = 0i32;
    let mut count = 0;
    let mut has_content = false;
    for t in toks {
        match t {
            Tok::Punct('(') => {
                depth += 1;
                has_content = true;
            }
            Tok::Punct(')') => depth -= 1,
            Tok::Punct(';') if depth == 0 => {
                if has_content {
                    count += 1;
                }
                has_content = false;
            }
            _ => has_content = true,
        }
    }
    count + usize::from(has_content)
}

fn has_top_level_word(toks: &[Tok], w: &str) -> bool {
    let mut depth = 0i32;
    for t in toks {
        match t {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            t if depth == 0 && is_word(t, w) => return true,
            _ => {}
        }
    }
    false
}

fn strip_trailing_semicolon(sql: &str) -> String {
    let t = sql.trim();
    let t = t.strip_suffix(';').unwrap_or(t);
    t.trim().to_string()
}

pub fn analyze(sql: &str, env: Environment) -> Analysis {
    let toks = tokenize(sql);
    let mut warnings = Vec::new();
    let mut warn = |severity, message: &str| {
        warnings.push(Warning {
            severity,
            message: message.to_string(),
        })
    };

    let plsql = is_plsql(&toks);
    let kind = match first_word(&toks) {
        None => StatementKind::Empty,
        _ if plsql => StatementKind::PlSql,
        Some("SELECT") | Some("WITH") => StatementKind::Select,
        Some("INSERT") => StatementKind::Insert,
        Some("UPDATE") => StatementKind::Update,
        Some("DELETE") => StatementKind::Delete,
        Some("MERGE") => StatementKind::Merge,
        Some("CREATE") | Some("ALTER") | Some("DROP") | Some("TRUNCATE") | Some("RENAME")
        | Some("COMMENT") | Some("GRANT") | Some("REVOKE") | Some("FLASHBACK") | Some("PURGE") => {
            StatementKind::Ddl
        }
        Some("COMMIT") | Some("ROLLBACK") | Some("SAVEPOINT") | Some("SET") => {
            StatementKind::Transaction
        }
        Some(_) => StatementKind::Other,
    };

    let blocked = (!plsql && top_level_statements(&toks) > 1)
        .then(|| "Multiple statements detected; run them one at a time.".to_string());

    match kind {
        StatementKind::Update | StatementKind::Delete if !has_top_level_word(&toks, "WHERE") => {
            warn(
                Severity::Danger,
                "No WHERE clause: this will affect every row in the table.",
            );
        }
        StatementKind::Ddl => {
            let w: Vec<&str> = toks
                .iter()
                .filter_map(|t| {
                    if let Tok::Word(w) = t {
                        Some(w.as_str())
                    } else {
                        None
                    }
                })
                .take(2)
                .collect();
            if matches!(w.first(), Some(&"DROP") | Some(&"TRUNCATE")) {
                warn(
                    Severity::Danger,
                    "DROP/TRUNCATE is permanent and cannot be rolled back.",
                );
            } else {
                warn(
                    Severity::Warning,
                    "DDL commits immediately and cannot be rolled back.",
                );
            }
        }
        StatementKind::Select
            if has_top_level_word(&toks, "FOR")
                && toks
                    .windows(2)
                    .any(|p| is_word(&p[0], "FOR") && is_word(&p[1], "UPDATE")) =>
        {
            warn(
                Severity::Warning,
                "SELECT ... FOR UPDATE locks the selected rows until commit/rollback.",
            );
        }
        StatementKind::PlSql => {
            warn(
                Severity::Warning,
                "PL/SQL may modify data or commit; its effects cannot be inferred statically.",
            );
        }
        _ => {}
    }

    let read_only =
        kind == StatementKind::Select && !warnings.iter().any(|w| w.message.contains("FOR UPDATE"));
    let changes_data = !matches!(
        kind,
        StatementKind::Select | StatementKind::Empty | StatementKind::Transaction
    );
    let worst = warnings.iter().map(|w| w.severity).max();
    let requires_confirmation =
        worst == Some(Severity::Danger) || (env == Environment::Prod && changes_data);
    if env == Environment::Prod && changes_data {
        warnings.push(Warning {
            severity: Severity::Warning,
            message: "You are connected to a PRODUCTION database.".into(),
        });
    }

    Analysis {
        kind,
        sql: if plsql {
            sql.trim().to_string()
        } else {
            strip_trailing_semicolon(sql)
        },
        warnings,
        requires_confirmation,
        blocked,
        read_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Analysis {
        analyze(s, Environment::Dev)
    }
    fn dangers(x: &Analysis) -> usize {
        x.warnings
            .iter()
            .filter(|w| w.severity == Severity::Danger)
            .count()
    }

    #[test]
    fn plain_select_is_clean_and_read_only() {
        let x = a("select * from emp;");
        assert_eq!(x.kind, StatementKind::Select);
        assert!(x.warnings.is_empty() && x.read_only && !x.requires_confirmation);
        assert_eq!(x.sql, "select * from emp");
    }

    #[test]
    fn delete_and_update_without_where_are_dangerous() {
        for s in [
            "delete from emp",
            "UPDATE emp SET sal = 1",
            "update emp set sal=(select max(sal) from emp where deptno=1)",
        ] {
            let x = a(s);
            assert_eq!(dangers(&x), 1, "{s}");
            assert!(x.requires_confirmation, "{s}");
        }
    }

    #[test]
    fn where_present_is_fine_and_subquery_where_does_not_count() {
        assert_eq!(dangers(&a("delete from emp where id = 1")), 0);
        // WHERE only inside the subquery -> outer statement still unbounded.
        assert_eq!(
            dangers(&a(
                "delete from emp e where exists (select 1 from d where d.id = e.id)"
            )),
            0
        );
        assert_eq!(
            dangers(&a("delete from (select * from emp where id = 1)")),
            1
        );
    }

    #[test]
    fn keywords_in_strings_and_comments_are_ignored() {
        assert_eq!(dangers(&a("delete from emp -- where id=1\n")), 1);
        assert_eq!(dangers(&a("delete from emp /* where x */")), 1);
        assert_eq!(dangers(&a("update t set note = 'where is it' ")), 1);
        assert_eq!(dangers(&a("update t set note = q'[it's where]'")), 1);
        assert_eq!(dangers(&a("update t set \"WHERE\" = 1")), 1);
        assert_eq!(
            a("select 'drop table x' from dual").kind,
            StatementKind::Select
        );
    }

    #[test]
    fn ddl_levels() {
        let d = a("drop table emp");
        assert_eq!((d.kind, dangers(&d)), (StatementKind::Ddl, 1));
        assert_eq!(dangers(&a("truncate table emp")), 1);
        let c = a("create index i on t(c)");
        assert_eq!(dangers(&c), 0);
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn multiple_statements_blocked_but_plsql_blocks_are_not() {
        assert!(a("select 1 from dual; select 2 from dual")
            .blocked
            .is_some());
        assert!(a("select 1 from dual;").blocked.is_none());
        assert!(a("begin null; end;").blocked.is_none());
        let p = a("begin update t set a=1; commit; end;");
        assert_eq!(p.kind, StatementKind::PlSql);
        assert_eq!(dangers(&p), 0);
        assert!(p.sql.ends_with("end;"), "PL/SQL keeps its terminator");
        let f = a("create or replace procedure p as begin null; end;");
        assert_eq!(f.kind, StatementKind::PlSql);
    }

    #[test]
    fn for_update_not_read_only() {
        let x = a("select * from emp where id=1 for update");
        assert!(!x.read_only);
        assert_eq!(x.warnings.len(), 1);
    }

    #[test]
    fn prod_requires_confirmation_for_changes_only() {
        assert!(analyze("insert into t values (1)", Environment::Prod).requires_confirmation);
        assert!(!analyze("select 1 from dual", Environment::Prod).requires_confirmation);
        assert!(!analyze("insert into t values (1)", Environment::Dev).requires_confirmation);
    }

    #[test]
    fn empty_and_comment_only() {
        assert_eq!(a("  -- nothing\n").kind, StatementKind::Empty);
    }
}
