//! Connection profiles. Passwords are never part of a profile: they live in
//! the OS keychain (handled by the app shell).

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    #[default]
    Dev,
    Test,
    Prod,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub service: String,
    pub user: String,
    #[serde(default)]
    pub environment: Environment,
}

fn default_port() -> u16 {
    1521
}

impl ConnectionProfile {
    pub fn validate(&self) -> Result<()> {
        for (label, v) in [
            ("name", &self.name),
            ("host", &self.host),
            ("service", &self.service),
            ("user", &self.user),
        ] {
            if v.trim().is_empty() {
                return Err(Error::Invalid(format!("{label} is required")));
            }
        }
        // The connect string is built by concatenation, so reject characters
        // that could alter its structure.
        for v in [&self.host, &self.service] {
            if v.chars()
                .any(|c| matches!(c, '/' | ':' | '(' | ')' | '=' | ' ' | '\'' | '"'))
            {
                return Err(Error::Invalid(format!("illegal character in '{v}'")));
            }
        }
        Ok(())
    }

    /// Easy Connect string: `//host:port/service`.
    pub fn connect_string(&self) -> String {
        format!("//{}:{}/{}", self.host, self.port, self.service)
    }
}
