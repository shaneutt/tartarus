//! Token-redaction helpers for `auth status`.

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Trailing characters preserved in redacted output.
const VISIBLE_TAIL: usize = 4;

/// Marker replacing the redacted prefix.
const REDACTED_MARKER: &str = "…";

// -----------------------------------------------------------------------------
// Redaction
// -----------------------------------------------------------------------------

/// Render `secret` as `…xxxx`. Secrets at or below four chars are
/// fully replaced with the marker. Operates on [`char`] boundaries.
pub fn last_four(secret: &str) -> String {
    let count = secret.chars().count();

    if count <= VISIBLE_TAIL {
        return REDACTED_MARKER.to_owned();
    }

    let tail: String = secret.chars().skip(count - VISIBLE_TAIL).collect();

    format!("{REDACTED_MARKER}{tail}")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_redacts_to_marker_only() {
        assert_eq!(
            last_four(""),
            REDACTED_MARKER,
            "an empty secret should not leak any characters",
        );
    }

    #[test]
    fn short_secret_redacts_completely() {
        assert_eq!(
            last_four("abc"),
            REDACTED_MARKER,
            "a 3-char secret is shorter than the tail",
        );
    }

    #[test]
    fn boundary_length_redacts_completely() {
        assert_eq!(
            last_four("abcd"),
            REDACTED_MARKER,
            "a length-equal secret should still be redacted in full",
        );
    }

    #[test]
    fn long_secret_keeps_last_four() {
        let redacted = last_four("ghp_pretendthisisarealtokenWXYZ");

        assert_eq!(
            redacted,
            format!("{REDACTED_MARKER}WXYZ"),
            "only the last four chars should remain visible",
        );
    }

    #[test]
    fn multibyte_input_does_not_split_codepoints() {
        let redacted = last_four("zzzzzααβγ");

        assert_eq!(
            redacted,
            format!("{REDACTED_MARKER}ααβγ"),
            "redaction should operate on chars, not bytes",
        );
    }
}
