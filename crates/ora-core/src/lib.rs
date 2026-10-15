//! Core of the Oracle IDE: connection profiles, SQL safety analysis,
//! streaming query sessions and schema browsing. UI-agnostic and testable
//! without a database (see [`fake::FakeDb`]).

pub mod ai;
pub mod catalog;
pub mod comments;
pub mod complete;
pub mod db;
pub mod dossier;
pub mod error;
pub mod export;
pub mod fake;
pub mod graph;
pub mod guard;
pub mod history;
pub mod import;
pub mod insight;
pub mod knowledge;
pub mod lexer;
pub mod meta;
#[cfg(feature = "oracle")]
pub mod oracle_backend;
pub mod packages;
pub mod parse;
pub mod profile;
pub mod schema;
pub mod search;
pub mod session;
pub mod sqltext;

pub use error::{Error, Result};
