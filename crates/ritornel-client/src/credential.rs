//! A credential: the session token a client presents.

use std::fmt;

/// A session token. `Debug` prints a fixed redaction and there is no
/// `Display`, so it cannot reach a log or an error message by accident.
#[derive(Clone)]
pub struct Credential(String);

impl Credential {
    /// Wraps a token read from somewhere, such as the credential file.
    /// Space around it is not part of it.
    pub fn new(token: impl AsRef<str>) -> Self {
        Self(token.as_ref().trim().to_owned())
    }

    /// The token itself, for the one place it goes: the `Authorization`
    /// header.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_a_fixed_redaction_and_the_line_end_is_not_part_of_the_token() {
        const SENTINEL: &str = "SENTINEL_SESSION_TOKEN_0001";
        let credential = Credential::new(format!("{SENTINEL}\n"));
        assert_eq!(format!("{credential:?}"), "Credential(<redacted>)");
        assert_eq!(
            format!("{:#?}", Some(&credential)),
            "Some(\n    Credential(<redacted>),\n)"
        );
        assert_eq!(credential.expose(), SENTINEL);
    }
}
