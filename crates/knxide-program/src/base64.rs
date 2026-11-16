//! Base64 decoding (RFC 4648 §4, standard alphabet) for the segment data in product files.

use crate::error::Error;

fn sextet(byte: u8) -> Option<u32> {
    let value = match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    };
    Some(u32::from(value))
}

/// Decode `text`. ASCII whitespace is skipped; padding is optional but, when present, must end
/// the input.
pub(crate) fn decode(text: &str) -> Result<Vec<u8>, Error> {
    let invalid = || Error::Invalid {
        what: "base64 data",
        value: text.chars().take(24).collect(),
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut padding = 0u32;
    for byte in text.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return Err(invalid());
        }
        buffer = (buffer << 6) | sextet(byte).ok_or_else(invalid)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    // A lone trailing sextet cannot encode a byte, left-over bits must be zero, and there are
    // never more than two padding characters.
    if bits == 6 || buffer != 0 || padding > 2 {
        return Err(invalid());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_vectors() {
        for (text, bytes) in [
            ("", &b""[..]),
            ("Zg==", b"f"),
            ("Zm8=", b"fo"),
            ("Zm9v", b"foo"),
            ("Zm9vYg==", b"foob"),
            ("Zm9vYmE=", b"fooba"),
            ("Zm9vYmFy", b"foobar"),
        ] {
            assert_eq!(decode(text).unwrap(), bytes, "{text}");
        }
    }

    #[test]
    fn whitespace_and_missing_padding() {
        assert_eq!(decode(" Zm9v\r\n  YmE ").unwrap(), b"fooba");
        assert_eq!(decode("/+8=").unwrap(), [0xFF, 0xEF]);
    }

    #[test]
    fn invalid_input() {
        for text in ["Z", "Zm9v*", "Zg==Zg", "Zh==", "Zg===", "Zm9vY"] {
            assert!(decode(text).is_err(), "{text:?} should fail");
        }
    }
}
