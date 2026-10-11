//! The Nintendo login's form encoding (`/ac`, `../docs/history/nintendo login.md`): `key=value` pairs joined by `&`, every value
//! base64 in the game's own alphabet, `.` and `-` for 62 and 63 and `*` for padding (D6, read in `main.dol`). The
//! game's decoder refuses `+`, `/` and `=`. Its requests are URL-encoded on top (`*` comes as `%2A`, seen in the
//! first console run, 2026-10-11); the replies it reads are not.

use base64::Engine;
use base64::alphabet::Alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

const ALPHABET: Alphabet =
    match Alphabet::new("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.-") {
        Ok(alphabet) => alphabet,
        Err(_) => panic!("64 distinct characters"),
    };
/// The padding is added and stripped by hand: the engine only knows `=`.
const ENGINE: GeneralPurpose = GeneralPurpose::new(
    &ALPHABET,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone),
);
const PAD: char = '*';

/// A form, as the game posts it or reads a reply: `None` if a pair has no `=` or a value isn't base64.
pub fn decode(body: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    body.split(|&b| matches!(b, b'&' | b'\r' | b'\n'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let at = pair.iter().position(|&b| b == b'=')?;
            let key = String::from_utf8(pair[..at].to_vec()).ok()?;
            let value: Vec<u8> = percent_encoding::percent_decode(&pair[at + 1..]).collect();
            let value = &value[..];
            let unpadded = value.strip_suffix(b"**").or(value.strip_suffix(b"*"));
            Some((key, ENGINE.decode(unpadded.unwrap_or(value)).ok()?))
        })
        .collect()
}

/// A form for the game, ending in `\r\n` as Nintendo's did.
pub fn encode(pairs: &[(&str, &[u8])]) -> String {
    let mut out = String::new();
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(key);
        out.push('=');
        ENGINE.encode_string(value, &mut out);
        out.extend(std::iter::repeat_n(PAD, (3 - value.len() % 3) % 3));
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_games_alphabet_and_padding() {
        // 62 and 63, then padding.
        assert_eq!(encode(&[("k", &[0xfb, 0xff])]), "k=.-8*\r\n");
        // A user id as AltWFC handed it out, and the game sent back; URL-encoded, as the game posts it.
        let form = decode(b"userid=MDAwMDAwMDAwMDAwMg%2A%2A&svc=").unwrap();
        assert_eq!(form[0], ("userid".into(), b"0000000000002".to_vec()));
        assert_eq!(form[1], ("svc".into(), Vec::new()));
        assert_eq!(decode(b"k=MDI*").unwrap()[0].1, b"02", "or not");
        let pairs: [(&str, &[u8]); 3] = [("a", b"x"), ("b", b"xy"), ("c", &[0, 0xfe, 0xff])];
        let form = decode(encode(&pairs).as_bytes()).unwrap();
        assert!(
            form.iter()
                .zip(pairs)
                .all(|(got, want)| got.0 == want.0 && got.1 == want.1)
        );
        // What the game refuses: the standard alphabet's 62, 63 and padding; a pair without `=`.
        for wrong in [&b"k=++8="[..], b"k=//8*", b"k=AA==", b"k"] {
            assert!(
                decode(wrong).is_none(),
                "{}",
                String::from_utf8_lossy(wrong)
            );
        }
    }
}
