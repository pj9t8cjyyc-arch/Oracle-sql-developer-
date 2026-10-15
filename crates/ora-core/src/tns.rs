//! Minimal `tnsnames.ora` reader: finds an alias and returns its full connect descriptor, so the
//! app does not depend on the Oracle client finding the file by itself.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TnsEntry {
    /// All names of the entry (`A, B = (DESCRIPTION=…)` defines two).
    pub names: Vec<String>,
    /// The descriptor on one line.
    pub descriptor: String,
    pub host: Option<String>,
    pub port: Option<String>,
    pub service: Option<String>,
    pub tls: bool,
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut quoted = false;
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                '#' if !quoted => break,
                _ => {}
            }
            out.push(c);
        }
        out.push('\n');
    }
    out
}

/// Text of `KEY=value` inside a descriptor (case-insensitive key), up to the closing parenthesis.
fn field(desc: &str, key: &str) -> Option<String> {
    let upper = desc.to_uppercase();
    let needle = format!("({key}");
    let mut from = 0;
    while let Some(p) = upper[from..].find(&needle) {
        let after = from + p + needle.len();
        let rest = desc[after..].trim_start();
        if let Some(v) = rest.strip_prefix('=') {
            let v = v.trim_start();
            let end = v.find(')').unwrap_or(v.len());
            return Some(v[..end].trim().to_string());
        }
        from = after;
    }
    None
}

/// Parses every entry of a `tnsnames.ora` text.
pub fn parse_tnsnames(text: &str) -> Vec<TnsEntry> {
    let text = strip_comments(text);
    let b: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        // Names up to '='.
        while i < b.len() && b[i].is_whitespace() {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i] != '=' && b[i] != '\n' && b[i] != '(' {
            i += 1;
        }
        if i >= b.len() || b[i] != '=' {
            // A line without `=` (or stray text): skip it.
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let names: Vec<String> = b[start..i]
            .iter()
            .collect::<String>()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        i += 1; // '='
        while i < b.len() && b[i].is_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != '(' {
            // `IFILE=path`, `SOMETHING=value`: not an entry.
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Balanced parentheses, ignoring those in quotes.
        let (mut depth, mut quoted, d0) = (0i32, false, i);
        while i < b.len() {
            match b[i] {
                '"' => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if depth != 0 || names.is_empty() {
            break; // unbalanced: stop rather than guess
        }
        let descriptor = b[d0..i]
            .iter()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push(TnsEntry {
            names,
            host: field(&descriptor, "HOST"),
            port: field(&descriptor, "PORT"),
            service: field(&descriptor, "SERVICE_NAME").or_else(|| field(&descriptor, "SID")),
            tls: field(&descriptor, "PROTOCOL").is_some_and(|p| p.eq_ignore_ascii_case("tcps")),
            descriptor,
        });
    }
    out
}

/// Case-insensitive; `ORCL.world` also matches `ORCL`.
pub fn find_alias<'a>(entries: &'a [TnsEntry], alias: &str) -> Option<&'a TnsEntry> {
    let want = alias.trim().to_lowercase();
    let base = |s: &str| s.split('.').next().unwrap_or(s).to_lowercase();
    entries
        .iter()
        .find(|e| e.names.iter().any(|n| n.to_lowercase() == want))
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.names.iter().any(|n| base(n) == base(&want)))
        })
}

/// Where Oracle looks for `tnsnames.ora`: the profile's folder, `TNS_ADMIN`, then `$ORACLE_HOME/network/admin`.
pub fn candidate_dirs(profile_dir: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if !profile_dir.trim().is_empty() {
        v.push(PathBuf::from(profile_dir.trim()));
    }
    if let Ok(d) = std::env::var("TNS_ADMIN") {
        if !d.is_empty() {
            v.push(PathBuf::from(d));
        }
    }
    if let Ok(h) = std::env::var("ORACLE_HOME") {
        if !h.is_empty() {
            v.push(Path::new(&h).join("network").join("admin"));
        }
    }
    v
}

/// Reads and parses the first `tnsnames.ora` found. The error lists every place that was tried.
pub fn load(profile_dir: &str) -> Result<(PathBuf, Vec<TnsEntry>)> {
    let dirs = candidate_dirs(profile_dir);
    for d in &dirs {
        let f = if d.is_file() {
            d.clone()
        } else {
            d.join("tnsnames.ora")
        };
        if let Ok(text) = std::fs::read_to_string(&f) {
            return Ok((f, parse_tnsnames(&text)));
        }
    }
    let tried = if dirs.is_empty() {
        "no folder is set and neither TNS_ADMIN nor ORACLE_HOME is defined".to_string()
    } else {
        dirs.iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Err(Error::Invalid(format!(
        "tnsnames.ora not found (looked in: {tried}). Choose the folder that contains it."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "
# production
PROD, PROD_RO =
  (DESCRIPTION =
    (ADDRESS = (PROTOCOL = TCP)(HOST = db-prod.corp)(PORT = 1521))
    (CONNECT_DATA = (SERVICE_NAME = prod.corp)))

DEV.world=(DESCRIPTION=(ADDRESS=(PROTOCOL=TCPS)(HOST=dev.corp)(PORT=2484))(CONNECT_DATA=(SERVICE_NAME=devpdb))) # trailing comment
IFILE=/etc/oracle/extra_tnsnames.ora
LEGACY =
  (DESCRIPTION=(ADDRESS_LIST=(ADDRESS=(PROTOCOL=TCP)(HOST=a)(PORT=1))(ADDRESS=(PROTOCOL=TCP)(HOST=b)(PORT=1)))
    (CONNECT_DATA=(SID=OLD)))
";

    #[test]
    fn entries_comments_multiple_names_and_nested_parentheses() {
        let e = parse_tnsnames(SAMPLE);
        assert_eq!(e.len(), 3, "IFILE is not an entry: {e:?}");
        assert_eq!(e[0].names, vec!["PROD", "PROD_RO"]);
        assert_eq!(
            (
                e[0].host.as_deref(),
                e[0].port.as_deref(),
                e[0].service.as_deref(),
                e[0].tls
            ),
            (Some("db-prod.corp"), Some("1521"), Some("prod.corp"), false)
        );
        assert!(e[0].descriptor.starts_with("(DESCRIPTION =") && !e[0].descriptor.contains('\n'));
        assert!(e[1].tls && e[1].port.as_deref() == Some("2484"));
        assert_eq!(
            e[2].service.as_deref(),
            Some("OLD"),
            "SID is used when there is no SERVICE_NAME"
        );
        assert!(e[2].descriptor.ends_with("(SID=OLD)))"));
    }

    #[test]
    fn aliases_match_case_insensitively_and_without_the_domain() {
        let e = parse_tnsnames(SAMPLE);
        assert!(find_alias(&e, "prod_ro").is_some());
        assert_eq!(find_alias(&e, "DEV").unwrap().names, vec!["DEV.world"]);
        assert!(find_alias(&e, "nope").is_none());
    }

    #[test]
    fn broken_files_do_not_panic_or_invent_entries() {
        assert!(parse_tnsnames("").is_empty());
        assert!(
            parse_tnsnames("X = (DESCRIPTION=(ADDRESS=(HOST=a)").is_empty(),
            "unbalanced"
        );
        assert!(parse_tnsnames("just some text\n=(\n").is_empty());
        assert_eq!(
            parse_tnsnames("A = (D=\")\")").len(),
            1,
            "a parenthesis inside quotes does not close the entry"
        );
    }

    #[test]
    fn loading_reports_every_place_it_looked() {
        let e = load("/definitely/not/here").unwrap_err().to_string();
        assert!(
            e.contains("tnsnames.ora not found") && e.contains("/definitely/not/here"),
            "{e}"
        );
        let dir = std::env::temp_dir().join(format!("ora-ide-tns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tnsnames.ora"), SAMPLE).unwrap();
        let (path, entries) = load(dir.to_str().unwrap()).unwrap();
        assert!(path.ends_with("tnsnames.ora") && entries.len() == 3);
        let (_, via_file) = load(dir.join("tnsnames.ora").to_str().unwrap()).unwrap();
        assert_eq!(via_file.len(), 3, "a path to the file itself works too");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
