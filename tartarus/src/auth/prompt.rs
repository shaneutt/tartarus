//! Interactive prompt helpers for reading lines from a [`BufRead`] +
//! [`Write`] pair.

use std::io::{BufRead, Write};

use crate::{auth::error::AuthError, error::Result};

// ---------------------------------------------------------------------------
// Line Reading
// ---------------------------------------------------------------------------

/// Print `prompt`, flush, and read one line. Trailing `\n`/`\r\n` are
/// stripped; an empty line (Enter) returns `""`.
pub fn read_line<R: BufRead, W: Write>(reader: &mut R, writer: &mut W, prompt: &str) -> Result<String> {
    write!(writer, "{prompt}").map_err(AuthError::InteractiveWriteFailed)?;
    writer.flush().map_err(AuthError::InteractiveWriteFailed)?;

    let mut buf = String::new();
    let n = reader.read_line(&mut buf).map_err(AuthError::InteractiveReadFailed)?;

    if n == 0 {
        return Ok(String::new());
    }

    if buf.ends_with('\n') {
        buf.pop();
        if buf.ends_with('\r') {
            buf.pop();
        }
    }

    Ok(buf)
}

/// Read a line, substituting `default` on empty input.
pub fn read_line_with_default<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
    prompt: &str,
    default: &str,
) -> Result<String> {
    let line = read_line(reader, writer, prompt)?;

    if line.is_empty() {
        return Ok(default.to_owned());
    }

    Ok(line)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};

    use super::*;

    #[test]
    fn read_line_returns_input_without_trailing_newline() {
        let input = b"hello world\n";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut writer = Vec::new();

        let line = read_line(&mut reader, &mut writer, "prompt: ").expect("read should succeed");

        assert_eq!(line, "hello world", "newline should be stripped");
        assert_eq!(writer, b"prompt: ", "prompt should be flushed to the writer");
    }

    #[test]
    fn read_line_handles_crlf() {
        let input = b"hello\r\n";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut writer = Vec::new();

        let line = read_line(&mut reader, &mut writer, "p: ").expect("read should succeed");

        assert_eq!(line, "hello", "CRLF endings should be normalised away");
    }

    #[test]
    fn empty_input_returns_empty_string() {
        let input = b"";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut writer = Vec::new();

        let line = read_line(&mut reader, &mut writer, "p: ").expect("read should succeed");

        assert_eq!(line, "", "EOF should produce an empty string");
    }

    #[test]
    fn enter_returns_empty_for_browser_fallback() {
        let input = b"\n";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut writer = Vec::new();

        let line = read_line(&mut reader, &mut writer, "p: ").expect("read should succeed");

        assert_eq!(line, "", "a bare newline should be treated as empty input");
    }

    #[test]
    fn read_line_with_default_substitutes_on_empty() {
        let input = b"\n";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut writer = Vec::new();

        let line =
            read_line_with_default(&mut reader, &mut writer, "region: ", "us-east5").expect("read should succeed");

        assert_eq!(line, "us-east5", "empty input should pick up the default");
    }

    #[test]
    fn read_line_with_default_keeps_explicit_value() {
        let input = b"europe-west4\n";
        let mut reader = BufReader::new(Cursor::new(input));
        let mut writer = Vec::new();

        let line =
            read_line_with_default(&mut reader, &mut writer, "region: ", "us-east5").expect("read should succeed");

        assert_eq!(line, "europe-west4", "explicit input should override the default");
    }
}
