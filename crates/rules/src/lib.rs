//! The read-only checks and generators of tabkit. This crate never depends on the ops crate, so a check cannot reach a
//! cluster, per ADR-0900.

pub mod affected;
pub mod r#gen;
pub mod rules;
