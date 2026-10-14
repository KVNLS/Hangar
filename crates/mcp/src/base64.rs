//! RFC 4648 base64 (standard alphabet, padded), encode only.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn sextet(n: u32, shift: u32) -> char {
    char::from(ALPHABET[((n >> shift) & 0x3f) as usize])
}

pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (chunks, rest) = input.as_chunks::<3>();
    for &[a, b, c] in chunks {
        let n = (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c);
        for shift in [18, 12, 6, 0] {
            out.push(sextet(n, shift));
        }
    }
    match *rest {
        [a] => {
            let n = u32::from(a) << 16;
            out.push(sextet(n, 18));
            out.push(sextet(n, 12));
            out.push_str("==");
        }
        [a, b] => {
            let n = (u32::from(a) << 16) | (u32::from(b) << 8);
            out.push(sextet(n, 18));
            out.push(sextet(n, 12));
            out.push(sextet(n, 6));
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    #[test]
    fn rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(encode(input.as_bytes()), expected, "input {input:?}");
        }
    }

    #[test]
    fn high_bytes_use_plus_and_slash() {
        assert_eq!(encode(&[0xfb, 0xff, 0xbf]), "+/+/");
        assert_eq!(encode(&[0x00, 0x00, 0x00, 0xff]), "AAAA/w==");
    }
}
