//! Bytes as lowercase hex digits: how a key, a signature or a nonce is
//! written on the wire.

/// `bytes` as two lowercase hex digits each.
pub fn encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to a string cannot fail.
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The bytes `text` writes, in either case. `None` if it is not an even
/// number of hex digits.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let (pairs, rest) = text.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|[high, low]| Some(digit(*high)? << 4 | digit(*low)?))
        .collect()
}

/// The `N` bytes `text` writes. `None` if it writes any other number.
pub fn decode_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    decode(text)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_go_there_and_back_and_anything_else_is_refused() {
        assert_eq!(encode(&[0x00, 0x5a, 0xff]), "005aff");
        assert_eq!(decode("005AfF"), Some(vec![0x00, 0x5a, 0xff]));
        assert_eq!(decode_array::<3>("005aff"), Some([0x00, 0x5a, 0xff]));
        assert_eq!(decode_array::<2>("005aff"), None);
        for wrong in ["0", "0g", "zz", "00 1"] {
            assert_eq!(decode(wrong), None, "{wrong}");
        }
    }
}
