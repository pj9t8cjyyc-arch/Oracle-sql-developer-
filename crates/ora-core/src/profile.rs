//! Connection profiles. Passwords are never part of a profile: they are typed (or come from a
//! wallet) and kept in memory only.
//!
//! A profile can describe a connection in three ways: host + port + service (Easy Connect, with
//! optional TLS), a `tnsnames.ora` alias, or a full connect string / descriptor; and it can
//! authenticate with a password, an external wallet / OS identity, or through a proxy user.

use serde::{Deserialize, Serialize};

use crate::{tns, Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    #[default]
    Dev,
    Test,
    Prod,
}

/// How the database is addressed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnKind {
    /// `host:port/service`, optionally over TLS.
    #[default]
    Easy,
    /// An alias from `tnsnames.ora`.
    Tns,
    /// A pasted Easy Connect string or full `(DESCRIPTION=…)`.
    ConnectString,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    #[default]
    Password,
    /// Credentials come from an Oracle wallet or the operating system: no user or password here.
    External,
    /// `user[target]`: sign in as `user`, work as `proxy_target`.
    Proxy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Privilege {
    #[default]
    None,
    Sysdba,
    Sysoper,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub name: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub kind: ConnKind,
    /// Easy Connect over TLS (`tcps://`).
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub tns_alias: String,
    /// Folder (or file) with `tnsnames.ora`; also used as `TNS_ADMIN` so a `sqlnet.ora` / wallet there is found.
    #[serde(default)]
    pub tns_admin: String,
    #[serde(default)]
    pub connect_string: String,
    /// Folder with a TLS wallet (`ewallet.p12` / `cwallet.sso`), added to the descriptor when it has none.
    #[serde(default)]
    pub wallet_dir: String,
    #[serde(default)]
    pub auth: AuthKind,
    #[serde(default)]
    pub proxy_target: String,
    #[serde(default)]
    pub privilege: Privilege,
    #[serde(default = "default_timeout")]
    pub connect_timeout_secs: u64,
}

fn default_port() -> u16 {
    1521
}
fn default_timeout() -> u64 {
    20
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

impl ConnectionProfile {
    /// A profile for the classic host / port / service form.
    pub fn easy(name: &str, host: &str, port: u16, service: &str, user: &str) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            port,
            service: service.into(),
            user: user.into(),
            environment: Environment::Dev,
            kind: ConnKind::Easy,
            tls: false,
            tns_alias: String::new(),
            tns_admin: String::new(),
            connect_string: String::new(),
            wallet_dir: String::new(),
            auth: AuthKind::Password,
            proxy_target: String::new(),
            privilege: Privilege::None,
            connect_timeout_secs: default_timeout(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let need = |label: &str, v: &str| -> Result<()> {
            if v.trim().is_empty() {
                Err(Error::Invalid(format!("{label} is required")))
            } else {
                Ok(())
            }
        };
        need("name", &self.name)?;
        match self.kind {
            ConnKind::Easy => {
                need("host", &self.host)?;
                need("service", &self.service)?;
                // The connect string is built by concatenation: reject characters that could alter its structure.
                for v in [&self.host, &self.service] {
                    if v.chars().any(|c| {
                        matches!(
                            c,
                            '/' | ':' | '(' | ')' | '=' | ' ' | '\'' | '"' | '?' | '&'
                        ) || c.is_control()
                    }) {
                        return Err(Error::Invalid(format!("illegal character in '{v}'")));
                    }
                }
                if self.port == 0 {
                    return Err(Error::Invalid(
                        "the port must be between 1 and 65535".into(),
                    ));
                }
            }
            ConnKind::Tns => {
                need("TNS alias", &self.tns_alias)?;
                if has_control(&self.tns_alias) || self.tns_alias.contains(['(', ')', '=']) {
                    return Err(Error::Invalid("the TNS alias is not valid".into()));
                }
            }
            ConnKind::ConnectString => {
                need("connect string", &self.connect_string)?;
                if has_control(&self.connect_string) {
                    return Err(Error::Invalid(
                        "the connect string must be on one line without control characters".into(),
                    ));
                }
            }
        }
        if has_control(&self.tns_admin) || has_control(&self.wallet_dir) {
            return Err(Error::Invalid(
                "a folder path contains control characters".into(),
            ));
        }
        match self.auth {
            AuthKind::External => {}
            AuthKind::Password => need("user", &self.user)?,
            AuthKind::Proxy => {
                need("user", &self.user)?;
                need("proxy target user", &self.proxy_target)?;
                if self.proxy_target.contains(['[', ']']) || self.user.contains(['[', ']']) {
                    return Err(Error::Invalid(
                        "brackets are not allowed in user names".into(),
                    ));
                }
            }
        }
        if self.auth == AuthKind::External && self.privilege != Privilege::None {
            return Err(Error::Invalid(
                "SYSDBA/SYSOPER cannot be combined with wallet / OS authentication here".into(),
            ));
        }
        if self.connect_timeout_secs == 0 || self.connect_timeout_secs > 600 {
            return Err(Error::Invalid(
                "the connect timeout must be between 1 and 600 seconds".into(),
            ));
        }
        Ok(())
    }

    /// What to pass to the Oracle client as the connect string. For a TNS alias the entry is read
    /// from `tnsnames.ora` here, so it works even when the client would not find the file itself.
    pub fn resolved_connect_string(&self) -> Result<String> {
        let cs = match self.kind {
            ConnKind::Easy => format!(
                "{}{}:{}/{}",
                if self.tls { "tcps://" } else { "//" },
                self.host,
                self.port,
                self.service
            ),
            ConnKind::ConnectString => self.connect_string.trim().to_string(),
            ConnKind::Tns => {
                let (file, entries) = tns::load(&self.tns_admin)?;
                let e = tns::find_alias(&entries, &self.tns_alias).ok_or_else(|| {
                    let known: Vec<&str> = entries
                        .iter()
                        .flat_map(|e| e.names.iter().map(String::as_str))
                        .take(12)
                        .collect();
                    Error::Invalid(format!(
                        "alias {:?} is not in {} (found: {})",
                        self.tns_alias,
                        file.display(),
                        if known.is_empty() {
                            "no entries".into()
                        } else {
                            known.join(", ")
                        }
                    ))
                })?;
                e.descriptor.clone()
            }
        };
        Ok(self.with_wallet(cs))
    }

    /// Adds `(SECURITY=(MY_WALLET_DIRECTORY=…))` to a descriptor that names no wallet.
    fn with_wallet(&self, cs: String) -> String {
        let dir = self.wallet_dir.trim();
        if dir.is_empty()
            || !cs.trim_start().starts_with('(')
            || cs.to_uppercase().contains("MY_WALLET_DIRECTORY")
        {
            return cs;
        }
        match cs.trim_end().strip_suffix(')') {
            Some(head) => format!("{head}(SECURITY=(MY_WALLET_DIRECTORY={dir})))"),
            None => cs,
        }
    }

    /// The user name handed to the client: empty for external authentication, `user[target]` for a proxy.
    pub fn login_user(&self) -> String {
        match self.auth {
            AuthKind::External => String::new(),
            AuthKind::Proxy => format!("{}[{}]", self.user.trim(), self.proxy_target.trim()),
            AuthKind::Password => self.user.trim().to_string(),
        }
    }

    /// Stable identity of *which database as which user*: the same across sessions, unlike a
    /// connection id. The classic host/port/service form keeps the key it always had, so existing
    /// history and search indexes stay attached.
    pub fn key(&self) -> String {
        let who = match self.auth {
            AuthKind::External => "external".to_string(),
            AuthKind::Proxy => format!("{}[{}]", self.user, self.proxy_target),
            AuthKind::Password => self.user.clone(),
        };
        let id = match (self.kind, self.tls, self.auth) {
            (ConnKind::Easy, false, AuthKind::Password) => {
                format!("{}@{}:{}/{}", self.user, self.host, self.port, self.service)
            }
            (ConnKind::Easy, tls, _) => format!(
                "{who}@{}{}:{}/{}",
                if tls { "tcps:" } else { "" },
                self.host,
                self.port,
                self.service
            ),
            (ConnKind::Tns, _, _) => format!("{who}@tns:{}", self.tns_alias),
            (ConnKind::ConnectString, _, _) => format!(
                "{who}@cs:{}",
                self.connect_string.split_whitespace().collect::<String>()
            ),
        }
        .to_lowercase();
        let mut h: u64 = 0xcbf29ce484222325;
        for b in id.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        format!("{h:016x}")
    }

    /// Short text for lists and tooltips; never contains a password.
    pub fn describe(&self) -> String {
        match self.kind {
            ConnKind::Easy => format!(
                "{}:{}/{}{}",
                self.host,
                self.port,
                self.service,
                if self.tls { " (TLS)" } else { "" }
            ),
            ConnKind::Tns => format!("TNS alias {}", self.tns_alias),
            ConnKind::ConnectString => "connect string".into(),
        }
    }
}

/// A plain-language next step for the connection errors people hit most. The original message is
/// always kept; this only adds what to check.
pub fn explain_connect_error(msg: &str) -> Option<&'static str> {
    let m = msg.to_uppercase();
    let has = |c: &str| m.contains(c);
    Some(
        if has("DPI-1047") || has("CANNOT LOCATE A 64-BIT ORACLE CLIENT") || has("DPI-1072") {
            "Oracle Instant Client was not found (or is the wrong version/bitness). Install the 64-bit Instant Client Basic package and add its folder to PATH (Windows), LD_LIBRARY_PATH (Linux) or install it into ~/lib (macOS)."
        } else if has("ORA-12541") || has("TNS-12541") {
            "Nothing is listening at that host and port: check the host name, the port (usually 1521, or 2484 for TLS) and that the listener is running. A firewall or VPN is a common cause."
        } else if has("ORA-12514") || has("ORA-12505") || has("ORA-12516") {
            "The listener does not know that service name. Use the SERVICE name (not the SID) of the pluggable database, for example from `lsnrctl status` or your DBA."
        } else if has("ORA-12154") || has("ORA-12262") || has("TNS-12154") {
            "The alias could not be resolved. Check the alias spelling and that the folder with tnsnames.ora is the one chosen in the connection."
        } else if has("ORA-12170") || has("ORA-12535") || has("TNS-12535") || has("TIMED OUT") {
            "The connection timed out. The host may be unreachable (VPN, firewall) or the port blocked."
        } else if has("ORA-01017") {
            "The user name or password is not accepted. Passwords are case-sensitive; also check that you are using the right database."
        } else if has("ORA-28000") {
            "The account is locked. Ask a DBA to unlock it."
        } else if has("ORA-28001") {
            "The password has expired. Change it (for example with ALTER USER … IDENTIFIED BY … or ask a DBA) and connect again."
        } else if has("ORA-28759") || has("ORA-28858") || has("ORA-17002") && has("WALLET") {
            "The wallet could not be opened. Check the wallet folder, that it contains cwallet.sso or ewallet.p12, and that sqlnet.ora (WALLET_LOCATION) points to it."
        } else if has("ORA-29024") || has("ORA-29106") || has("CERTIFICATE") {
            "TLS certificate validation failed. The server certificate must be trusted by the wallet, and the name in the certificate must match the host (check SSL_SERVER_DN_MATCH)."
        } else if has("ORA-01031") {
            "Insufficient privileges for this action or for the requested role (SYSDBA/SYSOPER)."
        } else if has("ORA-12560") || has("ORA-12547") {
            "The Oracle client could not reach a local database or its configuration is incomplete: for a remote database give a host and service or a TNS alias."
        } else if has("ORA-01005") {
            "No password was supplied. Enter one, or choose wallet / OS authentication."
        } else {
            return None;
        },
    )
}

/// `message`, plus the next step from [`explain_connect_error`] when there is one.
pub fn with_hint(message: &str) -> String {
    match explain_connect_error(message) {
        Some(h) => format!("{message}\n→ {h}"),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn easy() -> ConnectionProfile {
        ConnectionProfile::easy("Dev", "db.corp", 1521, "FREEPDB1", "hr")
    }

    #[test]
    fn the_classic_profile_builds_the_same_connect_string_and_keeps_its_key() {
        let p = easy();
        assert_eq!(
            p.resolved_connect_string().unwrap(),
            "//db.corp:1521/FREEPDB1"
        );
        // This value was produced by the previous implementation: history and indexes depend on it.
        let mut old: u64 = 0xcbf29ce484222325;
        for b in "hr@db.corp:1521/freepdb1".bytes() {
            old ^= b as u64;
            old = old.wrapping_mul(0x100000001b3);
        }
        assert_eq!(p.key(), format!("{old:016x}"));
        // And profiles written before these options existed still load.
        let json = r#"{"name":"Old","host":"h","port":1521,"service":"s","user":"u","environment":"prod"}"#;
        let loaded: ConnectionProfile = serde_json::from_str(json).unwrap();
        assert_eq!(
            (
                loaded.kind,
                loaded.auth,
                loaded.privilege,
                loaded.connect_timeout_secs
            ),
            (ConnKind::Easy, AuthKind::Password, Privilege::None, 20)
        );
        assert!(loaded.validate().is_ok());
    }

    #[test]
    fn tls_tns_and_free_form_connect_strings() {
        let mut p = easy();
        p.tls = true;
        p.port = 2484;
        assert_eq!(
            p.resolved_connect_string().unwrap(),
            "tcps://db.corp:2484/FREEPDB1"
        );
        assert_ne!(
            p.key(),
            easy().key(),
            "TLS is a different connection identity"
        );

        let mut c = easy();
        c.kind = ConnKind::ConnectString;
        c.connect_string = "  (DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=h)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=s)))  ".into();
        assert!(c
            .resolved_connect_string()
            .unwrap()
            .starts_with("(DESCRIPTION="));
        c.wallet_dir = "/opt/wallet".into();
        let with = c.resolved_connect_string().unwrap();
        assert!(
            with.ends_with("(SECURITY=(MY_WALLET_DIRECTORY=/opt/wallet)))")
                && with.matches("(DESCRIPTION=").count() == 1,
            "{with}"
        );
        c.connect_string = "host:1521/svc?retry_count=3".into();
        assert_eq!(
            c.resolved_connect_string().unwrap(),
            "host:1521/svc?retry_count=3",
            "the wallet is only injected into descriptors"
        );
        c.connect_string = "a\nb".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn a_tns_alias_is_resolved_from_tnsnames_ora() {
        let dir = std::env::temp_dir().join(format!("ora-ide-prof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tnsnames.ora"), "PROD=(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=p)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=prod)))\n").unwrap();
        let mut p = easy();
        p.kind = ConnKind::Tns;
        p.tns_alias = "prod".into();
        p.tns_admin = dir.display().to_string();
        assert!(p.resolved_connect_string().unwrap().contains("(HOST=p)"));
        p.tns_alias = "missing".into();
        let e = p.resolved_connect_string().unwrap_err().to_string();
        assert!(
            e.contains("\"missing\"") && e.contains("PROD"),
            "the error names what exists: {e}"
        );
        assert_ne!(p.key(), easy().key());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn authentication_modes_decide_the_login_name() {
        let mut p = easy();
        assert_eq!(p.login_user(), "hr");
        p.auth = AuthKind::Proxy;
        p.proxy_target = "app_owner".into();
        assert_eq!(p.login_user(), "hr[app_owner]");
        p.auth = AuthKind::External;
        p.user = String::new();
        assert_eq!(p.login_user(), "");
        assert!(
            p.validate().is_ok(),
            "no user is needed with a wallet / OS login"
        );
        p.privilege = Privilege::Sysdba;
        assert!(p.validate().unwrap_err().to_string().contains("SYSDBA"));
        let keys: std::collections::HashSet<String> =
            [AuthKind::Password, AuthKind::External, AuthKind::Proxy]
                .iter()
                .map(|a| {
                    ConnectionProfile {
                        auth: *a,
                        user: "hr".into(),
                        proxy_target: "x".into(),
                        privilege: Privilege::None,
                        ..easy()
                    }
                    .key()
                })
                .collect();
        assert_eq!(keys.len(), 3, "each way of signing in has its own history");
    }

    #[test]
    fn validation_refuses_what_could_change_the_connect_string() {
        for bad_host in ["h:1", "h/x", "h)(", "h ", "h?x=1", "h&x"] {
            let mut p = easy();
            p.host = bad_host.into();
            assert!(p.validate().is_err(), "{bad_host}");
        }
        let mut p = easy();
        p.service = "s)(HOST=evil".into();
        assert!(p.validate().is_err());
        let mut p = easy();
        p.kind = ConnKind::Tns;
        p.tns_alias = "A)(B".into();
        assert!(p.validate().is_err());
        let mut p = easy();
        p.user = "hr]".into();
        p.auth = AuthKind::Proxy;
        p.proxy_target = "t".into();
        assert!(p.validate().is_err());
        let mut p = easy();
        p.port = 0;
        assert!(p.validate().is_err());
        p = easy();
        p.connect_timeout_secs = 0;
        assert!(p.validate().is_err());
        assert!(ConnectionProfile::easy("", "h", 1, "s", "u")
            .validate()
            .is_err());
    }

    #[test]
    fn common_connection_errors_get_a_plain_next_step() {
        let hint = |m: &str| explain_connect_error(m).unwrap_or("");
        assert!(hint("ORA-12541: TNS:no listener").contains("listening"));
        assert!(
            hint("ORA-12514: listener does not currently know of service").contains("SERVICE name")
        );
        assert!(hint("ORA-12154: TNS:could not resolve the connect identifier").contains("alias"));
        assert!(hint("ORA-01017: invalid username/password; logon denied").contains("password"));
        assert!(
            hint("DPI-1047: Cannot locate a 64-bit Oracle Client library")
                .contains("Instant Client")
        );
        assert!(hint("ORA-28000: the account is locked").contains("locked"));
        assert!(hint("ORA-29024: Certificate validation failure").contains("certificate"));
        assert!(explain_connect_error("ORA-99999: something unusual").is_none());
    }
}
