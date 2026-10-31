//! Pure planning logic for Kitten: layout model, planner, validation and journal codec.
//! This crate performs no I/O.

pub mod journal;
pub mod layout;
pub mod plan;
pub mod planner;
pub mod refusal;
pub mod scope;
pub mod units;
pub mod validate;

#[cfg(test)]
mod test_support;
