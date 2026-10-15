//! Mapping a profiled file onto a target table, converting and validating rows, and
//! loading them in array-bound batches.
//!
//! Every value is converted in Rust to a canonical text form (`1234.56`,
//! `2020-01-31 10:20:30`) and bound as text; the INSERT converts with explicit formats
//! (`TO_NUMBER(.., 'TM9', 'NLS_NUMERIC_CHARACTERS=''.,''')`, `TO_DATE(.., 'YYYY-MM-DD HH24:MI:SS')`),
//! so the result never depends on the session's NLS settings.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::profile::{
    canonical_date, canonical_num, family, parse_date, parse_num, suggest_type, ColProfile,
    FileProfile, Kind, NumErr,
};
use super::source::RowSource;
use crate::comments::quote_ident;
use crate::db::{collect, Database, Row};
use crate::schema::display_type;
use crate::{Error, Result};

// ---------------------------------------------------------------------------- targets

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetCol {
    pub name: String,
    pub data_type: String,
    /// `VARCHAR2(30 CHAR)`, `NUMBER(10,2)`, ...
    pub display: String,
    pub char_length: u32,
    pub data_length: u32,
    /// `C` (characters), `B` (bytes) or empty.
    pub char_used: String,
    pub precision: Option<u32>,
    pub scale: Option<i32>,
    pub nullable: bool,
    /// Virtual and `GENERATED ALWAYS` identity columns cannot be inserted into.
    pub insertable: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TKind {
    Text,
    Clob,
    Number,
    Date,
    Timestamp,
    Unsupported,
}

impl TargetCol {
    fn tkind(&self) -> TKind {
        let u = self.data_type.to_uppercase();
        if u.starts_with("TIMESTAMP") {
            return if u.contains("TIME ZONE") {
                TKind::Unsupported
            } else {
                TKind::Timestamp
            };
        }
        match u.as_str() {
            "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" | "VARCHAR" => TKind::Text,
            "CLOB" | "NCLOB" | "LONG" => TKind::Clob,
            "NUMBER" | "FLOAT" | "BINARY_FLOAT" | "BINARY_DOUBLE" | "INTEGER" | "DECIMAL"
            | "NUMERIC" | "SMALLINT" => TKind::Number,
            "DATE" => TKind::Date,
            _ => TKind::Unsupported,
        }
    }
}

const SQL_TARGETS: &str = "SELECT /*imp:targets*/ column_name, data_type, data_length, char_length, char_used, data_precision, data_scale, nullable, virtual_column \
     FROM all_tab_cols WHERE owner = :1 AND table_name = :2 AND hidden_column = 'NO' ORDER BY column_id";
/// Oracle before 12c has no hidden/virtual column flags.
const SQL_TARGETS_LEGACY: &str = "SELECT /*imp:targets*/ column_name, data_type, data_length, char_length, char_used, data_precision, data_scale, nullable, 'NO' \
     FROM all_tab_columns WHERE owner = :1 AND table_name = :2 ORDER BY column_id";
const SQL_IDENTITY: &str = "SELECT /*imp:identity*/ column_name, generation_type FROM all_tab_identity_cols WHERE owner = :1 AND table_name = :2";

fn cell(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

pub fn load_targets(db: &dyn Database, owner: &str, table: &str) -> Result<Vec<TargetCol>> {
    let b = [owner.to_string(), table.to_string()];
    let rows = match collect(db, SQL_TARGETS, &b) {
        Ok((_, r)) => r,
        Err(_) => collect(db, SQL_TARGETS_LEGACY, &b)?.1,
    };
    if rows.is_empty() {
        return Err(Error::Invalid(format!(
            "table {owner}.{table} was not found or is not visible to this user"
        )));
    }
    let always: Vec<String> = collect(db, SQL_IDENTITY, &b)
        .map(|(_, r)| {
            r.iter()
                .filter(|x| cell(x, 1) == "ALWAYS")
                .map(|x| cell(x, 0))
                .collect()
        })
        .unwrap_or_default();
    Ok(rows
        .iter()
        .map(|r| {
            let name = cell(r, 0);
            let (dl, cl) = (
                cell(r, 2).parse().unwrap_or(0),
                cell(r, 3).parse().unwrap_or(0),
            );
            let precision = cell(r, 5).parse().ok();
            let scale = cell(r, 6).parse().ok();
            let virt = cell(r, 8) == "YES";
            let ident = always.contains(&name);
            let (insertable, note) = if virt {
                (false, Some("virtual column: calculated by Oracle".into()))
            } else if ident {
                (
                    false,
                    Some("GENERATED ALWAYS identity: filled by Oracle".into()),
                )
            } else {
                (true, None)
            };
            TargetCol {
                display: display_type(
                    &cell(r, 1),
                    &cell(r, 2),
                    &cell(r, 3),
                    &cell(r, 4),
                    &cell(r, 5),
                    &cell(r, 6),
                ),
                data_type: cell(r, 1),
                char_length: cl,
                data_length: dl,
                char_used: cell(r, 4),
                precision,
                scale,
                nullable: cell(r, 7) == "Y",
                insertable,
                note,
                name,
            }
        })
        .collect())
}

// ----------------------------------------------------------------------------- plan

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Conversion {
    /// Copy the text as it is.
    Text,
    Number {
        decimal: char,
    },
    /// Parsed with a named format family (see [`super::profile::FAMILIES`]).
    Date {
        family: String,
    },
    Timestamp {
        family: String,
    },
    /// true/yes/y/t/1 -> 1, false/no/n/f/0 -> 0.
    Boolean,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub severity: Severity,
    pub message: String,
    /// A statement that would remove the problem, for the user to review and run.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub label: String,
    /// The value to send back in [`PlanInput`] (a date family or `.` / `,`).
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMap {
    pub target: TargetCol,
    pub file_index: Option<usize>,
    pub file_name: Option<String>,
    pub conversion: Conversion,
    #[serde(skip_deserializing)]
    pub issues: Vec<Issue>,
    /// Set when more than one reading of the data is possible (date order, decimal mark).
    #[serde(skip_deserializing)]
    pub choices: Vec<Choice>,
    #[serde(skip_deserializing)]
    pub suggested_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MappingPlan {
    pub owner: String,
    pub table: String,
    pub maps: Vec<ColumnMap>,
    #[serde(skip_deserializing)]
    pub unmapped_file: Vec<String>,
    #[serde(skip_deserializing)]
    pub by_position: bool,
    #[serde(skip_deserializing)]
    pub errors: usize,
    #[serde(skip_deserializing)]
    pub warnings: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlanInput {
    /// Target column name -> file column index (`null`: leave unmapped). Absent: automatic.
    pub assignment: Option<HashMap<String, Option<usize>>>,
    /// Target column name -> date format family.
    pub date_family: HashMap<String, String>,
    /// Target column name -> `.` or `,`.
    pub number_decimal: HashMap<String, char>,
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

fn issue(severity: Severity, message: impl Into<String>, fix: Option<String>) -> Issue {
    Issue {
        severity,
        message: message.into(),
        fix,
    }
}

fn qname(owner: &str, table: &str) -> String {
    format!(
        "{}.{}",
        quote_ident(owner).unwrap_or_default(),
        quote_ident(table).unwrap_or_default()
    )
}

fn modify_fix(owner: &str, table: &str, t: &TargetCol, new_type: &str) -> Option<String> {
    Some(format!(
        "ALTER TABLE {} MODIFY ({} {})",
        qname(owner, table),
        quote_ident(&t.name).ok()?,
        new_type
    ))
}

pub fn plan_mapping(
    owner: &str,
    table: &str,
    profile: &FileProfile,
    targets: &[TargetCol],
    input: &PlanInput,
) -> MappingPlan {
    let insertable: Vec<&TargetCol> = targets.iter().filter(|t| t.insertable).collect();
    // 1. which file column feeds each target
    let mut by_position = false;
    let auto: HashMap<String, Option<usize>> = {
        let mut m: HashMap<String, Option<usize>> = HashMap::new();
        let mut used = vec![false; profile.columns.len()];
        for t in &insertable {
            let hit = profile
                .columns
                .iter()
                .find(|c| !used[c.index] && norm(&c.name) == norm(&t.name))
                .map(|c| c.index);
            if let Some(i) = hit {
                used[i] = true;
            }
            m.insert(t.name.clone(), hit);
        }
        // Nothing matched by name (e.g. a file without a header): fall back to position when the shapes agree.
        if m.values().all(Option::is_none)
            && !profile.columns.is_empty()
            && profile.columns.len() <= insertable.len()
        {
            by_position = true;
            for (i, t) in insertable.iter().enumerate().take(profile.columns.len()) {
                m.insert(t.name.clone(), Some(i));
            }
        }
        m
    };
    let assignment = input.assignment.clone().unwrap_or(auto);

    let mut maps = Vec::new();
    for t in targets {
        let file_index = if t.insertable {
            assignment
                .get(&t.name)
                .cloned()
                .flatten()
                .filter(|i| *i < profile.columns.len())
        } else {
            None
        };
        let col = file_index.map(|i| &profile.columns[i]);
        let mut m = ColumnMap {
            target: t.clone(),
            file_index,
            file_name: col.map(|c| c.name.clone()),
            conversion: Conversion::Text,
            issues: vec![],
            choices: vec![],
            suggested_type: None,
        };
        if !t.insertable {
            m.issues.push(issue(
                Severity::Info,
                t.note.clone().unwrap_or_else(|| "not insertable".into()),
                None,
            ));
        } else if let Some(c) = col {
            decide(owner, table, c, t, input, &mut m);
        } else if !t.nullable {
            m.issues.push(issue(Severity::Warning, "NOT NULL column has no value in the file: the insert fails unless the column has a DEFAULT or is filled by a trigger.", None));
        }
        maps.push(m);
    }
    let mapped: Vec<usize> = maps.iter().filter_map(|m| m.file_index).collect();
    let unmapped_file = profile
        .columns
        .iter()
        .filter(|c| !mapped.contains(&c.index))
        .map(|c| c.name.clone())
        .collect();
    let count = |s: Severity| {
        maps.iter()
            .flat_map(|m| &m.issues)
            .filter(|i| i.severity == s)
            .count()
    };
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    MappingPlan {
        owner: owner.into(),
        table: table.into(),
        maps,
        unmapped_file,
        by_position,
        errors,
        warnings,
    }
}

fn first_bad(samples: &[String], f: impl Fn(&str) -> bool) -> Option<&String> {
    samples.iter().find(|s| !f(s))
}

fn decide(
    owner: &str,
    table: &str,
    c: &ColProfile,
    t: &TargetCol,
    input: &PlanInput,
    m: &mut ColumnMap,
) {
    m.suggested_type = Some(suggest_type(c));
    let tk = t.tkind();
    let push = |m: &mut ColumnMap, sev, msg: String, fix: Option<String>| {
        m.issues.push(issue(sev, msg, fix))
    };
    match tk {
        TKind::Unsupported => push(
            m,
            Severity::Error,
            format!(
                "{} columns are not supported by the importer yet.",
                t.data_type
            ),
            None,
        ),
        TKind::Text | TKind::Clob => {
            m.conversion = Conversion::Text;
            if tk == TKind::Text {
                let (have, limit, unit) = if t.char_used == "C" {
                    (c.max_chars, t.char_length as usize, "characters")
                } else {
                    (c.max_bytes, t.data_length as usize, "bytes")
                };
                if limit > 0 && have > limit {
                    let want = ((have as f64) * 1.25).ceil() as usize;
                    let ty = if t.data_type.starts_with('N') {
                        format!("{}({want})", t.data_type)
                    } else {
                        format!("{}({want} CHAR)", t.data_type)
                    };
                    push(m, Severity::Error, format!("The longest value has {have} {unit} but the column holds {limit}: those rows would be rejected."), modify_fix(owner, table, t, &ty));
                }
            } else if c.max_bytes > 32_000 {
                push(
                    m,
                    Severity::Warning,
                    "Values over 32,000 bytes cannot be bound as text and will be rejected.".into(),
                    None,
                );
            }
            if matches!(c.kind, Kind::Date | Kind::Timestamp) {
                push(
                    m,
                    Severity::Info,
                    "Dates are stored as text exactly as written in the file.".into(),
                    None,
                );
            }
        }
        TKind::Number => {
            match c.kind {
                Kind::Empty => {}
                Kind::Boolean => {
                    m.conversion = Conversion::Boolean;
                    push(
                        m,
                        Severity::Info,
                        "true/yes/y become 1 and false/no/n become 0.".into(),
                        None,
                    );
                }
                kind => {
                    let numeric = matches!(kind, Kind::Integer | Kind::Decimal);
                    // The target decides the conversion. For a column that is only mostly numeric, read
                    // numbers with whichever decimal mark more of the values fit.
                    let default = if numeric {
                        c.number_style.map(|s| s.decimal).unwrap_or('.')
                    } else if c.comma_hits > c.dot_hits {
                        ','
                    } else {
                        '.'
                    };
                    let decimal = input
                        .number_decimal
                        .get(&t.name)
                        .copied()
                        .unwrap_or(default);
                    m.conversion = Conversion::Number { decimal };
                    let hits = c.dot_hits.max(c.comma_hits);
                    if !numeric {
                        let bad = first_bad(&c.samples, |s| {
                            !matches!(parse_num(s, decimal), Err(NumErr::Invalid))
                        })
                        .cloned()
                        .unwrap_or_default();
                        if hits == 0 {
                            push(m, Severity::Error, format!("The file column holds text (e.g. '{bad}') but the target is {}.", t.display), None);
                        } else if hits < c.hit_sample {
                            push(m, Severity::Warning, format!("{} of the first {} values are not numbers (e.g. '{bad}'); those rows will be rejected.", c.hit_sample - hits, c.hit_sample), None);
                        }
                    }
                    if c.number_ambiguous {
                        push(m, Severity::Warning, format!("Numbers like {} could use '.' or ',' as the decimal mark; reading them with '{decimal}'. Check this.", c.samples.first().cloned().unwrap_or_default()), None);
                        m.choices = vec![
                            Choice {
                                label: "1,234.56 (dot is the decimal mark)".into(),
                                value: ".".into(),
                            },
                            Choice {
                                label: "1.234,56 (comma is the decimal mark)".into(),
                                value: ",".into(),
                            },
                        ];
                    }
                    if c.leading_zeros {
                        push(
                            m,
                            Severity::Warning,
                            "Values have leading zeros; they will be dropped in a number column."
                                .into(),
                            None,
                        );
                    }
                    if numeric {
                        if let (Some(p), Some(s)) = (t.precision, t.scale.or(Some(0))) {
                            let room = p as i64 - s.max(0) as i64;
                            if (c.int_digits as i64) > room {
                                let want = c.int_digits as u32 + s.max(0) as u32;
                                push(m, Severity::Error, format!("Values need {} digits before the decimal point but NUMBER({p},{s}) allows {room}.", c.int_digits), modify_fix(owner, table, t, &format!("NUMBER({want},{})", s.max(0))));
                            }
                            if c.scale as i32 > s && s >= 0 {
                                push(m, Severity::Warning, format!("Values have up to {} decimals; the column keeps {s}, so extra digits are rounded.", c.scale), None);
                            }
                        }
                    }
                }
            }
        }
        TKind::Date | TKind::Timestamp => match c.kind {
            Kind::Empty => {}
            Kind::Date | Kind::Timestamp => {
                let default = c.date_formats.first().cloned().unwrap_or_default();
                let fam = input
                    .date_family
                    .get(&t.name)
                    .cloned()
                    .filter(|f| c.date_formats.contains(f))
                    .unwrap_or(default);
                m.conversion = if tk == TKind::Date {
                    Conversion::Date {
                        family: fam.clone(),
                    }
                } else {
                    Conversion::Timestamp {
                        family: fam.clone(),
                    }
                };
                if c.date_ambiguous {
                    push(m, Severity::Warning, format!("Dates like {} could be read in more than one order; reading them as {fam}. Check this.", c.samples.first().cloned().unwrap_or_default()), None);
                    m.choices = c
                        .date_formats
                        .iter()
                        .map(|f| Choice {
                            label: f.clone(),
                            value: f.clone(),
                        })
                        .collect();
                }
                if tk == TKind::Date && c.kind == Kind::Timestamp {
                    push(
                        m,
                        Severity::Info,
                        "Fractional seconds are dropped: DATE has whole-second precision.".into(),
                        None,
                    );
                }
            }
            _ => {
                // Mostly-dates column with some dirty cells: the best-fitting format decides.
                let best = c
                    .date_hits
                    .iter()
                    .find(|(f, _)| {
                        input
                            .date_family
                            .get(&t.name)
                            .map(|x| x == f)
                            .unwrap_or(true)
                    })
                    .or(c.date_hits.first());
                match best {
                    None => {
                        let bad = c.samples.first().cloned().unwrap_or_default();
                        push(m, Severity::Error, format!("No date format could be recognised in this column (e.g. '{bad}'); rows would be rejected."), None);
                    }
                    Some((fam, n)) => {
                        m.conversion = if tk == TKind::Date {
                            Conversion::Date {
                                family: fam.clone(),
                            }
                        } else {
                            Conversion::Timestamp {
                                family: fam.clone(),
                            }
                        };
                        if *n < c.hit_sample {
                            let bad = first_bad(&c.samples, |s| {
                                family(fam).and_then(|f| parse_date(s, f)).is_some()
                            })
                            .cloned()
                            .unwrap_or_default();
                            push(m, Severity::Warning, format!("{} of the first {} values are not {fam} dates (e.g. '{bad}'); those rows will be rejected.", c.hit_sample - n, c.hit_sample), None);
                        }
                    }
                }
            }
        },
    }
}

// ------------------------------------------------------------------- compile + convert

struct CCol {
    file_index: usize,
    name: String,
    conv: Conversion,
    tk: TKind,
    nullable: bool,
    target: TargetCol,
}

pub struct Compiled {
    cols: Vec<CCol>,
    pub sql: String,
    pub qualified: String,
    pub file_columns: usize,
}

/// Validate a plan (typically sent back from the UI) and build the INSERT. Identifiers are
/// checked, conversions must name known formats, and plans with errors are refused.
pub fn compile(plan: &MappingPlan, hint: bool) -> Result<Compiled> {
    let qualified = format!(
        "{}.{}",
        quote_ident(&plan.owner)?,
        quote_ident(&plan.table)?
    );
    let mut cols = Vec::new();
    let mut exprs = Vec::new();
    let mut names = Vec::new();
    for m in &plan.maps {
        let Some(fi) = m.file_index else { continue };
        if !m.target.insertable {
            continue;
        }
        let tk = m.target.tkind();
        if tk == TKind::Unsupported {
            return Err(Error::Invalid(format!(
                "column {} has an unsupported type",
                m.target.name
            )));
        }
        if let Conversion::Date { family: f } | Conversion::Timestamp { family: f } = &m.conversion
        {
            family(f).ok_or_else(|| Error::Invalid(format!("unknown date format {f:?}")))?;
        }
        let ph = format!(":{}", cols.len() + 1);
        exprs.push(match tk {
            TKind::Number => format!("TO_NUMBER({ph}, 'TM9', 'NLS_NUMERIC_CHARACTERS=''.,''')"),
            TKind::Date => format!("TO_DATE({ph}, 'YYYY-MM-DD HH24:MI:SS')"),
            TKind::Timestamp => format!("TO_TIMESTAMP({ph}, 'YYYY-MM-DD HH24:MI:SS.FF6')"),
            _ => ph,
        });
        names.push(quote_ident(&m.target.name)?);
        cols.push(CCol {
            file_index: fi,
            name: m.target.name.clone(),
            conv: m.conversion.clone(),
            tk,
            nullable: m.target.nullable,
            target: m.target.clone(),
        });
    }
    if cols.is_empty() {
        return Err(Error::Invalid(
            "no file column is mapped to a table column".into(),
        ));
    }
    let file_columns = cols.iter().map(|c| c.file_index + 1).max().unwrap_or(0);
    let sql = format!(
        "INSERT {}INTO {qualified} ({}) VALUES ({})",
        if hint { "/*+ APPEND_VALUES */ " } else { "" },
        names.join(", "),
        exprs.join(", ")
    );
    Ok(Compiled {
        cols,
        sql,
        qualified,
        file_columns,
    })
}

fn too_long(v: &str, t: &TargetCol) -> Option<String> {
    if t.char_used == "C" {
        let n = v.chars().count();
        (t.char_length > 0 && n > t.char_length as usize).then(|| {
            format!(
                "value too long for {} ({n} characters, limit {})",
                t.name, t.char_length
            )
        })
    } else {
        (t.data_length > 0 && v.len() > t.data_length as usize).then(|| {
            format!(
                "value too long for {} ({} bytes, limit {})",
                t.name,
                v.len(),
                t.data_length
            )
        })
    }
}

fn convert_cell(raw: &str, c: &CCol, trim: bool) -> std::result::Result<Option<String>, String> {
    let is_text = matches!(c.conv, Conversion::Text);
    let v = if is_text && !trim { raw } else { raw.trim() };
    if v.is_empty() {
        // Oracle stores '' as NULL, so an empty cell is NULL
        return if c.nullable {
            Ok(None)
        } else {
            Err(format!("required value missing for {}", c.name))
        };
    }
    match &c.conv {
        Conversion::Text => {
            if c.tk == TKind::Text {
                if let Some(e) = too_long(v, &c.target) {
                    return Err(e);
                }
            }
            Ok(Some(v.to_string()))
        }
        Conversion::Number { decimal } => {
            let p = match parse_num(v, *decimal) {
                Ok(p) => p,
                // `007` -> 7 when the target is a number
                Err(NumErr::LeadingZero) => {
                    parse_num(v.trim_start_matches(['0', '+', '-']), *decimal)
                        .map_err(|_| format!("'{v}' is not a number"))?
                }
                Err(NumErr::Invalid) => return Err(format!("'{v}' is not a valid number")),
            };
            if let (Some(pr), s) = (c.target.precision, c.target.scale.unwrap_or(0)) {
                let room = pr as i64 - s.max(0) as i64;
                if (p.int.len() as i64) > room && p.int != "0" {
                    return Err(format!(
                        "{v} needs {} digits before the decimal point; {} allows {room}",
                        p.int.len(),
                        c.target.display
                    ));
                }
            }
            Ok(Some(canonical_num(&p)))
        }
        Conversion::Date { family: f } | Conversion::Timestamp { family: f } => {
            let fam = family(f).ok_or_else(|| format!("unknown date format {f}"))?;
            let (dt, _) =
                parse_date(v, fam).ok_or_else(|| format!("'{v}' is not a valid {f} date"))?;
            Ok(Some(canonical_date(&dt, c.tk == TKind::Timestamp)))
        }
        Conversion::Boolean => match v.to_lowercase().as_str() {
            "true" | "yes" | "y" | "t" | "1" => Ok(Some("1".into())),
            "false" | "no" | "n" | "f" | "0" => Ok(Some("0".into())),
            _ => Err(format!("'{v}' is not a yes/no value")),
        },
    }
}

impl Compiled {
    /// Convert one file row into the bind values of the INSERT (one per mapped column).
    pub fn convert_row(
        &self,
        record: &[String],
        trim: bool,
    ) -> std::result::Result<Vec<Option<String>>, (String, usize)> {
        if record.len() < self.file_columns {
            return Err((
                format!(
                    "row has {} fields, at least {} are needed",
                    record.len(),
                    self.file_columns
                ),
                usize::MAX,
            ));
        }
        let mut out = Vec::with_capacity(self.cols.len());
        for (i, c) in self.cols.iter().enumerate() {
            match convert_cell(&record[c.file_index], c, trim) {
                Ok(v) => out.push(v),
                Err(e) => return Err((e, i)),
            }
        }
        Ok(out)
    }
    fn column_name(&self, i: usize) -> &str {
        self.cols.get(i).map(|c| c.name.as_str()).unwrap_or("(row)")
    }
}

// ---------------------------------------------------------------- validate + import

#[derive(Debug, Clone, Serialize)]
pub struct Reject {
    /// 1-based data row (the header does not count).
    pub row: u64,
    pub reason: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ValidationReport {
    pub rows: u64,
    pub ok: u64,
    pub rejected: u64,
    pub by_column: Vec<(String, u64)>,
    pub samples: Vec<Reject>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ImportProgress {
    pub read: u64,
    pub inserted: u64,
    pub rejected: u64,
    pub elapsed_ms: u64,
}

/// Dry run: convert every row without touching the database.
pub fn validate(
    src: &mut dyn RowSource,
    plan: &MappingPlan,
    trim: bool,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(&ImportProgress),
) -> Result<ValidationReport> {
    let c = compile(plan, false)?;
    let mut rep = ValidationReport::default();
    let mut per: HashMap<String, u64> = HashMap::new();
    let t0 = Instant::now();
    while let Some(rec) = src.next_row()? {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            break;
        }
        rep.rows += 1;
        match c.convert_row(&rec, trim) {
            Ok(_) => rep.ok += 1,
            Err((reason, col)) => {
                rep.rejected += 1;
                *per.entry(c.column_name(col).to_string()).or_default() += 1;
                if rep.samples.len() < 50 {
                    rep.samples.push(Reject {
                        row: rep.rows,
                        reason,
                        fields: rec,
                    });
                }
            }
        }
        if rep.rows % 20_000 == 0 {
            on_progress(&ImportProgress {
                read: rep.rows,
                inserted: rep.ok,
                rejected: rep.rejected,
                elapsed_ms: t0.elapsed().as_millis() as u64,
            });
        }
    }
    let mut by: Vec<(String, u64)> = per.into_iter().collect();
    by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rep.by_column = by;
    Ok(rep)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitMode {
    /// One transaction: all rows or none.
    AtEnd,
    /// Commit after every batch: partial progress survives a failure.
    PerBatch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    /// Stop at the first bad row.
    Stop,
    /// Put bad rows in the reject file and carry on.
    Skip,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ImportOptions {
    pub batch_size: usize,
    pub commit: CommitMode,
    pub on_error: OnError,
    /// Abort when more rows than this were rejected.
    pub max_rejects: u64,
    /// `TRUNCATE TABLE` first. DDL: it commits and cannot be rolled back.
    pub truncate_first: bool,
    pub trim: bool,
    /// Direct-path insert (`APPEND_VALUES`): faster, but the table is locked until commit.
    pub append_hint: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            batch_size: 5_000,
            commit: CommitMode::AtEnd,
            on_error: OnError::Skip,
            max_rejects: 10_000,
            truncate_first: false,
            trim: false,
            append_hint: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ImportReport {
    pub read: u64,
    pub inserted: u64,
    pub rejected: u64,
    pub committed: bool,
    pub rolled_back: bool,
    pub reject_file: Option<String>,
    pub stopped: Option<String>,
    pub cancelled: bool,
    pub elapsed_ms: u64,
    pub rows_per_sec: u64,
    pub samples: Vec<Reject>,
}

struct RejectWriter {
    path: PathBuf,
    w: Option<csv::Writer<File>>,
    header: Vec<String>,
}

impl RejectWriter {
    fn write(&mut self, r: &Reject) -> Result<()> {
        if self.w.is_none() {
            let mut w = csv::Writer::from_path(&self.path)
                .map_err(|e| Error::Invalid(format!("cannot write the reject file: {e}")))?;
            let mut h = vec!["row".to_string(), "reason".to_string()];
            h.extend(self.header.iter().cloned());
            w.write_record(&h)
                .map_err(|e| Error::Invalid(e.to_string()))?;
            self.w = Some(w);
        }
        let mut rec = vec![r.row.to_string(), r.reason.clone()];
        rec.extend(r.fields.iter().cloned());
        self.w
            .as_mut()
            .unwrap()
            .write_record(&rec)
            .map_err(|e| Error::Invalid(e.to_string()))
    }
    fn finish(&mut self) -> Option<String> {
        self.w.take().map(|mut w| {
            let _ = w.flush();
            self.path.to_string_lossy().into_owned()
        })
    }
}

/// Load rows into the target table. Rows that cannot be converted, and rows Oracle rejects,
/// go to `reject_path` (a CSV with the row number and reason) instead of stopping the load,
/// unless `on_error` is `Stop`.
pub fn import(
    db: &dyn Database,
    src: &mut dyn RowSource,
    plan: &MappingPlan,
    opts: &ImportOptions,
    reject_path: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(&ImportProgress),
) -> Result<ImportReport> {
    let c = compile(plan, opts.append_hint)?;
    let header: Vec<String> = src.headers().to_vec();
    let mut rejects = RejectWriter {
        path: reject_path.to_path_buf(),
        w: None,
        header,
    };
    let batch_size = opts.batch_size.clamp(1, 50_000);
    let t0 = Instant::now();
    let mut rep = ImportReport::default();
    if opts.truncate_first {
        db.run(&format!("TRUNCATE TABLE {}", c.qualified), &[], 1)?;
    }

    let mut batch: Vec<Vec<Option<String>>> = Vec::with_capacity(batch_size);
    let mut batch_src: Vec<(u64, Vec<String>)> = Vec::with_capacity(batch_size);
    let mut last_report = Instant::now();

    let mut run = || -> Result<()> {
        let flush = |batch: &mut Vec<Vec<Option<String>>>,
                     batch_src: &mut Vec<(u64, Vec<String>)>,
                     rep: &mut ImportReport,
                     rejects: &mut RejectWriter|
         -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let out = db.insert_batch(&c.sql, batch)?;
            rep.inserted += out.inserted;
            for (idx, msg) in &out.errors {
                let (row, fields) = batch_src.get(*idx).cloned().unwrap_or((0, vec![]));
                let r = Reject {
                    row,
                    reason: msg.clone(),
                    fields,
                };
                rejects.write(&r)?;
                rep.rejected += 1;
                if rep.samples.len() < 50 {
                    rep.samples.push(r);
                }
            }
            batch.clear();
            batch_src.clear();
            if opts.commit == CommitMode::PerBatch {
                db.commit()?;
            }
            Ok(())
        };
        while let Some(rec) = src.next_row()? {
            if cancel.load(Ordering::Relaxed) {
                rep.cancelled = true;
                return Ok(());
            }
            rep.read += 1;
            match c.convert_row(&rec, opts.trim) {
                Ok(vals) => {
                    batch.push(vals);
                    batch_src.push((rep.read, rec));
                }
                Err((reason, _)) => {
                    let r = Reject {
                        row: rep.read,
                        reason: reason.clone(),
                        fields: rec,
                    };
                    rejects.write(&r)?;
                    rep.rejected += 1;
                    if rep.samples.len() < 50 {
                        rep.samples.push(r);
                    }
                    if opts.on_error == OnError::Stop {
                        rep.stopped = Some(format!("stopped at row {}: {reason}", rep.read));
                        return Ok(());
                    }
                }
            }
            if rep.rejected > opts.max_rejects {
                rep.stopped = Some(format!(
                    "more than {} rows were rejected; check the mapping",
                    opts.max_rejects
                ));
                return Ok(());
            }
            if batch.len() >= batch_size {
                flush(&mut batch, &mut batch_src, &mut rep, &mut rejects)?;
                if opts.on_error == OnError::Stop && rep.rejected > 0 {
                    let first = rep
                        .samples
                        .first()
                        .map(|r| format!("row {}: {}", r.row, r.reason))
                        .unwrap_or_default();
                    rep.stopped = Some(format!("stopped: {first}"));
                    return Ok(());
                }
            }
            if last_report.elapsed().as_millis() >= 250 {
                last_report = Instant::now();
                on_progress(&ImportProgress {
                    read: rep.read,
                    inserted: rep.inserted,
                    rejected: rep.rejected,
                    elapsed_ms: t0.elapsed().as_millis() as u64,
                });
            }
        }
        flush(&mut batch, &mut batch_src, &mut rep, &mut rejects)?;
        if opts.on_error == OnError::Stop && rep.rejected > 0 && rep.stopped.is_none() {
            let first = rep
                .samples
                .first()
                .map(|r| format!("row {}: {}", r.row, r.reason))
                .unwrap_or_default();
            rep.stopped = Some(format!("stopped: {first}"));
        }
        Ok(())
    };

    let outcome = run();
    rep.reject_file = rejects.finish();
    if let Err(e) = outcome {
        let _ = db.rollback();
        return Err(e);
    }
    // Decide the fate of the open transaction.
    let incomplete = rep.cancelled || rep.stopped.is_some();
    match opts.commit {
        CommitMode::AtEnd if incomplete => {
            db.rollback()?;
            rep.rolled_back = true;
            rep.inserted = 0;
        }
        CommitMode::AtEnd => {
            db.commit()?;
            rep.committed = true;
        }
        CommitMode::PerBatch => {
            db.commit()?;
            rep.committed = true;
        }
    }
    rep.elapsed_ms = t0.elapsed().as_millis() as u64;
    rep.rows_per_sec = (rep.read * 1000)
        .checked_div(rep.elapsed_ms)
        .unwrap_or(rep.read);
    on_progress(&ImportProgress {
        read: rep.read,
        inserted: rep.inserted,
        rejected: rep.rejected,
        elapsed_ms: rep.elapsed_ms,
    });
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};
    use crate::import::profile::profile_source;
    use crate::import::source::{open_source, ReadOptions};
    use std::sync::atomic::AtomicUsize;

    static N: AtomicUsize = AtomicUsize::new(0);
    fn file(name: &str, body: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ora-ide-plan-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }
    fn profile(p: &Path) -> FileProfile {
        profile_source(
            &mut *open_source(p, &ReadOptions::default()).unwrap(),
            100_000,
        )
        .unwrap()
    }
    fn tgt(name: &str, ty: &str, nullable: bool) -> TargetCol {
        let (base, p, s, len): (&str, Option<u32>, Option<i32>, (u32, &str)) = match ty {
            n if n.starts_with("NUMBER(") => {
                let inner = &n[7..n.len() - 1];
                let (pp, ss) = inner.split_once(',').unwrap_or((inner, "0"));
                ("NUMBER", pp.parse().ok(), ss.parse().ok(), (22, ""))
            }
            "VARCHAR2(5)" => ("VARCHAR2", None, None, (5, "C")),
            "VARCHAR2(30)" => ("VARCHAR2", None, None, (30, "C")),
            "BYTES(4)" => ("VARCHAR2", None, None, (4, "B")),
            "DATE" => ("DATE", None, None, (7, "")),
            "TIMESTAMP(6)" => ("TIMESTAMP(6)", None, None, (11, "")),
            "BLOB" => ("BLOB", None, None, (4000, "")),
            other => (other, None, None, (0, "")),
        };
        TargetCol {
            name: name.into(),
            data_type: base.into(),
            display: ty.into(),
            char_length: len.0,
            data_length: len.0,
            char_used: len.1.into(),
            precision: p,
            scale: s,
            nullable,
            insertable: true,
            note: None,
        }
    }
    fn plan(csv: &str, targets: &[TargetCol]) -> (MappingPlan, PathBuf) {
        let f = file("in.csv", csv);
        (
            plan_mapping("HR", "T", &profile(&f), targets, &PlanInput::default()),
            f,
        )
    }
    fn issues(p: &MappingPlan, col: &str) -> Vec<(Severity, String)> {
        p.maps
            .iter()
            .find(|m| m.target.name == col)
            .unwrap()
            .issues
            .iter()
            .map(|i| (i.severity, i.message.clone()))
            .collect()
    }

    #[test]
    fn maps_by_name_ignoring_case_and_punctuation_and_reports_the_rest() {
        let (p, _) = plan(
            "Employee ID,first-name,Extra\n1,Ann,x\n",
            &[
                tgt("EMPLOYEE_ID", "NUMBER(10)", false),
                tgt("FIRST_NAME", "VARCHAR2(30)", true),
                tgt("SALARY", "NUMBER(8,2)", true),
                tgt("DEPT", "NUMBER(3)", false),
            ],
        );
        assert_eq!(
            p.maps.iter().map(|m| m.file_index).collect::<Vec<_>>(),
            vec![Some(0), Some(1), None, None]
        );
        assert_eq!(p.unmapped_file, vec!["Extra"]);
        assert!(!p.by_position);
        assert!(issues(&p, "DEPT")[0]
            .1
            .contains("NOT NULL column has no value"));
        assert!(
            issues(&p, "SALARY").is_empty(),
            "an unmapped nullable column is fine"
        );
    }

    #[test]
    fn headerless_files_map_by_position() {
        let (p, _) = plan(
            "1,Ann\n2,Bob\n",
            &[
                tgt("ID", "NUMBER(10)", false),
                tgt("NAME", "VARCHAR2(30)", true),
            ],
        );
        assert!(p.by_position);
        assert_eq!(
            p.maps.iter().map(|m| m.file_index).collect::<Vec<_>>(),
            vec![Some(0), Some(1)]
        );
    }

    #[test]
    fn type_problems_come_with_a_suggested_fix() {
        let (p, _) = plan(
            "name,amount,code,joined,qty\nAnnabelle Longname,123456789.5,abc,not a date,12345\nBob,1.5,def,also not,1\n",
            &[tgt("NAME", "VARCHAR2(5)", true), tgt("AMOUNT", "NUMBER(8,2)", true), tgt("CODE", "NUMBER(3)", true), tgt("JOINED", "DATE", true), tgt("QTY", "NUMBER(3)", true)],
        );
        let name = issues(&p, "NAME");
        assert_eq!(name[0].0, Severity::Error);
        assert!(
            name[0].1.contains("18 characters but the column holds 5"),
            "{}",
            name[0].1
        );
        let fix = p.maps[0].issues[0].fix.clone().unwrap();
        assert_eq!(
            fix,
            "ALTER TABLE \"HR\".\"T\" MODIFY (\"NAME\" VARCHAR2(23 CHAR))"
        );
        assert!(issues(&p, "AMOUNT")[0]
            .1
            .contains("9 digits before the decimal point but NUMBER(8,2) allows 6"));
        assert_eq!(
            p.maps[1].issues[0].fix.as_deref(),
            Some("ALTER TABLE \"HR\".\"T\" MODIFY (\"AMOUNT\" NUMBER(11,2))")
        );
        assert!(issues(&p, "CODE")[0].1.contains("holds text (e.g. 'abc')"));
        assert!(issues(&p, "JOINED")[0]
            .1
            .contains("No date format could be recognised"));
        assert!(
            issues(&p, "QTY")[0].1.contains("5 digits"),
            "{:?}",
            issues(&p, "QTY")
        );
        assert_eq!(p.errors, 5);
        assert!(
            compile(&p, false).is_ok(),
            "compile only checks the plan's shape; validation reports the data problems"
        );
    }

    #[test]
    fn byte_semantics_and_unsupported_types_and_non_insertable_columns() {
        let (p, _) = plan(
            "a,b,c,d\nñandú,1,2,3\n",
            &[
                tgt("A", "BYTES(4)", true),
                tgt("B", "BLOB", true),
                TargetCol {
                    insertable: false,
                    note: Some("virtual column: calculated by Oracle".into()),
                    ..tgt("C", "NUMBER(10)", true)
                },
                tgt("D", "NUMBER(10)", true),
            ],
        );
        assert!(
            issues(&p, "A")[0]
                .1
                .contains("has 7 bytes but the column holds 4"),
            "{:?}",
            issues(&p, "A")
        );
        assert!(issues(&p, "B")[0].1.contains("not supported"));
        assert_eq!(p.maps[2].file_index, None, "virtual column is never mapped");
        assert_eq!(issues(&p, "C")[0].0, Severity::Info);
        assert!(p.maps[3].file_index.is_some());
    }

    #[test]
    fn ambiguity_becomes_a_choice_and_the_choice_is_honoured() {
        let f = file(
            "amb.csv",
            "d,n\n03/04/2020,\"1,234\"\n05/06/2020,\"5,678\"\n",
        );
        let prof = profile(&f);
        let t = [tgt("D", "DATE", true), tgt("N", "NUMBER(10)", true)];
        let p = plan_mapping("HR", "T", &prof, &t, &PlanInput::default());
        assert_eq!(
            p.maps[0]
                .choices
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>(),
            vec!["DD/MM/YYYY", "MM/DD/YYYY"]
        );
        assert_eq!(
            p.maps[0].conversion,
            Conversion::Date {
                family: "DD/MM/YYYY".into()
            }
        );
        assert_eq!(p.warnings, 2);
        let mut input = PlanInput::default();
        input.date_family.insert("D".into(), "MM/DD/YYYY".into());
        input.number_decimal.insert("N".into(), ',');
        let p = plan_mapping("HR", "T", &prof, &t, &input);
        assert_eq!(
            p.maps[0].conversion,
            Conversion::Date {
                family: "MM/DD/YYYY".into()
            }
        );
        assert_eq!(p.maps[1].conversion, Conversion::Number { decimal: ',' });
        let c = compile(&p, false).unwrap();
        assert_eq!(
            c.convert_row(&["03/04/2020".into(), "1,234".into()], false)
                .unwrap(),
            vec![Some("2020-03-04 00:00:00".into()), Some("1.234".into())],
            "March 4th and 1.234, as chosen"
        );
        // a choice that the data does not support is ignored
        let mut bad = PlanInput::default();
        bad.date_family.insert("D".into(), "YYYY-MM-DD".into());
        assert_eq!(
            plan_mapping("HR", "T", &prof, &t, &bad).maps[0].conversion,
            Conversion::Date {
                family: "DD/MM/YYYY".into()
            }
        );
    }

    #[test]
    fn the_insert_uses_explicit_formats_so_nls_settings_cannot_matter() {
        let (p, _) = plan(
            "id,name,amount,born,seen\n1,Ann,1.5,2020-01-31,2020-01-31 10:20:30.5\n",
            &[
                tgt("ID", "NUMBER(10)", false),
                tgt("NAME", "VARCHAR2(30)", true),
                tgt("AMOUNT", "NUMBER(8,2)", true),
                tgt("BORN", "DATE", true),
                tgt("SEEN", "TIMESTAMP(6)", true),
            ],
        );
        let c = compile(&p, false).unwrap();
        assert_eq!(
            c.sql,
            "INSERT INTO \"HR\".\"T\" (\"ID\", \"NAME\", \"AMOUNT\", \"BORN\", \"SEEN\") VALUES (TO_NUMBER(:1, 'TM9', 'NLS_NUMERIC_CHARACTERS=''.,'''), :2, TO_NUMBER(:3, 'TM9', 'NLS_NUMERIC_CHARACTERS=''.,'''), TO_DATE(:4, 'YYYY-MM-DD HH24:MI:SS'), TO_TIMESTAMP(:5, 'YYYY-MM-DD HH24:MI:SS.FF6'))"
        );
        assert!(compile(&p, true)
            .unwrap()
            .sql
            .starts_with("INSERT /*+ APPEND_VALUES */ INTO"));
        let row: Vec<String> = ["1", "Ann", "1.5", "2020-01-31", "2020-01-31 10:20:30.5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            c.convert_row(&row, false).unwrap(),
            vec![
                Some("1".into()),
                Some("Ann".into()),
                Some("1.5".into()),
                Some("2020-01-31 00:00:00".into()),
                Some("2020-01-31 10:20:30.500000".into())
            ]
        );
    }

    #[test]
    fn compile_refuses_hostile_or_broken_plans() {
        let (mut p, _) = plan("id\n1\n", &[tgt("ID", "NUMBER(10)", false)]);
        p.table = "T\" ; DROP TABLE x; --".into();
        assert!(compile(&p, false).is_err(), "table name is validated");
        let (mut p, _) = plan("id\n1\n", &[tgt("ID", "NUMBER(10)", false)]);
        p.maps[0].target.name = "ID\" , \"X".into();
        assert!(compile(&p, false).is_err(), "column name is validated");
        let (mut p, _) = plan("d\n2020-01-01\n", &[tgt("D", "DATE", true)]);
        p.maps[0].conversion = Conversion::Date {
            family: "'; DROP".into(),
        };
        assert!(compile(&p, false).is_err(), "only known date formats");
        let (mut p, _) = plan("id\n1\n", &[tgt("ID", "NUMBER(10)", false)]);
        p.maps[0].file_index = None;
        assert!(compile(&p, false).is_err(), "nothing mapped");
    }

    #[test]
    fn row_conversion_rules() {
        let (p, _) = plan(
            "n,t,d,b,z\n1,a,2020-01-01,yes,007\n",
            &[
                tgt("N", "NUMBER(3)", false),
                tgt("T", "VARCHAR2(5)", true),
                tgt("D", "DATE", true),
                tgt("B", "NUMBER(3)", true),
                tgt("Z", "NUMBER(10)", true),
            ],
        );
        let c = compile(&p, false).unwrap();
        let r = |v: [&str; 5]| {
            c.convert_row(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>(), false)
        };
        assert_eq!(
            r(["1", "a", "2020-01-01", "yes", "007"]).unwrap(),
            vec![
                Some("1".into()),
                Some("a".into()),
                Some("2020-01-01 00:00:00".into()),
                Some("1".into()),
                Some("7".into())
            ],
            "yes -> 1, 007 -> 7"
        );
        assert_eq!(
            r(["2", "", "", "no", ""]).unwrap(),
            vec![Some("2".into()), None, None, Some("0".into()), None],
            "empty cells are NULL"
        );
        let e = |v: [&str; 5]| r(v).unwrap_err().0;
        assert!(e(["", "a", "2020-01-01", "yes", "1"]).contains("required value missing for N"));
        assert!(e(["1234", "a", "2020-01-01", "yes", "1"]).contains("needs 4 digits"));
        assert!(e(["1", "toolong", "2020-01-01", "yes", "1"])
            .contains("value too long for T (7 characters, limit 5)"));
        assert!(e(["1", "a", "31/31/2020", "yes", "1"]).contains("not a valid"));
        assert!(e(["1", "a", "2020-01-01", "maybe", "1"]).contains("not a yes/no value"));
        assert!(e(["abc", "a", "2020-01-01", "yes", "1"]).contains("not a valid number"));
        assert!(c
            .convert_row(&["1".into()], false)
            .unwrap_err()
            .0
            .contains("row has 1 fields"));
    }

    fn fake_table() -> Vec<TargetCol> {
        vec![
            tgt("ID", "NUMBER(10)", false),
            tgt("NAME", "VARCHAR2(30)", true),
            tgt("AMOUNT", "NUMBER(8,2)", true),
        ]
    }
    fn csv_rows(n: usize) -> String {
        let mut s = String::from("id,name,amount\n");
        for i in 1..=n {
            s.push_str(&format!("{i},Name {i},{}.5\n", i));
        }
        s
    }
    fn run_import(db: &FakeDb, csv: &str, opts: ImportOptions) -> (ImportReport, PathBuf) {
        let f = file("load.csv", csv);
        let p = plan_mapping(
            "HR",
            "T",
            &profile(&f),
            &fake_table(),
            &PlanInput::default(),
        );
        let rej = f.with_file_name("load.rejects.csv");
        let rep = import(
            db,
            &mut *open_source(&f, &ReadOptions::default()).unwrap(),
            &p,
            &opts,
            &rej,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        (rep, rej)
    }

    #[test]
    fn loads_in_batches_and_commits_once_at_the_end() {
        let db = FakeDb::new();
        let (r, rej) = run_import(
            &db,
            &csv_rows(25),
            ImportOptions {
                batch_size: 10,
                ..Default::default()
            },
        );
        assert_eq!(
            (r.read, r.inserted, r.rejected, r.committed),
            (25, 25, 0, true)
        );
        assert!(
            r.reject_file.is_none() && !rej.exists(),
            "no reject file when nothing was rejected"
        );
        let log = db.log.lock().unwrap();
        assert_eq!(
            log.batches.iter().map(|b| b.1.len()).collect::<Vec<_>>(),
            vec![10, 10, 5]
        );
        assert_eq!(
            (log.commits, log.rollbacks),
            (1, 0),
            "AtEnd: a single commit"
        );
        assert_eq!(
            log.batches[0].1[0],
            vec![Some("1".into()), Some("Name 1".into()), Some("1.5".into())]
        );
    }

    #[test]
    fn per_batch_commit_mode_commits_each_batch() {
        let db = FakeDb::new();
        run_import(
            &db,
            &csv_rows(25),
            ImportOptions {
                batch_size: 10,
                commit: CommitMode::PerBatch,
                ..Default::default()
            },
        );
        assert_eq!(
            db.log.lock().unwrap().commits,
            4,
            "3 batches + the final commit"
        );
    }

    #[test]
    fn bad_rows_and_database_rejects_go_to_the_reject_file_with_source_row_numbers() {
        let db = FakeDb::new().with_batch_failures(&["3"]);
        let csv = "id,name,amount\n1,Ann,1.5\n2,Bob,not a number\n3,Cy,2.5\n4,Di,9999999999.5\n5,Ed,5.5\n";
        let (r, rej) = run_import(&db, csv, ImportOptions::default());
        assert_eq!((r.read, r.inserted, r.rejected), (5, 2, 3));
        let text = std::fs::read_to_string(rej).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "row,reason,id,name,amount");
        assert!(
            lines[1].starts_with("2,")
                && lines[1].contains("not a valid number")
                && lines[1].ends_with("2,Bob,not a number"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].starts_with("4,") && lines[2].contains("digits before the decimal point"),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].starts_with("3,") && lines[3].contains("ORA-00001"),
            "the database's own rejection is mapped back to source row 3: {}",
            lines[3]
        );
        assert!(r.committed, "the good rows are committed");
    }

    #[test]
    fn stop_on_error_rolls_everything_back_in_one_transaction() {
        let db = FakeDb::new();
        let csv = "id,name,amount\n1,Ann,1.5\n2,Bob,oops\n3,Cy,2.5\n";
        let (r, _) = run_import(
            &db,
            csv,
            ImportOptions {
                on_error: OnError::Stop,
                ..Default::default()
            },
        );
        assert!(r.stopped.as_deref().unwrap().contains("row 2"));
        assert!(r.rolled_back && !r.committed);
        assert_eq!(r.inserted, 0, "all-or-nothing: nothing counts as inserted");
        let log = db.log.lock().unwrap();
        assert_eq!((log.commits, log.rollbacks), (0, 1));
    }

    #[test]
    fn too_many_rejects_abort_the_load() {
        let db = FakeDb::new();
        let mut csv = String::from("id,name,amount\n");
        for _ in 0..30 {
            csv.push_str("x,Bad,1\n");
        }
        let (r, _) = run_import(
            &db,
            &csv,
            ImportOptions {
                max_rejects: 5,
                ..Default::default()
            },
        );
        assert!(r
            .stopped
            .as_deref()
            .unwrap()
            .contains("more than 5 rows were rejected"));
        assert_eq!(r.read, 6, "stops as soon as the limit is crossed");
        assert!(r.rolled_back);
    }

    #[test]
    fn cancel_rolls_back_and_truncate_runs_first() {
        let db = FakeDb::new().with_affected(0);
        let f = file("c.csv", &csv_rows(50));
        let p = plan_mapping(
            "HR",
            "T",
            &profile(&f),
            &fake_table(),
            &PlanInput::default(),
        );
        let cancel = AtomicBool::new(true);
        let rej = f.with_file_name("c.rejects.csv");
        let r = import(
            &db,
            &mut *open_source(&f, &ReadOptions::default()).unwrap(),
            &p,
            &ImportOptions {
                truncate_first: true,
                ..Default::default()
            },
            &rej,
            &cancel,
            &mut |_| {},
        )
        .unwrap();
        assert!(r.cancelled && r.rolled_back && r.inserted == 0);
        let log = db.log.lock().unwrap();
        assert_eq!(
            log.calls[0].0, "TRUNCATE TABLE \"HR\".\"T\"",
            "truncate is explicit and quoted"
        );
        assert!(log.batches.is_empty());
    }

    #[test]
    fn validate_is_a_dry_run_with_per_column_counts() {
        let f = file(
            "v.csv",
            "id,name,amount\n1,Ann,1.5\nx,Bob,2\n3,Cy,zzz\n4,Di,9\n,Ed,1\n",
        );
        let p = plan_mapping(
            "HR",
            "T",
            &profile(&f),
            &fake_table(),
            &PlanInput::default(),
        );
        let rep = validate(
            &mut *open_source(&f, &ReadOptions::default()).unwrap(),
            &p,
            false,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!((rep.rows, rep.ok, rep.rejected), (5, 2, 3));
        assert_eq!(
            rep.by_column,
            vec![("ID".to_string(), 2), ("AMOUNT".to_string(), 1)]
        );
        assert_eq!(rep.samples[0].row, 2);
    }

    #[test]
    fn european_numbers_and_dates_load_correctly() {
        let db = FakeDb::new();
        let f = file(
            "eu.csv",
            "id;name;amount\n1;Ann;\"1.234,56\"\n2;Bob;\"12.345.678,9\"\n",
        );
        let t = vec![
            tgt("ID", "NUMBER(10)", false),
            tgt("NAME", "VARCHAR2(30)", true),
            tgt("AMOUNT", "NUMBER(12,2)", true),
        ];
        let p = plan_mapping("HR", "T", &profile(&f), &t, &PlanInput::default());
        assert_eq!(p.maps[2].conversion, Conversion::Number { decimal: ',' });
        import(
            &db,
            &mut *open_source(&f, &ReadOptions::default()).unwrap(),
            &p,
            &ImportOptions::default(),
            &f.with_extension("rej"),
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let log = db.log.lock().unwrap();
        assert_eq!(log.batches[0].1[0][2], Some("1234.56".into()));
        assert_eq!(log.batches[0].1[1][2], Some("12345678.9".into()));
    }

    #[test]
    fn target_columns_come_from_the_dictionary_with_a_legacy_fallback() {
        let n = None;
        let rows = vec![
            vec![
                Some("ID"),
                Some("NUMBER"),
                Some("22"),
                n,
                n,
                Some("10"),
                Some("0"),
                Some("N"),
                Some("NO"),
            ],
            vec![
                Some("NAME"),
                Some("VARCHAR2"),
                Some("120"),
                Some("30"),
                Some("C"),
                n,
                n,
                Some("Y"),
                Some("NO"),
            ],
            vec![
                Some("TOTAL"),
                Some("NUMBER"),
                Some("22"),
                n,
                n,
                n,
                n,
                Some("Y"),
                Some("YES"),
            ],
            vec![
                Some("SEQ_ID"),
                Some("NUMBER"),
                Some("22"),
                n,
                n,
                Some("10"),
                Some("0"),
                Some("N"),
                Some("NO"),
            ],
        ];
        let tbl = FakeTable::new(&["a"; 9], rows.clone());
        let db = FakeDb::new().with_table("imp:targets", tbl).with_table(
            "imp:identity",
            FakeTable::new(&["c", "g"], vec![vec![Some("SEQ_ID"), Some("ALWAYS")]]),
        );
        let t = load_targets(&db, "HR", "T").unwrap();
        assert_eq!(t[0].display, "NUMBER(10)");
        assert_eq!(t[1].display, "VARCHAR2(30 CHAR)");
        assert!(!t[2].insertable && t[2].note.as_deref().unwrap().contains("virtual"));
        assert!(!t[3].insertable && t[3].note.as_deref().unwrap().contains("identity"));
        // pre-12c: the first query fails, the legacy one is used and identity is optional
        let legacy = FakeDb::new()
            .with_error("hidden_column", "ORA-00904: invalid identifier")
            .with_table("imp:targets", FakeTable::new(&["a"; 9], rows[..2].to_vec()));
        assert_eq!(load_targets(&legacy, "HR", "T").unwrap().len(), 2);
        // unknown table
        assert!(load_targets(
            &FakeDb::new().with_table("imp:targets", FakeTable::new(&["a"; 9], vec![])),
            "HR",
            "NOPE"
        )
        .is_err());
        assert_eq!(
            db.log.lock().unwrap().calls[0].1,
            vec!["HR".to_string(), "T".to_string()],
            "identifiers are bound"
        );
    }

    #[test]
    fn export_then_import_round_trips_values_exactly() {
        use crate::db::{Column, Cursor};
        use crate::export::{export, ExportOptions};
        struct C(Vec<Column>, Option<Vec<Row>>);
        impl Cursor for C {
            fn columns(&self) -> &[Column] {
                &self.0
            }
            fn next_batch(&mut self, _m: usize) -> Result<Vec<Row>> {
                Ok(self.1.take().unwrap_or_default())
            }
        }
        let col = |n: &str, t: &str| Column {
            name: n.into(),
            data_type: t.into(),
            nullable: true,
        };
        let rows: Vec<Row> = vec![
            vec![
                Some("1".into()),
                Some("Zoë, \"Z\"\nline2".into()),
                Some("-5.50".into()),
                Some("2020-02-29 13:45:10".into()),
            ],
            vec![Some("2".into()), None, Some(".5".into()), None],
            vec![
                Some("30".into()),
                Some("=SUM(A1)".into()),
                Some("1000".into()),
                Some("2021-01-01 00:00:00".into()),
            ],
        ];
        let mut c = C(
            vec![
                col("ID", "NUMBER(10)"),
                col("NAME", "VARCHAR2(30)"),
                col("AMOUNT", "NUMBER(8,2)"),
                col("D", "DATE"),
            ],
            Some(rows),
        );
        let dir = file("seed.txt", "").parent().unwrap().to_path_buf();
        let rep = export(
            &mut c,
            &dir.join("rt"),
            &ExportOptions {
                sanitize_formulas: false,
                ..Default::default()
            },
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let f = PathBuf::from(&rep.files[0]);
        let t = vec![
            tgt("ID", "NUMBER(10)", false),
            tgt("NAME", "VARCHAR2(30)", true),
            tgt("AMOUNT", "NUMBER(8,2)", true),
            tgt("D", "DATE", true),
        ];
        let p = plan_mapping("HR", "T", &profile(&f), &t, &PlanInput::default());
        assert_eq!(
            p.errors,
            0,
            "{:?}",
            p.maps.iter().map(|m| &m.issues).collect::<Vec<_>>()
        );
        let db = FakeDb::new();
        let r = import(
            &db,
            &mut *open_source(&f, &ReadOptions::default()).unwrap(),
            &p,
            &ImportOptions::default(),
            &f.with_extension("rej"),
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!((r.inserted, r.rejected), (3, 0), "{:?}", r.samples);
        let got = &db.log.lock().unwrap().batches[0].1;
        assert_eq!(
            got[0],
            vec![
                Some("1".into()),
                Some("Zoë, \"Z\"\nline2".into()),
                Some("-5.50".into()),
                Some("2020-02-29 13:45:10".into())
            ]
        );
        assert_eq!(
            got[1],
            vec![Some("2".into()), None, Some("0.5".into()), None],
            ".5 becomes 0.5, NULLs stay NULL"
        );
        assert_eq!(got[2][1], Some("=SUM(A1)".into()));
    }

    #[test]
    #[ignore]
    fn import_throughput() {
        let n = 500_000;
        let f = file("big.csv", &csv_rows(n));
        let t = Instant::now();
        let prof = profile(&f);
        let t_profile = t.elapsed();
        let p = plan_mapping("HR", "T", &prof, &fake_table(), &PlanInput::default());
        let db = FakeDb::new();
        let t = Instant::now();
        let r = import(
            &db,
            &mut *open_source(&f, &ReadOptions::default()).unwrap(),
            &p,
            &ImportOptions::default(),
            &f.with_extension("rej"),
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        println!("profile {n} rows: {:?}; convert+batch {} rows: {:?} = {} rows/s (excluding the database)", t_profile, r.read, t.elapsed(), r.rows_per_sec);
    }
}
