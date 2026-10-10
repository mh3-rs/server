//! The game's packet encryption (`roadmap/packet encryption.md`): once a console has a key (`AnsCommonKey` on OPN),
//! every payload on LMP and FMP is encrypted, both ways; the frame header stays in clear, its size the encrypted
//! length. Camellia with a 256-bit key, each 16-byte block on its own (ECB), over the payload behind a 4-byte header
//! (its length, big-endian u16, then two zero bytes), padded to whole blocks. An empty payload stays empty (the game
//! neither encrypts nor decrypts one). The decomp: `PatCryptEncrypt` / `PatCryptDecrypt`.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use openssl::symm::{Cipher, Crypter, Mode};

/// The key's length: the game uses its first 32 bytes (`Camellia_Ekeygen(0x100, …)`).
pub const KEY_LEN: usize = 32;
const BLOCK: usize = 16;
const HEADER: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum CryptError {
    #[error("encrypted payload of {0} bytes: not whole 16-byte blocks")]
    NotBlocks(usize),
    #[error("decrypted length {0} longer than its {1} bytes")]
    Length(usize, usize),
    #[error("decrypted header without its two zero bytes")]
    Header,
    #[error("cipher: {0}")]
    Cipher(#[from] openssl::error::ErrorStack),
}

pub fn encrypt(key: &[u8; KEY_LEN], payload: &[u8]) -> Result<Bytes, CryptError> {
    if payload.is_empty() {
        return Ok(Bytes::new());
    }
    let mut plain = BytesMut::with_capacity((HEADER + payload.len()).next_multiple_of(BLOCK));
    plain.put_u16(payload.len() as u16);
    plain.put_u16(0);
    plain.put_slice(payload);
    plain.resize(plain.len().next_multiple_of(BLOCK), 0);
    Ok(ecb(Mode::Encrypt, key, &plain)?.into())
}

pub fn decrypt(key: &[u8; KEY_LEN], payload: &[u8]) -> Result<Bytes, CryptError> {
    if payload.is_empty() {
        return Ok(Bytes::new());
    }
    if !payload.len().is_multiple_of(BLOCK) {
        return Err(CryptError::NotBlocks(payload.len()));
    }
    let mut plain = Bytes::from(ecb(Mode::Decrypt, key, payload)?);
    let len = usize::from(plain.get_u16());
    if plain.get_u16() != 0 {
        return Err(CryptError::Header);
    }
    if len > plain.len() {
        return Err(CryptError::Length(len, plain.len()));
    }
    Ok(plain.split_to(len))
}

fn ecb(mode: Mode, key: &[u8; KEY_LEN], data: &[u8]) -> Result<Vec<u8>, CryptError> {
    let mut crypter = Crypter::new(Cipher::camellia_256_ecb(), mode, key, None)?;
    crypter.pad(false);
    let mut out = vec![0; data.len() + BLOCK];
    let n = crypter.update(data, &mut out)?;
    let m = crypter.finalize(&mut out[n..])?;
    out.truncate(n + m);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = *b"0123456789abcdef0123456789abcdef";

    #[test]
    fn round_trips() {
        for len in [0usize, 1, 11, 12, 13, 28, 100, 4096] {
            let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let sealed = encrypt(&KEY, &payload).unwrap();
            // The length header and the payload, in whole blocks.
            assert_eq!(
                sealed.len(),
                (len + 4).next_multiple_of(16) * usize::from(len > 0)
            );
            assert_eq!(decrypt(&KEY, &sealed).unwrap(), payload);
        }
    }

    /// Camellia-256 itself: RFC 3713's test vector (one block).
    #[test]
    fn camellia_is_rfc_3713() {
        let key = hex::decode("0123456789abcdeffedcba987654321000112233445566778899aabbccddeeff")
            .unwrap();
        let plain = hex::decode("0123456789abcdeffedcba9876543210").unwrap();
        let sealed = ecb(Mode::Encrypt, &key.try_into().unwrap(), &plain).unwrap();
        assert_eq!(hex::encode(sealed), "9acc237dff16d76c20ef7c919e3a7509");
    }

    #[test]
    fn refuses_what_isnt_blocks_or_lies_about_its_length() {
        assert!(matches!(
            decrypt(&KEY, &[0; 15]),
            Err(CryptError::NotBlocks(15))
        ));
        let mut lying = [0; 16];
        lying[0] = 0xff;
        let lying = ecb(Mode::Encrypt, &KEY, &lying).unwrap();
        assert!(matches!(decrypt(&KEY, &lying), Err(CryptError::Length(..))));
        let garbled = ecb(Mode::Encrypt, &KEY, &[0xff; 16]).unwrap();
        assert!(matches!(decrypt(&KEY, &garbled), Err(CryptError::Header)));
    }
}
