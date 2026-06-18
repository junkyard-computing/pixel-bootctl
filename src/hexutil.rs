// Tiny hex helpers for the Trusty `send` diagnostic.

pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !s.len().is_multiple_of(2) {
        return Err("hex must have even length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

pub fn encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let bytes = [0x00, 0x01, 0xfe, 0xca, 0xff];
        assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn strips_whitespace_and_decodes() {
        assert_eq!(decode("01 02\t03\n").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn rejects_odd_and_nonhex() {
        assert!(decode("abc").is_err());
        assert!(decode("zz").is_err());
    }

    #[test]
    fn encodes_lowercase_padded() {
        assert_eq!(encode(&[0x0a, 0xb0]), "0ab0");
    }
}
