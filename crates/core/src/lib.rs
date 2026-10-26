//! Pure planning logic for BootShift: layout model, planner, validation and journal codec.
//! This crate performs no I/O.

pub mod layout;
pub mod plan;
pub mod planner;
pub mod refusal;
pub mod units;
pub mod validate;

#[cfg(test)]
mod test_support;
