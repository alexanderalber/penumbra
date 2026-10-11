//! The common error type.

use std::fmt;

/// Why something failed. The message is meant for a person (or Claude) and goes back to the
/// caller of a command verbatim, so it says what was wrong and, where possible, what is allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// The same error with `context` in front, e.g. `op 3 (set /noise1.scale): ...`.
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self(format!("{context}: {}", self.0))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}
