//! Profiling an import file: for every column infer its type, number style, date format
//! and size from the data. Ambiguity (is `03/04/2020` 3 April or 4 March? is `1.234`
//! one-point-two-three-four or one thousand?) is reported, never silently guessed.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};

use super::source::RowSource;
use crate::comments::quote_ident;
use crate::Result;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Empty,
    Integer,
    Decimal,
    Date,
    Timestamp,
    Boolean,
    Text,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct NumStyle {
    /// `.` or `,`.
    pub decimal: char,
    pub thousands: Option<char>,
}

impl NumStyle {
    pub fn dot() -> Self {
        Self {
            decimal: '.',
            thousands: Some(','),
        }
    }
    pub fn comma() -> Self {
        Self {
            decimal: ',',
            thousands: Some('.'),
        }
    }
    pub fn from_decimal(d: char) -> Self {
        if d == ',' {
            Self::comma()
        } else {
            Self::dot()
        }
    }
}

// ------------------------------------------------------------------------- numbers

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNum {
    pub neg: bool,
    pub int: String,
    pub frac: String,
    pub used_thousands: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NumErr {
    /// `007`: digits that should stay text (zip codes, ids).
    LeadingZero,
    Invalid,
}

pub fn parse_num(s: &str, decimal: char) -> std::result::Result<ParsedNum, NumErr> {
    let t = s.trim();
    let (neg, rest) = match t.chars().next() {
        Some('-') => (true, &t[1..]),
        Some('+') => (false, &t[1..]),
        _ => (false, t),
    };
    let thousands = if decimal == '.' { ',' } else { '.' };
    let (mant, exp) = if decimal == '.' {
        match rest.find(['e', 'E']) {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        }
    } else {
        (rest, None)
    };
    let (ip, fp) = match mant.split_once(decimal) {
        Some((a, b)) => (a, Some(b)),
        None => (mant, None),
    };
    if fp.is_some_and(|f| f.is_empty() || !f.chars().all(|c| c.is_ascii_digit())) {
        return Err(NumErr::Invalid);
    }
    let mut used_thousands = false;
    let int: String = if ip.contains(thousands) {
        let groups: Vec<&str> = ip.split(thousands).collect();
        let ok = !groups[0].is_empty()
            && groups[0].len() <= 3
            && groups.iter().all(|g| g.chars().all(|c| c.is_ascii_digit()))
            && groups[1..].iter().all(|g| g.len() == 3);
        if !ok {
            return Err(NumErr::Invalid);
        }
        used_thousands = true;
        groups.concat()
    } else if ip.chars().all(|c| c.is_ascii_digit()) {
        ip.to_string()
    } else {
        return Err(NumErr::Invalid);
    };
    if int.is_empty() && fp.is_none() {
        return Err(NumErr::Invalid);
    }
    if int.len() > 1 && int.starts_with('0') {
        return Err(NumErr::LeadingZero);
    }
    let mut int = if int.is_empty() { "0".to_string() } else { int };
    let mut frac = fp.unwrap_or("").to_string();
    if let Some(e) = exp {
        let e: i32 = e.parse().map_err(|_| NumErr::Invalid)?;
        if e.abs() > 38 {
            return Err(NumErr::Invalid);
        }
        let digits = format!("{int}{frac}");
        let point = int.len() as i32 + e;
        if point <= 0 {
            int = "0".into();
            frac = format!("{}{}", "0".repeat((-point) as usize), digits);
        } else if point as usize >= digits.len() {
            int = format!("{}{}", digits, "0".repeat(point as usize - digits.len()));
            frac = String::new();
        } else {
            int = digits[..point as usize].to_string();
            frac = digits[point as usize..].to_string();
        }
        let trimmed = int.trim_start_matches('0');
        int = if trimmed.is_empty() {
            "0".into()
        } else {
            trimmed.to_string()
        };
    }
    if int.len() + frac.len() > 38 {
        return Err(NumErr::Invalid);
    }
    Ok(ParsedNum {
        neg,
        int,
        frac,
        used_thousands,
    })
}

/// `-1234.5` (no separators, `.` as decimal): what Oracle's `TO_NUMBER(.., 'TM9', 'NLS_NUMERIC_CHARACTERS=''.,''')` takes.
pub fn canonical_num(p: &ParsedNum) -> String {
    let zero = p.int == "0" && p.frac.chars().all(|c| c == '0');
    format!(
        "{}{}{}{}",
        if p.neg && !zero { "-" } else { "" },
        p.int,
        if p.frac.is_empty() { "" } else { "." },
        p.frac
    )
}

// --------------------------------------------------------------------------- dates

pub struct Family {
    pub name: &'static str,
    date_fmt: &'static str,
    iso: bool,
}

pub const FAMILIES: [Family; 8] = [
    Family {
        name: "YYYY-MM-DD",
        date_fmt: "%Y-%m-%d",
        iso: true,
    },
    Family {
        name: "YYYY/MM/DD",
        date_fmt: "%Y/%m/%d",
        iso: false,
    },
    Family {
        name: "DD/MM/YYYY",
        date_fmt: "%d/%m/%Y",
        iso: false,
    },
    Family {
        name: "MM/DD/YYYY",
        date_fmt: "%m/%d/%Y",
        iso: false,
    },
    Family {
        name: "DD-MM-YYYY",
        date_fmt: "%d-%m-%Y",
        iso: false,
    },
    Family {
        name: "DD.MM.YYYY",
        date_fmt: "%d.%m.%Y",
        iso: false,
    },
    Family {
        name: "DD-MON-YYYY",
        date_fmt: "%d-%b-%Y",
        iso: false,
    },
    Family {
        name: "DD-MON-YY",
        date_fmt: "%d-%b-%y",
        iso: false,
    },
];

pub fn family(name: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.name == name)
}

/// A parsed date/time and how many fractional-second digits the text had.
pub fn parse_date(v: &str, f: &Family) -> Option<(NaiveDateTime, usize)> {
    let v = v.trim();
    if v.len() < 6 || v.len() > 40 || !v.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let midnight = NaiveTime::MIN;
    if let Ok(d) = NaiveDate::parse_from_str(v, f.date_fmt) {
        return Some((d.and_time(midnight), 0));
    }
    let mut suffixes: Vec<String> = vec![
        " %H:%M:%S%.f".into(),
        " %H:%M:%S".into(),
        " %H:%M".into(),
        " %I:%M:%S %p".into(),
        " %I:%M %p".into(),
    ];
    if f.iso {
        suffixes.insert(0, "T%H:%M:%S%.f".into());
        suffixes.insert(1, "T%H:%M:%S".into());
    }
    for s in suffixes {
        if let Ok(dt) = NaiveDateTime::parse_from_str(v, &format!("{}{}", f.date_fmt, s)) {
            if dt
                .format("%Y")
                .to_string()
                .parse::<i32>()
                .is_ok_and(|y| (1..=9999).contains(&y))
            {
                let frac = v
                    .rfind(':')
                    .and_then(|i| {
                        v[i..].find('.').map(|j| {
                            v[i + j + 1..]
                                .chars()
                                .take_while(|c| c.is_ascii_digit())
                                .count()
                        })
                    })
                    .unwrap_or(0);
                return Some((dt, frac));
            }
        }
    }
    None
}

/// `YYYY-MM-DD HH24:MI:SS` (and `.ffffff` for timestamps), what the INSERT binds expect.
pub fn canonical_date(dt: &NaiveDateTime, timestamp: bool) -> String {
    if timestamp {
        dt.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    }
}

// ------------------------------------------------------------------------ profiling

#[derive(Debug, Clone, Serialize)]
pub struct ColProfile {
    pub index: usize,
    pub name: String,
    pub kind: Kind,
    pub nulls: u64,
    pub non_null: u64,
    pub max_chars: usize,
    pub max_bytes: usize,
    /// Digits left / right of the decimal point (numbers).
    pub int_digits: usize,
    pub scale: usize,
    /// Date formats every value matches, in order of preference. More than one means ambiguity.
    pub date_formats: Vec<String>,
    pub date_ambiguous: bool,
    pub number_style: Option<NumStyle>,
    pub number_ambiguous: bool,
    /// Values like `007`: kept as text so the zeros survive.
    pub leading_zeros: bool,
    pub samples: Vec<String>,
    /// For columns that are only mostly numbers or dates: among the first `hit_sample` values,
    /// how many parse as numbers with `.` / `,` as the decimal mark, and as each date format.
    pub hit_sample: u64,
    pub dot_hits: u64,
    pub comma_hits: u64,
    pub date_hits: Vec<(String, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileProfile {
    pub columns: Vec<ColProfile>,
    pub rows_scanned: u64,
    /// The file has more rows than were scanned for this profile.
    pub more_rows: bool,
    pub ragged_rows: u64,
    pub format: String,
    pub delimiter: Option<char>,
    pub has_header: bool,
    pub encoding: String,
    pub sheets: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Default, Clone)]
struct NumAcc {
    ok: bool,
    any_sep: bool,
    used_thousands: bool,
    int_digits: usize,
    scale: usize,
}

struct Acc {
    non_null: u64,
    nulls: u64,
    max_chars: usize,
    max_bytes: usize,
    samples: Vec<String>,
    dot: NumAcc,
    comma: NumAcc,
    leading_zero: bool,
    fams: [bool; 8],
    fam_frac: [bool; 8],
    bool_ok: bool,
    bool_alpha: bool,
    hit_n: u64,
    dot_hits: u64,
    comma_hits: u64,
    fam_hits: [u64; 8],
}

/// Values examined for the per-format hit counts (kept small: it tries every date format).
const HIT_SAMPLE: u64 = 2000;

impl Acc {
    fn new() -> Self {
        Self {
            non_null: 0,
            nulls: 0,
            max_chars: 0,
            max_bytes: 0,
            samples: vec![],
            dot: NumAcc {
                ok: true,
                ..Default::default()
            },
            comma: NumAcc {
                ok: true,
                ..Default::default()
            },
            leading_zero: false,
            fams: [true; 8],
            fam_frac: [false; 8],
            bool_ok: true,
            bool_alpha: false,
            hit_n: 0,
            dot_hits: 0,
            comma_hits: 0,
            fam_hits: [0; 8],
        }
    }

    fn add(&mut self, raw: &str) {
        let v = raw.trim();
        if v.is_empty() {
            self.nulls += 1;
            return;
        }
        self.non_null += 1;
        self.max_chars = self.max_chars.max(raw.chars().count());
        self.max_bytes = self.max_bytes.max(raw.len());
        if self.samples.len() < 5 && !self.samples.iter().any(|s| s == v) {
            self.samples.push(v.chars().take(60).collect());
        }
        if self.hit_n < HIT_SAMPLE {
            self.hit_n += 1;
            let hit = |r: std::result::Result<ParsedNum, NumErr>| {
                matches!(r, Ok(_) | Err(NumErr::LeadingZero))
            };
            self.dot_hits += hit(parse_num(v, '.')) as u64;
            self.comma_hits += hit(parse_num(v, ',')) as u64;
            if v.chars().any(|c| matches!(c, '-' | '/' | '.')) {
                for (i, f) in FAMILIES.iter().enumerate() {
                    self.fam_hits[i] += parse_date(v, f).is_some() as u64;
                }
            }
        }
        for (acc, dec) in [(&mut self.dot, '.'), (&mut self.comma, ',')] {
            if !acc.ok {
                continue;
            }
            match parse_num(v, dec) {
                Ok(p) => {
                    acc.int_digits = acc.int_digits.max(p.int.len());
                    acc.scale = acc.scale.max(p.frac.len());
                    acc.used_thousands |= p.used_thousands;
                    acc.any_sep |= p.used_thousands || v.contains(dec);
                }
                Err(NumErr::LeadingZero) => {
                    acc.ok = false;
                    self.leading_zero = true;
                }
                Err(NumErr::Invalid) => acc.ok = false,
            }
        }
        if self.bool_ok {
            let l = v.to_lowercase();
            if matches!(
                l.as_str(),
                "true" | "false" | "yes" | "no" | "y" | "n" | "t" | "f"
            ) {
                self.bool_alpha = true;
            } else if !matches!(l.as_str(), "1" | "0") {
                self.bool_ok = false;
            }
        }
        if self.fams.iter().any(|x| *x) && v.chars().any(|c| matches!(c, '-' | '/' | '.')) {
            for (i, f) in FAMILIES.iter().enumerate() {
                if !self.fams[i] {
                    continue;
                }
                match parse_date(v, f) {
                    Some((_, frac)) => self.fam_frac[i] |= frac > 0,
                    None => self.fams[i] = false,
                }
            }
        } else {
            self.fams = [false; 8];
        }
    }

    fn finish(self, index: usize, name: String) -> ColProfile {
        let mut p = ColProfile {
            index,
            name,
            kind: Kind::Text,
            nulls: self.nulls,
            non_null: self.non_null,
            max_chars: self.max_chars,
            max_bytes: self.max_bytes,
            int_digits: 0,
            scale: 0,
            date_formats: vec![],
            date_ambiguous: false,
            number_style: None,
            number_ambiguous: false,
            leading_zeros: self.leading_zero,
            samples: self.samples,
            hit_sample: self.hit_n,
            dot_hits: self.dot_hits,
            comma_hits: self.comma_hits,
            date_hits: {
                let mut h: Vec<(String, u64)> = (0..8)
                    .filter(|i| self.fam_hits[*i] > 0)
                    .map(|i| (FAMILIES[i].name.to_string(), self.fam_hits[i]))
                    .collect();
                h.sort_by_key(|x| std::cmp::Reverse(x.1));
                h
            },
        };
        if self.non_null == 0 {
            p.kind = Kind::Empty;
        } else if self.leading_zero {
            p.kind = Kind::Text;
        } else if self.dot.ok || self.comma.ok {
            let ambiguous =
                self.dot.ok && self.comma.ok && (self.dot.any_sep || self.comma.any_sep);
            let (acc, dec) = if self.dot.ok {
                (&self.dot, '.')
            } else {
                (&self.comma, ',')
            };
            p.number_ambiguous = ambiguous;
            p.number_style = Some(NumStyle {
                decimal: dec,
                thousands: acc
                    .used_thousands
                    .then_some(if dec == '.' { ',' } else { '.' }),
            });
            p.int_digits = acc.int_digits;
            p.scale = acc.scale;
            p.kind = if acc.scale == 0 {
                Kind::Integer
            } else {
                Kind::Decimal
            };
        } else if self.bool_ok && self.bool_alpha {
            p.kind = Kind::Boolean;
        } else {
            let ok: Vec<usize> = (0..8).filter(|i| self.fams[*i]).collect();
            if !ok.is_empty() {
                p.date_formats = ok.iter().map(|i| FAMILIES[*i].name.to_string()).collect();
                p.date_ambiguous = ok.len() > 1;
                p.kind = if self.fam_frac[ok[0]] {
                    Kind::Timestamp
                } else {
                    Kind::Date
                };
            }
        }
        p
    }
}

/// Scan up to `sample_rows` rows (all of them if the file is shorter).
pub fn profile_source(src: &mut dyn RowSource, sample_rows: u64) -> Result<FileProfile> {
    let headers = src.headers().to_vec();
    let mut accs: Vec<Acc> = headers.iter().map(|_| Acc::new()).collect();
    let (mut rows, mut ragged) = (0u64, 0u64);
    let mut more = false;
    while let Some(r) = src.next_row()? {
        if rows >= sample_rows {
            more = true;
            break;
        }
        rows += 1;
        if r.len() != headers.len() {
            ragged += 1;
        }
        for (i, a) in accs.iter_mut().enumerate() {
            a.add(r.get(i).map(String::as_str).unwrap_or(""));
        }
    }
    let info = src.info().clone();
    let columns: Vec<ColProfile> = accs
        .into_iter()
        .zip(headers)
        .enumerate()
        .map(|(i, (a, h))| a.finish(i, h))
        .collect();
    let mut warnings = Vec::new();
    if ragged > 0 {
        warnings.push(format!("{ragged} row(s) have a different number of fields than the header; they will be rejected."));
    }
    if columns.iter().any(|c| c.date_ambiguous) {
        warnings.push("Some date columns could be day-first or month-first. Check the format chosen for each.".into());
    }
    if columns.iter().any(|c| c.number_ambiguous) {
        warnings.push("Some numbers could use either '.' or ',' as the decimal mark. Check the style chosen for each.".into());
    }
    if info.format == "xlsx" && rows > 500_000 {
        warnings.push(
            "Excel files are read into memory; for files this large, export to CSV first.".into(),
        );
    }
    Ok(FileProfile {
        columns,
        rows_scanned: rows,
        more_rows: more,
        ragged_rows: ragged,
        format: info.format,
        delimiter: info.delimiter,
        has_header: info.has_header,
        encoding: info.encoding,
        sheets: info.sheets,
        warnings,
    })
}

// -------------------------------------------------------------------------- create table

const RESERVED: &[&str] = &[
    "ACCESS",
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "AUDIT",
    "BETWEEN",
    "BY",
    "CHAR",
    "CHECK",
    "CLUSTER",
    "COLUMN",
    "COMMENT",
    "COMPRESS",
    "CONNECT",
    "CREATE",
    "CURRENT",
    "DATE",
    "DECIMAL",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "EXCLUSIVE",
    "EXISTS",
    "FILE",
    "FLOAT",
    "FOR",
    "FROM",
    "GRANT",
    "GROUP",
    "HAVING",
    "IDENTIFIED",
    "IMMEDIATE",
    "IN",
    "INCREMENT",
    "INDEX",
    "INITIAL",
    "INSERT",
    "INTEGER",
    "INTERSECT",
    "INTO",
    "IS",
    "LEVEL",
    "LIKE",
    "LOCK",
    "LONG",
    "MAXEXTENTS",
    "MINUS",
    "MODE",
    "MODIFY",
    "NOAUDIT",
    "NOCOMPRESS",
    "NOT",
    "NOWAIT",
    "NULL",
    "NUMBER",
    "OF",
    "OFFLINE",
    "ON",
    "ONLINE",
    "OPTION",
    "OR",
    "ORDER",
    "PCTFREE",
    "PRIOR",
    "PUBLIC",
    "RAW",
    "RENAME",
    "RESOURCE",
    "REVOKE",
    "ROW",
    "ROWID",
    "ROWNUM",
    "ROWS",
    "SELECT",
    "SESSION",
    "SET",
    "SHARE",
    "SIZE",
    "SMALLINT",
    "START",
    "SUCCESSFUL",
    "SYNONYM",
    "SYSDATE",
    "TABLE",
    "THEN",
    "TO",
    "TRIGGER",
    "UID",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USER",
    "VALIDATE",
    "VALUES",
    "VARCHAR",
    "VARCHAR2",
    "VIEW",
    "WHENEVER",
    "WHERE",
    "WITH",
];

/// An unquoted-safe upper-case Oracle identifier from a file header.
pub fn oracle_ident(name: &str) -> String {
    let mut s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '$' | '#') {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    s = s.trim_matches('_').to_string();
    if s.is_empty() {
        s = "COL".into();
    }
    if !s.chars().next().unwrap().is_ascii_alphabetic() {
        s = format!("C_{s}");
    }
    if RESERVED.contains(&s.as_str()) {
        s.push('_');
    }
    s.chars().take(128).collect()
}

fn text_len(max_chars: usize) -> usize {
    // headroom for values longer than the sample: ~1.5x, rounded up to a friendly size
    let want = (max_chars as f64 * 1.5).ceil() as usize;
    [16usize, 32, 50, 100, 255, 500, 1000, 2000, 4000]
        .into_iter()
        .find(|n| *n >= want)
        .unwrap_or(4000)
}

/// Suggested Oracle column type for a profiled column.
pub fn suggest_type(c: &ColProfile) -> String {
    match c.kind {
        Kind::Integer if c.int_digits >= 1 && c.int_digits <= 38 => {
            format!("NUMBER({})", c.int_digits.max(1))
        }
        Kind::Integer => "NUMBER".into(),
        Kind::Decimal => format!(
            "NUMBER({},{})",
            (c.int_digits + c.scale).clamp(1, 38),
            c.scale.min(37)
        ),
        Kind::Date => "DATE".into(),
        Kind::Timestamp => "TIMESTAMP(6)".into(),
        Kind::Boolean => "NUMBER(1)".into(),
        Kind::Empty => "VARCHAR2(100 CHAR)".into(),
        Kind::Text if c.max_bytes > 4000 => "CLOB".into(),
        Kind::Text => format!("VARCHAR2({} CHAR)", text_len(c.max_chars)),
    }
}

/// `CREATE TABLE` for a file that has no target table yet. Shown to the user before it runs.
pub fn create_table_ddl(p: &FileProfile, owner: &str, table: &str) -> Result<String> {
    let mut used = std::collections::HashSet::new();
    let cols: Vec<String> = p
        .columns
        .iter()
        .map(|c| {
            let mut n = oracle_ident(&c.name);
            let mut k = 1;
            while !used.insert(n.clone()) {
                k += 1;
                n = format!("{}_{k}", oracle_ident(&c.name));
            }
            format!("  \"{n}\" {}", suggest_type(c))
        })
        .collect();
    Ok(format!(
        "CREATE TABLE {}.{} (\n{}\n)",
        quote_ident(owner)?,
        quote_ident(table)?,
        cols.join(",\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::source::{open_source, ReadOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);
    fn prof(csv: &str) -> FileProfile {
        let d = std::env::temp_dir().join(format!(
            "ora-ide-prof-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("t.csv");
        std::fs::write(&p, csv).unwrap();
        profile_source(
            &mut *open_source(&p, &ReadOptions::default()).unwrap(),
            100_000,
        )
        .unwrap()
    }
    fn one(values: &[&str]) -> ColProfile {
        let csv = format!(
            "v\n{}\n",
            values
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join("\n")
        );
        prof(&csv).columns.remove(0)
    }

    #[test]
    fn integers_decimals_and_sizes() {
        let c = one(&["1", "-25", "+300", "", "42"]);
        assert_eq!(
            (c.kind, c.int_digits, c.scale, c.nulls, c.non_null),
            (Kind::Integer, 3, 0, 1, 4)
        );
        let c = one(&["12.5", "3", "-0.25", "1000.125"]);
        assert_eq!((c.kind, c.int_digits, c.scale), (Kind::Decimal, 4, 3));
        assert_eq!(suggest_type(&c), "NUMBER(7,3)");
        assert_eq!(suggest_type(&one(&["1", "999"])), "NUMBER(3)");
        assert_eq!(
            one(&["1E3", "2.5e-2"]).kind,
            Kind::Decimal,
            "scientific notation"
        );
    }

    #[test]
    fn number_styles_us_european_and_the_ambiguous_case() {
        let us = one(&["1,234.56", "12,345,678.9", "7"]);
        assert_eq!(
            (us.kind, us.number_style, us.number_ambiguous),
            (
                Kind::Decimal,
                Some(NumStyle {
                    decimal: '.',
                    thousands: Some(',')
                }),
                false
            )
        );
        let eu = one(&["1.234,56", "12.345.678,9", "7"]);
        assert_eq!(
            (
                eu.kind,
                eu.number_style.unwrap().decimal,
                eu.number_ambiguous
            ),
            (Kind::Decimal, ',', false)
        );
        let plain_comma = one(&["3,5", "10,25"]);
        assert_eq!(
            (plain_comma.kind, plain_comma.number_style.unwrap().decimal),
            (Kind::Decimal, ',')
        );
        // "1,234" is 1234 in the US and 1.234 in Europe
        let amb = one(&["1,234", "5,678"]);
        assert!(
            amb.number_ambiguous,
            "must be flagged, not guessed silently"
        );
        // "1.234" likewise
        assert!(one(&["1.234", "2.345"]).number_ambiguous);
        // plain integers and ordinary decimals are not ambiguous
        assert!(!one(&["1", "22", "333"]).number_ambiguous);
        assert!(!one(&["1.5", "2.25"]).number_ambiguous);
    }

    #[test]
    fn leading_zeros_stay_text() {
        for vals in [&["007", "012"][..], &["01234", "98765"], &["0123", "5"]] {
            let c = one(vals);
            assert_eq!(c.kind, Kind::Text, "{vals:?}");
            assert!(c.leading_zeros);
        }
        assert_eq!(
            one(&["0", "0.5", "10"]).kind,
            Kind::Decimal,
            "a lone 0 and 0.x are numbers"
        );
        assert_eq!(
            one(&["12345678901234567890123456789012345678901"]).kind,
            Kind::Text,
            "more than 38 digits cannot be an Oracle NUMBER"
        );
    }

    #[test]
    fn date_formats_are_detected_and_ambiguity_flagged() {
        let iso = one(&["2020-01-31", "2021-12-01"]);
        assert_eq!(
            (iso.kind, iso.date_formats.clone(), iso.date_ambiguous),
            (Kind::Date, vec!["YYYY-MM-DD".to_string()], false)
        );
        // a value with day > 12 settles the order
        let dmy = one(&["31/01/2020", "05/02/2020"]);
        assert_eq!(
            (dmy.date_formats.clone(), dmy.date_ambiguous),
            (vec!["DD/MM/YYYY".to_string()], false)
        );
        let mdy = one(&["01/31/2020", "02/05/2020"]);
        assert_eq!(mdy.date_formats, vec!["MM/DD/YYYY".to_string()]);
        // everything <= 12: genuinely ambiguous
        let amb = one(&["03/04/2020", "05/06/2020"]);
        assert!(amb.date_ambiguous);
        assert_eq!(
            amb.date_formats,
            vec!["DD/MM/YYYY".to_string(), "MM/DD/YYYY".to_string()]
        );
        assert_eq!(
            one(&["31.01.2020", "01.02.2020"]).date_formats,
            vec!["DD.MM.YYYY".to_string()]
        );
        assert_eq!(
            one(&["31-JAN-2020", "01-Feb-2020"]).date_formats,
            vec!["DD-MON-YYYY".to_string()]
        );
        assert_eq!(
            one(&["2020/01/31"]).date_formats,
            vec!["YYYY/MM/DD".to_string()]
        );
        // invalid calendar dates are not dates
        assert_eq!(one(&["2020-02-30", "2020-01-01"]).kind, Kind::Text);
    }

    #[test]
    fn times_and_fractions_make_date_or_timestamp() {
        let c = one(&["2020-01-31 10:20:30", "2020-02-01", "2020-02-02 00:00:00"]);
        assert_eq!(
            c.kind,
            Kind::Date,
            "a mix of date-only and date-time values is one DATE column"
        );
        let t = one(&["2020-01-31 10:20:30.123456", "2020-01-31 10:20:31"]);
        assert_eq!(t.kind, Kind::Timestamp);
        assert_eq!(suggest_type(&t), "TIMESTAMP(6)");
        assert_eq!(
            one(&["2020-01-31T10:20:30", "2020-01-31T11:00:00"]).kind,
            Kind::Date
        );
        assert_eq!(
            one(&["31/01/2020 14:05", "01/02/2020 09:30"]).kind,
            Kind::Date
        );
        assert_eq!(
            one(&["01/31/2020 02:05:09 PM", "02/01/2020 09:30:00 AM"]).date_formats,
            vec!["MM/DD/YYYY".to_string()]
        );
    }

    #[test]
    fn booleans_text_empty_and_mixed() {
        assert_eq!(one(&["true", "FALSE", "true"]).kind, Kind::Boolean);
        assert_eq!(one(&["Y", "N", "y", ""]).kind, Kind::Boolean);
        assert_eq!(one(&["yes", "no"]).kind, Kind::Boolean);
        assert_eq!(
            one(&["0", "1", "1"]).kind,
            Kind::Integer,
            "0/1 columns stay numbers"
        );
        assert_eq!(one(&["", " "]).kind, Kind::Empty);
        let t = one(&["Ann", "Bob", "42"]);
        assert_eq!(
            t.kind,
            Kind::Text,
            "a single non-number makes the whole column text"
        );
        assert_eq!(suggest_type(&t), "VARCHAR2(16 CHAR)");
        let long = one(&[&"x".repeat(5000)]);
        assert_eq!(
            (long.kind, suggest_type(&long).as_str()),
            (Kind::Text, "CLOB")
        );
        assert_eq!(
            suggest_type(&one(&[&"x".repeat(300)])),
            "VARCHAR2(500 CHAR)"
        );
    }

    #[test]
    fn whole_file_profile_with_warnings_and_sampling() {
        let p = prof(
            "id,name,joined,amount\n1,Ann,03/04/2020,\"1,234\"\n2,Bob,05/06/2020,\"5,678\"\n3,Cy\n",
        );
        assert_eq!(
            p.columns.iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![Kind::Integer, Kind::Text, Kind::Date, Kind::Integer]
        );
        assert_eq!(p.ragged_rows, 1);
        assert_eq!((p.rows_scanned, p.has_header), (3, true));
        assert!(p
            .warnings
            .iter()
            .any(|w| w.contains("different number of fields")));
        assert!(
            p.warnings.iter().any(|w| w.contains("day-first"))
                && p.warnings.iter().any(|w| w.contains("decimal mark"))
        );
        // sampling stops early and says so
        let d = std::env::temp_dir().join(format!("ora-ide-prof-s-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("big.csv");
        std::fs::write(
            &f,
            format!(
                "n\n{}",
                (0..50)
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .unwrap();
        let p =
            profile_source(&mut *open_source(&f, &ReadOptions::default()).unwrap(), 20).unwrap();
        assert_eq!((p.rows_scanned, p.more_rows), (20, true));
    }

    #[test]
    fn create_table_ddl_is_safe_and_readable() {
        let p = prof("Order ID,Order Date,Customer Name,Amount,date,Order ID\n1,2020-01-31,Ann,10.50,2020-01-01,5\n22,2020-02-01,Bob,200.25,2020-01-02,6\n");
        let ddl = create_table_ddl(&p, "HR", "ORDERS_IMPORT").unwrap();
        assert_eq!(
            ddl,
            "CREATE TABLE \"HR\".\"ORDERS_IMPORT\" (\n  \"ORDER_ID\" NUMBER(2),\n  \"ORDER_DATE\" DATE,\n  \"CUSTOMER_NAME\" VARCHAR2(16 CHAR),\n  \"AMOUNT\" NUMBER(5,2),\n  \"DATE_\" DATE,\n  \"ORDER_ID_2\" NUMBER(1)\n)"
        );
        assert!(
            create_table_ddl(&p, "HR", "T\" ; DROP").is_err(),
            "table name is validated"
        );
        assert_eq!(oracle_ident("1st col!"), "C_1ST_COL");
        assert_eq!(oracle_ident("  "), "COL");
    }

    #[test]
    fn number_parsing_details() {
        let c = |s: &str, d| parse_num(s, d).map(|p| canonical_num(&p));
        assert_eq!(c("1,234.50", '.'), Ok("1234.50".to_string()));
        assert_eq!(c("1.234,50", ','), Ok("1234.50".to_string()));
        assert_eq!(c("-.5", '.'), Ok("-0.5".to_string()));
        assert_eq!(c("1.5e3", '.'), Ok("1500".to_string()));
        assert_eq!(c("1.5e-3", '.'), Ok("0.0015".to_string()));
        assert_eq!(c("-0.00", '.'), Ok("0.00".to_string()), "no negative zero");
        assert_eq!(
            c("1,23", '.'),
            Err(NumErr::Invalid),
            "bad thousands grouping"
        );
        assert_eq!(c("1,234,5", '.'), Err(NumErr::Invalid));
        assert_eq!(c("12abc", '.'), Err(NumErr::Invalid));
        assert_eq!(c("5.", '.'), Err(NumErr::Invalid));
        assert_eq!(c("007", '.'), Err(NumErr::LeadingZero));
        assert_eq!(c("", '.'), Err(NumErr::Invalid));
    }
}
