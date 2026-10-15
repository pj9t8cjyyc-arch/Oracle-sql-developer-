//! Import of CSV/TSV/Excel files into Oracle tables: read, profile (infer types and
//! formats), map to a target table, validate, and load in array-bound batches.

pub mod plan;
pub mod profile;
pub mod source;
