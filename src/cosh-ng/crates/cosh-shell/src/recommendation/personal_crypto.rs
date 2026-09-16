use std::fmt;
use std::io::{self, Read};

const SHA256_BLOCK_BYTES: usize = 64;
const SHA256_OUTPUT_BYTES: usize = 32;

#[derive(Debug)]
pub(crate) enum CryptoError {
    Io(io::Error),
    InvalidOutputLength,
}

impl PartialEq for CryptoError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Io(left), Self::Io(right)) => left.kind() == right.kind(),
            (Self::InvalidOutputLength, Self::InvalidOutputLength) => true,
            _ => false,
        }
    }
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "random source failed: {error}"),
            Self::InvalidOutputLength => formatter.write_str("HKDF output exceeds 8160 bytes"),
        }
    }
}

impl std::error::Error for CryptoError {}

impl From<CryptoError> for String {
    fn from(error: CryptoError) -> Self {
        error.to_string()
    }
}

impl From<io::Error> for CryptoError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn sha256(input: &[u8]) -> [u8; SHA256_OUTPUT_BYTES] {
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(input.len() + 1 + SHA256_BLOCK_BYTES);
    padded.extend_from_slice(input);
    padded.push(0x80);
    while padded.len() % SHA256_BLOCK_BYTES != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for chunk in padded.as_chunks::<SHA256_BLOCK_BYTES>().0 {
        compress(&mut state, chunk);
    }

    let mut output = [0u8; SHA256_OUTPUT_BYTES];
    for (index, word) in state.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}

pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; SHA256_OUTPUT_BYTES] {
    let mut block = [0u8; SHA256_BLOCK_BYTES];
    if key.len() > SHA256_BLOCK_BYTES {
        block[..SHA256_OUTPUT_BYTES].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Vec::with_capacity(SHA256_BLOCK_BYTES + message.len());
    let mut outer = Vec::with_capacity(SHA256_BLOCK_BYTES + SHA256_OUTPUT_BYTES);
    for byte in block {
        inner.push(byte ^ 0x36);
        outer.push(byte ^ 0x5c);
    }
    inner.extend_from_slice(message);
    outer.extend_from_slice(&sha256(&inner));
    sha256(&outer)
}

pub(crate) fn hkdf_sha256(
    salt: &[u8],
    input_key_material: &[u8],
    info: &[u8],
    output_len: usize,
) -> Result<Vec<u8>, CryptoError> {
    if output_len > 255 * SHA256_OUTPUT_BYTES {
        return Err(CryptoError::InvalidOutputLength);
    }
    let default_salt = [0u8; SHA256_OUTPUT_BYTES];
    let extract_salt = if salt.is_empty() { &default_salt } else { salt };
    let pseudo_random_key = hmac_sha256(extract_salt, input_key_material);
    let mut output = Vec::with_capacity(output_len);
    let mut previous = Vec::new();
    for counter in 1..=output_len.div_ceil(SHA256_OUTPUT_BYTES) {
        let mut message = Vec::with_capacity(previous.len() + info.len() + 1);
        message.extend_from_slice(&previous);
        message.extend_from_slice(info);
        message.push(counter as u8);
        previous = hmac_sha256(&pseudo_random_key, &message).to_vec();
        output.extend_from_slice(&previous);
    }
    output.truncate(output_len);
    Ok(output)
}

pub(crate) fn derive_epoch_key(
    master_key: &[u8],
    store_epoch: &str,
) -> Result<[u8; SHA256_OUTPUT_BYTES], CryptoError> {
    let bytes = hkdf_sha256(
        store_epoch.as_bytes(),
        master_key,
        b"cosh-shell/recommendation/epoch-key/v1",
        SHA256_OUTPUT_BYTES,
    )?;
    let mut key = [0u8; SHA256_OUTPUT_BYTES];
    key.copy_from_slice(&bytes);
    Ok(key)
}

pub(crate) fn random_bytes(size: usize) -> Result<Vec<u8>, CryptoError> {
    let mut file = std::fs::File::open("/dev/urandom")?;
    let mut bytes = vec![0u8; size];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn random_hex(size: usize) -> Result<String, CryptoError> {
    Ok(hex(&random_bytes(size)?))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn compress(state: &mut [u32; 8], block: &[u8]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut words = [0u32; 64];
    for (index, bytes) in block.as_chunks::<4>().0.iter().enumerate() {
        words[index] = u32::from_be_bytes(*bytes);
    }
    for index in 16..64 {
        let s0 = words[index - 15].rotate_right(7)
            ^ words[index - 15].rotate_right(18)
            ^ (words[index - 15] >> 3);
        let s1 = words[index - 2].rotate_right(17)
            ^ words[index - 2].rotate_right(19)
            ^ (words[index - 2] >> 10);
        words[index] = words[index - 16]
            .wrapping_add(s0)
            .wrapping_add(words[index - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(choice)
            .wrapping_add(K[index])
            .wrapping_add(words[index]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }
    for (target, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *target = target.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_fips_vector() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231_case_one() {
        let key = [0x0b; 20];
        assert_eq!(
            hex(&hmac_sha256(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn hkdf_matches_rfc_5869_case_one() {
        let ikm = [0x0b; 22];
        let salt = decode_hex("000102030405060708090a0b0c");
        let info = decode_hex("f0f1f2f3f4f5f6f7f8f9");
        let okm = hkdf_sha256(&salt, &ikm, &info, 42).unwrap();
        assert_eq!(
            hex(&okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    #[test]
    fn random_opaque_values_have_requested_entropy_size() {
        let first = random_hex(32).unwrap();
        let second = random_hex(32).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(second.len(), 64);
        assert_ne!(first, second);
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}
