/// Why BootShift won't do something, and what the user can do about it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct Refusal {
    pub reason: String,
    pub fix: String,
}

impl Refusal {
    pub fn new(reason: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            fix: fix.into(),
        }
    }
}
