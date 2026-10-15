//! What a connection actually reached: database version, container, user and whether PL/Scope is
//! on. Used by "Test connection" so a person can see *which* database they are about to use.

use serde::Serialize;

use crate::db::{collect, Database, Row};
use crate::Result;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ConnDetails {
    /// For example `Oracle Database 23ai Free Release 23.4.0.24.05 - Develop, Learn, and Run for Free`.
    pub banner: String,
    /// Major release number (19, 21, 23…), when it could be read from the banner.
    pub major: Option<u32>,
    pub db_name: String,
    /// The pluggable database, on a multitenant system.
    pub container: Option<String>,
    pub session_user: String,
    pub server_host: Option<String>,
    /// Value of the `plscope_settings` parameter, when this user may read it.
    pub plscope_settings: Option<String>,
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

/// First `a.b.c.d` style number in the text; its first part is the major release.
pub fn major_version(banner: &str) -> Option<u32> {
    let b: Vec<char> = banner.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                i += 1;
            }
            let token: String = b[start..i].iter().collect();
            if token.matches('.').count() >= 2 {
                return token.split('.').next().and_then(|m| m.parse().ok());
            }
        } else {
            i += 1;
        }
    }
    None
}

pub fn details(db: &dyn Database) -> Result<ConnDetails> {
    // BANNER_FULL (18c+) has the exact release; older databases only have BANNER.
    let banner = match collect(
        db,
        "SELECT /*ci:banner_full*/ banner_full FROM v$version WHERE ROWNUM = 1",
        &[],
    ) {
        Ok((_, rows)) if !rows.is_empty() => s(&rows[0], 0),
        _ => collect(
            db,
            "SELECT /*ci:banner*/ banner FROM v$version WHERE ROWNUM = 1",
            &[],
        )
        .map(|(_, rows)| rows.first().map(|r| s(r, 0)).unwrap_or_default())
        .unwrap_or_default(),
    };
    let (_, ctx) = collect(
        db,
        "SELECT /*ci:ctx*/ SYS_CONTEXT('USERENV','DB_NAME'), SYS_CONTEXT('USERENV','CON_NAME'), SYS_CONTEXT('USERENV','SESSION_USER'), SYS_CONTEXT('USERENV','SERVER_HOST') FROM dual",
        &[],
    )?;
    let c = ctx.first();
    let get = |i: usize| c.map(|r| s(r, i)).unwrap_or_default();
    let nonempty = |v: String| if v.is_empty() { None } else { Some(v) };
    let plscope = collect(
        db,
        "SELECT /*ci:plscope*/ value FROM v$parameter WHERE name = 'plscope_settings'",
        &[],
    )
    .ok()
    .and_then(|(_, rows)| rows.first().map(|r| s(r, 0)))
    .and_then(nonempty);
    Ok(ConnDetails {
        major: major_version(&banner),
        banner: banner.lines().next().unwrap_or("").trim().to_string(),
        db_name: get(0),
        container: nonempty(get(1)),
        session_user: get(2),
        server_host: nonempty(get(3)),
        plscope_settings: plscope,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn t(cols: &[&str], rows: Vec<Vec<Option<&str>>>) -> FakeTable {
        FakeTable::new(cols, rows)
    }

    #[test]
    fn major_release_comes_from_the_first_dotted_number() {
        assert_eq!(
            major_version(
                "Oracle Database 23ai Free Release 23.4.0.24.05 - Develop, Learn, and Run for Free"
            ),
            Some(23)
        );
        assert_eq!(
            major_version("Oracle Database 19c Enterprise Edition Release 19.0.0.0.0 - Production"),
            Some(19)
        );
        assert_eq!(
            major_version("Oracle Database 12c Release 12.1.0.2.0"),
            Some(12)
        );
        assert_eq!(
            major_version(
                "Oracle Database 11g Express Edition Release 11.2.0.2.0 - 64bit Production"
            ),
            Some(11)
        );
        assert_eq!(major_version("no version here 24"), None);
    }

    #[test]
    fn details_read_version_container_user_and_plscope() {
        let db = FakeDb::new()
            .with_table("ci:banner_full", t(&["b"], vec![vec![Some("Oracle Database 23ai Free Release 23.4.0.24.05 - Develop, Learn, and Run for Free\nVersion 23.4.0.24.05")]]))
            .with_table("ci:ctx", t(&["a", "b", "c", "d"], vec![vec![Some("FREE"), Some("FREEPDB1"), Some("HR"), Some("dbhost")]]))
            .with_table("ci:plscope", t(&["v"], vec![vec![Some("IDENTIFIERS:NONE")]]));
        let d = details(&db).unwrap();
        assert_eq!(
            (
                d.major,
                d.db_name.as_str(),
                d.container.as_deref(),
                d.session_user.as_str()
            ),
            (Some(23), "FREE", Some("FREEPDB1"), "HR")
        );
        assert!(
            d.banner.starts_with("Oracle Database 23ai") && !d.banner.contains("Version 23"),
            "{}",
            d.banner
        );
        assert_eq!(
            (d.server_host.as_deref(), d.plscope_settings.as_deref()),
            (Some("dbhost"), Some("IDENTIFIERS:NONE"))
        );
    }

    #[test]
    fn older_databases_and_restricted_users_still_connect_cleanly() {
        // No BANNER_FULL, no access to V$PARAMETER, no multitenant container.
        let db = FakeDb::new()
            .with_error("ci:banner_full", "ORA-00904: invalid identifier")
            .with_error("ci:plscope", "ORA-00942: table or view does not exist")
            .with_table(
                "ci:banner",
                t(
                    &["b"],
                    vec![vec![Some(
                        "Oracle Database 11g Express Edition Release 11.2.0.2.0 - 64bit Production",
                    )]],
                ),
            )
            .with_table(
                "ci:ctx",
                t(
                    &["a", "b", "c", "d"],
                    vec![vec![Some("XE"), None, Some("SCOTT"), None]],
                ),
            );
        let d = details(&db).unwrap();
        assert_eq!(
            (d.major, d.container, d.plscope_settings),
            (Some(11), None, None)
        );
        assert_eq!(d.session_user, "SCOTT");
    }
}
