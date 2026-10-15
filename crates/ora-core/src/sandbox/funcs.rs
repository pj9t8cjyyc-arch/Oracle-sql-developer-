//! Oracle functions people use every day, registered in SQLite so most queries run unchanged.
//! Dates are ISO text (`YYYY-MM-DD HH:MM:SS`), which is how the clone stores them.

use chrono::{Datelike, Months, NaiveDate, NaiveDateTime};
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, Error as SqlError, Result as SqlResult};

fn owned(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Integer(i),
        ValueRef::Real(f) => Value::Real(f),
        ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    }
}

fn arg(ctx: &Context<'_>, i: usize) -> Value {
    owned(ctx.get_raw(i))
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(i.to_string()),
        Value::Real(f) => Some(num_text(*f)),
        Value::Text(s) => Some(s.clone()),
        Value::Blob(b) => Some(format!("({} bytes)", b.len())),
    }
}

fn num_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

fn num_of(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(f) => Some(*f),
        Value::Text(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn num_value(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        Value::Integer(f as i64)
    } else {
        Value::Real(f)
    }
}

/// Equality the way DECODE sees it: numbers by value, everything else by text, NULL equals NULL.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        _ => match (num_of(a), num_of(b)) {
            (Some(x), Some(y)) if !matches!(a, Value::Text(_)) || !matches!(b, Value::Text(_)) => {
                x == y
            }
            _ => text_of(a) == text_of(b),
        },
    }
}

fn parse_dt(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(s, f) {
            return Some(d);
        }
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return d.and_hms_opt(0, 0, 0);
        }
    }
    None
}

fn iso(d: NaiveDateTime) -> String {
    d.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Oracle date format elements -> chrono. Unknown elements are an error, never a silent guess.
fn chrono_format(fmt: &str) -> Result<String, String> {
    let f = fmt.to_uppercase();
    let mut out = String::new();
    let mut i = 0;
    let b: Vec<char> = f.chars().collect();
    let starts = |i: usize, p: &str| b[i..].iter().collect::<String>().starts_with(p);
    while i < b.len() {
        let (rep, n) = if starts(i, "YYYY") {
            ("%Y", 4)
        } else if starts(i, "HH24") {
            ("%H", 4)
        } else if starts(i, "MON") {
            ("%b", 3)
        } else if starts(i, "YY") {
            ("%y", 2)
        } else if starts(i, "MM") {
            ("%m", 2)
        } else if starts(i, "DD") {
            ("%d", 2)
        } else if starts(i, "HH") {
            ("%I", 2)
        } else if starts(i, "MI") {
            ("%M", 2)
        } else if starts(i, "SS") {
            ("%S", 2)
        } else if matches!(b[i], '-' | '/' | ':' | ' ' | '.' | ',' | 'T') {
            out.push(b[i]);
            i += 1;
            continue;
        } else {
            return Err(format!(
                "date format element at {:?} is not supported in the sandbox",
                &fmt[i.min(fmt.len())..]
            ));
        };
        out.push_str(rep);
        i += n;
    }
    Ok(out)
}

fn err(msg: impl Into<String>) -> SqlError {
    SqlError::UserFunctionError(msg.into().into())
}

pub fn register(c: &Connection) -> SqlResult<()> {
    let det = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    c.create_scalar_function("NVL", 2, det, |ctx| {
        let a = arg(ctx, 0);
        Ok(if matches!(a, Value::Null) {
            arg(ctx, 1)
        } else {
            a
        })
    })?;
    c.create_scalar_function("NVL2", 3, det, |ctx| {
        Ok(if matches!(arg(ctx, 0), Value::Null) {
            arg(ctx, 2)
        } else {
            arg(ctx, 1)
        })
    })?;
    c.create_scalar_function("DECODE", -1, det, |ctx| {
        let n = ctx.len();
        if n < 3 {
            return Err(err(
                "DECODE needs an expression and at least one search/result pair",
            ));
        }
        let e = arg(ctx, 0);
        let mut i = 1;
        while i + 1 < n {
            if same(&e, &arg(ctx, i)) {
                return Ok(arg(ctx, i + 1));
            }
            i += 2;
        }
        Ok(if i < n { arg(ctx, i) } else { Value::Null })
    })?;
    c.create_scalar_function("TO_NUMBER", -1, det, |ctx| {
        let v = arg(ctx, 0);
        match &v {
            Value::Null => Ok(Value::Null),
            Value::Integer(_) | Value::Real(_) => Ok(v),
            Value::Text(s) => s
                .trim()
                .replace(',', "")
                .parse::<f64>()
                .map(num_value)
                .map_err(|_| err(format!("ORA-01722: invalid number: {s:?}"))),
            Value::Blob(_) => Err(err("ORA-01722: invalid number")),
        }
    })?;
    c.create_scalar_function("TO_DATE", -1, det, |ctx| {
        let Some(s) = text_of(&arg(ctx, 0)) else {
            return Ok(Value::Null);
        };
        let d = if ctx.len() > 1 {
            let fmt = text_of(&arg(ctx, 1)).unwrap_or_default();
            let cf = chrono_format(&fmt).map_err(err)?;
            NaiveDateTime::parse_from_str(&s, &cf).ok().or_else(|| {
                NaiveDate::parse_from_str(&s, &cf)
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
            })
        } else {
            parse_dt(&s)
        };
        d.map(|d| Value::Text(iso(d))).ok_or_else(|| {
            err(format!(
                "ORA-01861: literal {s:?} does not match the date format"
            ))
        })
    })?;
    c.create_scalar_function("TO_CHAR", -1, det, |ctx| {
        let v = arg(ctx, 0);
        if matches!(v, Value::Null) {
            return Ok(Value::Null);
        }
        if ctx.len() > 1 {
            if let (Some(s), Some(fmt)) = (text_of(&v), text_of(&arg(ctx, 1))) {
                if let Some(d) = parse_dt(&s) {
                    let cf = chrono_format(&fmt).map_err(err)?;
                    return Ok(Value::Text(
                        d.format(&cf).to_string().to_uppercase_if_mon(&fmt),
                    ));
                }
            }
        }
        Ok(Value::Text(text_of(&v).unwrap_or_default()))
    })?;
    c.create_scalar_function("TRUNC", -1, det, |ctx| {
        let v = arg(ctx, 0);
        match &v {
            Value::Null => Ok(Value::Null),
            Value::Text(s) if parse_dt(s).is_some() && ctx.len() == 1 => {
                Ok(Value::Text(iso(parse_dt(s)
                    .unwrap()
                    .date()
                    .and_hms_opt(0, 0, 0)
                    .unwrap())))
            }
            _ => {
                let x = num_of(&v).ok_or_else(|| err("TRUNC expects a number or a date"))?;
                let n = if ctx.len() > 1 {
                    num_of(&arg(ctx, 1)).unwrap_or(0.0) as i32
                } else {
                    0
                };
                let m = 10f64.powi(n);
                Ok(num_value((x * m).trunc() / m))
            }
        }
    })?;
    for (name, f) in [
        ("CEIL", f64::ceil as fn(f64) -> f64),
        ("FLOOR", f64::floor),
        ("SQRT", f64::sqrt),
    ] {
        c.create_scalar_function(name, 1, det, move |ctx| {
            Ok(num_of(&arg(ctx, 0)).map_or(Value::Null, |x| num_value(f(x))))
        })?;
    }
    c.create_scalar_function("POWER", 2, det, |ctx| {
        Ok(match (num_of(&arg(ctx, 0)), num_of(&arg(ctx, 1))) {
            (Some(a), Some(b)) => num_value(a.powf(b)),
            _ => Value::Null,
        })
    })?;
    c.create_scalar_function("MOD", 2, det, |ctx| {
        Ok(match (num_of(&arg(ctx, 0)), num_of(&arg(ctx, 1))) {
            (Some(a), Some(0.0)) => num_value(a),
            (Some(a), Some(b)) => num_value(a % b),
            _ => Value::Null,
        })
    })?;
    for (name, left) in [("LPAD", true), ("RPAD", false)] {
        c.create_scalar_function(name, -1, det, move |ctx| {
            let Some(s) = text_of(&arg(ctx, 0)) else {
                return Ok(Value::Null);
            };
            let n = num_of(&arg(ctx, 1)).unwrap_or(0.0).max(0.0) as usize;
            let pad = if ctx.len() > 2 {
                text_of(&arg(ctx, 2)).unwrap_or_default()
            } else {
                " ".into()
            };
            let len = s.chars().count();
            if len >= n {
                return Ok(Value::Text(s.chars().take(n).collect()));
            }
            if pad.is_empty() {
                return Ok(Value::Text(s));
            }
            let fill: String = pad.chars().cycle().take(n - len).collect();
            Ok(Value::Text(if left { fill + &s } else { s + &fill }))
        })?;
    }
    c.create_scalar_function("INITCAP", 1, det, |ctx| {
        Ok(match text_of(&arg(ctx, 0)) {
            None => Value::Null,
            Some(s) => {
                let mut up = true;
                Value::Text(
                    s.chars()
                        .map(|ch| {
                            let r = if up {
                                ch.to_uppercase().next().unwrap_or(ch)
                            } else {
                                ch.to_lowercase().next().unwrap_or(ch)
                            };
                            up = !ch.is_alphanumeric();
                            r
                        })
                        .collect(),
                )
            }
        })
    })?;
    c.create_scalar_function("ADD_MONTHS", 2, det, |ctx| {
        let (Some(s), Some(n)) = (text_of(&arg(ctx, 0)), num_of(&arg(ctx, 1))) else {
            return Ok(Value::Null);
        };
        let d = parse_dt(&s).ok_or_else(|| err(format!("ORA-01861: {s:?} is not a date")))?;
        let n = n as i32;
        let r = if n >= 0 {
            d.checked_add_months(Months::new(n as u32))
        } else {
            d.checked_sub_months(Months::new(n.unsigned_abs()))
        };
        r.map(|d| Value::Text(iso(d)))
            .ok_or_else(|| err("date out of range"))
    })?;
    c.create_scalar_function("LAST_DAY", 1, det, |ctx| {
        let Some(s) = text_of(&arg(ctx, 0)) else {
            return Ok(Value::Null);
        };
        let d = parse_dt(&s).ok_or_else(|| err(format!("ORA-01861: {s:?} is not a date")))?;
        let first = NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap();
        let last = first
            .checked_add_months(Months::new(1))
            .unwrap()
            .pred_opt()
            .unwrap();
        Ok(Value::Text(iso(last.and_time(d.time()))))
    })?;
    Ok(())
}

trait MonCase {
    fn to_uppercase_if_mon(self, fmt: &str) -> String;
}
impl MonCase for String {
    /// Oracle prints `MON` as JAN when the format is upper case.
    fn to_uppercase_if_mon(self, fmt: &str) -> String {
        if fmt.contains("MON") {
            self.to_uppercase()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(sql: &str) -> String {
        let c = Connection::open_in_memory().unwrap();
        register(&c).unwrap();
        c.query_row(sql, [], |r| {
            Ok(text_of(&owned(r.get_ref(0)?)).unwrap_or_else(|| "NULL".into()))
        })
        .unwrap()
    }

    #[test]
    fn nvl_nvl2_and_decode() {
        assert_eq!(q("select nvl(null, 5)"), "5");
        assert_eq!(q("select nvl(3, 5)"), "3");
        assert_eq!(q("select nvl2(null, 'a', 'b')"), "b");
        assert_eq!(q("select nvl2(1, 'a', 'b')"), "a");
        assert_eq!(q("select decode(2, 1, 'one', 2, 'two', 'many')"), "two");
        assert_eq!(q("select decode(9, 1, 'one', 'many')"), "many");
        assert_eq!(q("select decode(9, 1, 'one')"), "NULL");
        assert_eq!(
            q("select decode(null, null, 'both null', 'x')"),
            "both null"
        );
        assert_eq!(q("select decode(1.0, 1, 'same')"), "same");
    }

    #[test]
    fn dates_convert_and_format() {
        assert_eq!(
            q("select to_date('2021-03-04', 'YYYY-MM-DD')"),
            "2021-03-04 00:00:00"
        );
        assert_eq!(
            q("select to_date('04/03/2021 10:30', 'DD/MM/YYYY HH24:MI')"),
            "2021-03-04 10:30:00"
        );
        assert_eq!(
            q("select to_char('2021-03-04 10:30:00', 'DD-MON-YYYY')"),
            "04-MAR-2021"
        );
        assert_eq!(
            q("select to_char('2021-03-04 10:30:00', 'YYYY/MM/DD HH24:MI:SS')"),
            "2021/03/04 10:30:00"
        );
        assert_eq!(q("select to_char(42)"), "42");
        assert_eq!(
            q("select trunc('2021-03-04 10:30:00')"),
            "2021-03-04 00:00:00"
        );
        assert_eq!(
            q("select add_months('2021-01-31', 1)"),
            "2021-02-28 00:00:00"
        );
        assert_eq!(q("select last_day('2021-02-10')"), "2021-02-28 00:00:00");
    }

    #[test]
    fn numbers_and_strings() {
        assert_eq!(q("select trunc(3.789, 1)"), "3.7");
        assert_eq!(q("select trunc(-3.7)"), "-3");
        assert_eq!(q("select ceil(1.2) || '/' || floor(1.8)"), "2/1");
        assert_eq!(q("select power(2, 10)"), "1024");
        assert_eq!(q("select mod(10, 3)"), "1");
        assert_eq!(q("select to_number('1,234.5')"), "1234.5");
        assert_eq!(q("select lpad('7', 3, '0')"), "007");
        assert_eq!(q("select rpad('ab', 5, 'xy')"), "abxyx");
        assert_eq!(q("select lpad('abcdef', 3)"), "abc");
        assert_eq!(q("select initcap('hello wORLD')"), "Hello World");
    }

    #[test]
    fn bad_input_is_an_error_not_a_guess() {
        let c = Connection::open_in_memory().unwrap();
        register(&c).unwrap();
        for sql in [
            "select to_number('abc')",
            "select to_date('nonsense')",
            "select to_date('2021', 'YYYY-Q')",
            "select decode(1)",
        ] {
            assert!(c.query_row(sql, [], |_| Ok(())).is_err(), "{sql}");
        }
    }
}
