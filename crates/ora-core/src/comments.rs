//! Saving table/column comments (`COMMENT ON ... IS '...'`).
//!
//! `COMMENT` is DDL: it cannot take bind variables and it commits. The SQL is
//! therefore built here with strict validation, and callers show it to the
//! user before running it.

use crate::db::{Database, Outcome};
use crate::{Error, Result};

/// Quote an identifier taken from the dictionary (exact case). Rejects
/// anything that could break out of the quotes.
pub fn quote_ident(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > 128 {
        return Err(Error::Invalid("identifier must be 1-128 characters".into()));
    }
    if name
        .chars()
        .any(|c| c == '"' || c == '\0' || c.is_control())
    {
        return Err(Error::Invalid(format!(
            "illegal character in identifier {name:?}"
        )));
    }
    Ok(format!("\"{name}\""))
}

/// Oracle string literal: single quotes doubled. Comments are limited to
/// 4000 bytes by Oracle.
fn quote_literal(text: &str) -> Result<String> {
    if text.contains('\0') {
        return Err(Error::Invalid("comment contains a NUL character".into()));
    }
    if text.len() > 4000 {
        return Err(Error::Invalid("comment longer than 4000 bytes".into()));
    }
    Ok(format!("'{}'", text.replace('\'', "''")))
}

/// `COMMENT ON TABLE`/`COLUMN` statement. An empty comment clears it (`IS ''`).
pub fn comment_sql(owner: &str, table: &str, column: Option<&str>, text: &str) -> Result<String> {
    let target = match column {
        Some(c) => format!(
            "COLUMN {}.{}.{}",
            quote_ident(owner)?,
            quote_ident(table)?,
            quote_ident(c)?
        ),
        None => format!("TABLE {}.{}", quote_ident(owner)?, quote_ident(table)?),
    };
    Ok(format!("COMMENT ON {target} IS {}", quote_literal(text)?))
}

/// Run the statement. Returns the SQL that was executed.
pub fn save_comment(
    db: &dyn Database,
    owner: &str,
    table: &str,
    column: Option<&str>,
    text: &str,
) -> Result<String> {
    let sql = comment_sql(owner, table, column, text)?;
    match db.run(&sql, &[], 1)? {
        Outcome::Done | Outcome::Affected(_) => Ok(sql),
        Outcome::Rows(_) => Err(Error::Database("unexpected result set".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeDb;

    #[test]
    fn builds_column_and_table_comments() {
        assert_eq!(
            comment_sql("HR", "EMP", Some("SAL"), "Monthly salary").unwrap(),
            r#"COMMENT ON COLUMN "HR"."EMP"."SAL" IS 'Monthly salary'"#
        );
        assert_eq!(
            comment_sql("HR", "EMP", None, "").unwrap(),
            r#"COMMENT ON TABLE "HR"."EMP" IS ''"#
        );
    }

    #[test]
    fn single_quotes_are_doubled_so_text_cannot_escape_the_literal() {
        let s = comment_sql("HR", "EMP", Some("A"), "it's' ; DROP TABLE emp; --").unwrap();
        assert_eq!(
            s,
            r#"COMMENT ON COLUMN "HR"."EMP"."A" IS 'it''s'' ; DROP TABLE emp; --'"#
        );
        // Walk the literal: after removing doubled quotes exactly the closing quote remains.
        let lit = s.split(" IS ").nth(1).unwrap();
        assert_eq!(lit.replace("''", "").matches('\'').count(), 2);
    }

    #[test]
    fn hostile_identifiers_are_rejected() {
        for bad in ["EMP\" ; DROP TABLE x; --", "A\0B", "line\nbreak", ""] {
            assert!(comment_sql("HR", bad, None, "x").is_err(), "{bad:?}");
            assert!(comment_sql(bad, "EMP", Some("A"), "x").is_err());
            assert!(comment_sql("HR", "EMP", Some(bad), "x").is_err());
        }
        assert!(comment_sql("HR", &"X".repeat(129), None, "x").is_err());
    }

    #[test]
    fn mixed_case_and_special_names_are_preserved_by_quoting() {
        assert_eq!(
            comment_sql("hr", "My Table", Some("a$b#c"), "x").unwrap(),
            r#"COMMENT ON COLUMN "hr"."My Table"."a$b#c" IS 'x'"#
        );
    }

    #[test]
    fn length_and_nul_limits() {
        assert!(comment_sql("HR", "T", None, &"x".repeat(4000)).is_ok());
        assert!(comment_sql("HR", "T", None, &"x".repeat(4001)).is_err());
        assert!(comment_sql("HR", "T", None, "a\0b").is_err());
        // multibyte: limit is bytes, as in Oracle
        assert!(comment_sql("HR", "T", None, &"é".repeat(2001)).is_err());
    }

    #[test]
    fn save_runs_exactly_the_built_statement() {
        let db = FakeDb::new().with_affected(0);
        let sql = save_comment(&db, "HR", "EMP", Some("SAL"), "x").unwrap();
        let log = db.log.lock().unwrap();
        assert_eq!(log.calls.len(), 1);
        assert_eq!(log.calls[0].0, sql);
        assert!(log.calls[0].1.is_empty());
    }
}
