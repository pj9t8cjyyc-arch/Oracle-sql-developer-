//! Core of the Oracle IDE: connection profiles, SQL safety analysis,
//! streaming query sessions and schema browsing. UI-agnostic and testable
//! without a database (see [`fake::FakeDb`]).

pub mod catalog;
pub mod comments;
pub mod complete;
pub mod db;
pub mod error;
pub mod fake;
pub mod guard;
pub mod insight;
pub mod lexer;
pub mod meta;
#[cfg(feature = "oracle")]
pub mod oracle_backend;
pub mod parse;
pub mod profile;
pub mod schema;
pub mod search;
pub mod session;

pub use error::{Error, Result};
