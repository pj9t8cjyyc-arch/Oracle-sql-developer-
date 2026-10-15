//! Rules for the in-app updater that are worth testing without a window: which update address is acceptable.

use crate::{Error, Result};

/// An update feed address typed by an administrator or the person using the app.
/// `https://` only (an internal server must still use TLS); plain `http://` is accepted for localhost
/// so a feed can be tried on the same machine. No credentials in the URL, no spaces, no fragments.
pub fn normalize_endpoint(raw: &str) -> Result<String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(Error::Invalid("the update address is empty".into()));
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Invalid(
            "the update address must not contain spaces or control characters".into(),
        ));
    }
    let (scheme, rest) = s
        .split_once("://")
        .ok_or_else(|| Error::Invalid("the update address must start with https://".into()))?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(Error::Invalid(
            "the update address must not contain a user name or password".into(),
        ));
    }
    if s.contains('#') {
        return Err(Error::Invalid(
            "the update address must not contain a # fragment".into(),
        ));
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            authority
        }
    });
    if host.is_empty() {
        return Err(Error::Invalid("the update address has no host name".into()));
    }
    let local = matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    );
    match scheme.as_str() {
        "https" => {}
        "http" if local => {}
        "http" => {
            return Err(Error::Invalid(
                "the update address must use https:// (http:// is allowed for localhost only)"
                    .into(),
            ))
        }
        other => {
            return Err(Error::Invalid(format!(
                "the update address cannot use {other}://"
            )))
        }
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_feeds_are_accepted() {
        for ok in [
            "https://updates.corp.example/ora-ide/latest.json",
            "  https://github.com/o/r/releases/latest/download/latest.json ",
            "https://host:8443/x?channel=stable",
            "http://localhost:8080/latest.json",
            "http://127.0.0.1/latest.json",
            "HTTPS://Host/x",
        ] {
            assert!(normalize_endpoint(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            normalize_endpoint("  https://a.b/c ").unwrap(),
            "https://a.b/c"
        );
    }

    #[test]
    fn unsafe_or_malformed_feeds_are_refused() {
        for bad in [
            "",
            "   ",
            "latest.json",
            "http://updates.corp.example/latest.json",
            "ftp://host/latest.json",
            "file:///etc/passwd",
            "https://user:pw@host/latest.json",
            "https://host/a b",
            "https://host/x#frag",
            "https:///path-only",
            "https://",
            "http://localhost.evil.example/latest.json",
        ] {
            assert!(normalize_endpoint(bad).is_err(), "{bad}");
        }
    }
}
