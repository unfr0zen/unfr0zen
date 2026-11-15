//! The update session's sealed channel: a ChaCha20-Poly1305 key a direction,
//! derived from the SPAKE2 key; streams are sealed in counted CHUNK pieces.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::hkdf;

use crate::random;

/// The plaintext size of a stream's chunks, all but the last.
pub const CHUNK: usize = 64 << 10;

const TO_UNIT: &[u8] = b"nestlocal-update c2u";
const TO_CLIENT: &[u8] = b"nestlocal-update u2c";
const PREFIX_LEN: usize = 7;
const TAG_LEN: usize = 16;

/// A sealed message or chunk that does not open: the wrong key, AAD or
/// order, or changed on the way.
#[derive(Debug, PartialEq, Eq)]
pub struct Tampered;

/// One key a direction.
pub struct Keys {
    pub to_unit: [u8; 32],
    pub to_client: [u8; 32],
}

/// The session's keys: HKDF-SHA256 of the SPAKE2 key, with an empty salt and
/// one label a direction.
pub fn keys(session_key: &[u8]) -> Keys {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(session_key);
    let expand = |info: &[u8]| {
        let mut key = [0; 32];
        prk.expand(&[info], hkdf::HKDF_SHA256)
            .and_then(|okm| okm.fill(&mut key))
            .expect("32 bytes is a valid HKDF-SHA256 length");
        key
    };
    Keys { to_unit: expand(TO_UNIT), to_client: expand(TO_CLIENT) }
}

fn cipher(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("ChaCha20 takes a 32-byte key"))
}

/// A sealed request's or answer's AAD, as both ends have it: the request's
/// path and its session, so a message is bound to both.
pub fn aad(path: &str, session: &str) -> Vec<u8> {
    [b"nestlocal-update ", path.as_bytes(), b"\n", session.as_bytes()].concat()
}

/// `plaintext` sealed under `key` and `aad`: a random 12-byte nonce, the
/// ciphertext, then its tag.
pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let nonce = random::<NONCE_LEN>();
    let mut sealed = Vec::with_capacity(NONCE_LEN + plaintext.len() + TAG_LEN);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(plaintext);
    let tag = cipher(key)
        .seal_in_place_separate_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad), &mut sealed[NONCE_LEN..])
        .expect("a message fits ChaCha20-Poly1305");
    sealed.extend_from_slice(tag.as_ref());
    sealed
}

/// The plaintext of a message `seal` made with the same key and AAD.
pub fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, Tampered> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(Tampered);
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| Tampered)?;
    let mut body = body.to_vec();
    let plaintext = cipher(key).open_in_place(nonce, Aad::from(aad), &mut body).map_err(|_| Tampered)?.len();
    body.truncate(plaintext);
    Ok(body)
}

/// A sealed message's nonce. A response's AAD includes its request's nonce,
/// so a response cannot be replayed onto another request.
pub fn nonce_of(sealed: &[u8]) -> Option<&[u8]> {
    sealed.get(..NONCE_LEN)
}

/// A chunk's nonce: the stream's random prefix, the chunk's index (u32 BE),
/// and 1 on the last chunk, else 0.
fn stream_nonce(prefix: &[u8; PREFIX_LEN], index: u32, last: bool) -> Nonce {
    let mut nonce = [0; NONCE_LEN];
    nonce[..PREFIX_LEN].copy_from_slice(prefix);
    nonce[PREFIX_LEN..NONCE_LEN - 1].copy_from_slice(&index.to_be_bytes());
    nonce[NONCE_LEN - 1] = last.into();
    Nonce::assume_unique_for_key(nonce)
}

/// Seals a stream in order. Each chunk is its last flag (one byte), then the
/// ciphertext and its tag. The flag is also in the nonce, so it is
/// authenticated.
pub struct StreamSealer {
    key: LessSafeKey,
    aad: Vec<u8>,
    prefix: [u8; PREFIX_LEN],
    index: u32,
}

impl StreamSealer {
    /// A sealer and the random prefix its opener needs.
    pub fn new(key: &[u8; 32], aad: &[u8]) -> (StreamSealer, [u8; PREFIX_LEN]) {
        let prefix = random();
        (StreamSealer { key: cipher(key), aad: aad.to_vec(), prefix, index: 0 }, prefix)
    }

    /// The next chunk, `last` on the stream's final one.
    pub fn chunk(&mut self, data: &[u8], last: bool) -> Vec<u8> {
        let nonce = stream_nonce(&self.prefix, self.index, last);
        self.index = self.index.checked_add(1).expect("a stream has fewer than 2^32 chunks");
        let mut sealed = Vec::with_capacity(1 + data.len() + TAG_LEN);
        sealed.push(last.into());
        sealed.extend_from_slice(data);
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(&self.aad), &mut sealed[1..])
            .expect("a chunk fits ChaCha20-Poly1305");
        sealed.extend_from_slice(tag.as_ref());
        sealed
    }
}

/// Opens a stream's chunks in the order they were sealed, and none after the
/// last. A stream that ends before a chunk reported last was cut short: the
/// caller treats it as tampered.
pub struct StreamOpener {
    key: LessSafeKey,
    aad: Vec<u8>,
    prefix: [u8; PREFIX_LEN],
    index: u32,
    done: bool,
}

impl StreamOpener {
    pub fn new(key: &[u8; 32], aad: &[u8], prefix: [u8; PREFIX_LEN]) -> StreamOpener {
        StreamOpener { key: cipher(key), aad: aad.to_vec(), prefix, index: 0, done: false }
    }

    /// The next chunk's plaintext and whether it is the last.
    pub fn chunk(&mut self, sealed: &[u8]) -> Result<(Vec<u8>, bool), Tampered> {
        if self.done || sealed.len() < 1 + TAG_LEN {
            return Err(Tampered);
        }
        let last = match sealed[0] {
            0 => false,
            1 => true,
            _ => return Err(Tampered),
        };
        let nonce = stream_nonce(&self.prefix, self.index, last);
        let mut body = sealed[1..].to_vec();
        let plaintext = self.key.open_in_place(nonce, Aad::from(&self.aad), &mut body).map_err(|_| Tampered)?.len();
        body.truncate(plaintext);
        self.index = self.index.checked_add(1).ok_or(Tampered)?;
        self.done = last;
        Ok((body, last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn keys_match_the_shared_vector() {
        let session: Vec<u8> = (0..32).collect();
        let keys = keys(&session);
        assert_eq!(hex(&keys.to_unit), "142f40d937d3e036b379d8c5cbc0ed2d293a41d3c2245758fa260400d253cef3");
        assert_eq!(hex(&keys.to_client), "c15f9731af18b44a18e040414b0102ca33f8316e4b8ddece0cf3664b412511ed");
    }

    #[test]
    fn a_sealed_message_opens_only_with_its_key_and_aad() {
        let sealed = seal(&KEY, b"aad", b"hello");
        assert_eq!(open(&KEY, b"aad", &sealed), Ok(b"hello".to_vec()));
        assert_eq!(nonce_of(&sealed), Some(&sealed[..12]));
        assert_eq!(open(&[8; 32], b"aad", &sealed), Err(Tampered));
        assert_eq!(open(&KEY, b"other", &sealed), Err(Tampered));
        assert_ne!(seal(&KEY, b"aad", b"hello"), sealed, "each seal takes a fresh nonce");
    }

    #[test]
    fn a_flipped_byte_is_tampered() {
        let sealed = seal(&KEY, b"aad", b"hello");
        for i in 0..sealed.len() {
            let mut flipped = sealed.clone();
            flipped[i] ^= 1;
            assert_eq!(open(&KEY, b"aad", &flipped), Err(Tampered), "byte {i}");
        }
        assert_eq!(open(&KEY, b"aad", &sealed[..sealed.len() - 1]), Err(Tampered));
        assert_eq!(open(&KEY, b"aad", &sealed[..12]), Err(Tampered));
        assert_eq!(nonce_of(&sealed[..11]), None);
    }

    fn stream(parts: &[&[u8]]) -> ([u8; 7], Vec<Vec<u8>>) {
        let (mut sealer, prefix) = StreamSealer::new(&KEY, b"aad");
        let chunks = parts.iter().enumerate().map(|(i, part)| sealer.chunk(part, i + 1 == parts.len())).collect();
        (prefix, chunks)
    }

    #[test]
    fn stream_chunks_open_in_order_and_flag_the_last() {
        let big = vec![0x5a; CHUNK];
        let (prefix, chunks) = stream(&[b"one", &big, b"three"]);
        let mut opener = StreamOpener::new(&KEY, b"aad", prefix);
        assert_eq!(opener.chunk(&chunks[0]), Ok((b"one".to_vec(), false)));
        assert_eq!(opener.chunk(&chunks[1]), Ok((big, false)));
        assert_eq!(opener.chunk(&chunks[2]), Ok((b"three".to_vec(), true)));
        assert_eq!(opener.chunk(&chunks[2]), Err(Tampered), "nothing follows the last chunk");
        assert_eq!(StreamOpener::new(&KEY, b"other", prefix).chunk(&chunks[0]), Err(Tampered));
    }

    #[test]
    fn a_reordered_dropped_or_truncated_stream_is_tampered() {
        let (prefix, chunks) = stream(&[b"zero", b"one", b"two"]);

        let mut swapped = StreamOpener::new(&KEY, b"aad", prefix);
        assert_eq!(swapped.chunk(&chunks[1]), Err(Tampered));

        let mut truncated = StreamOpener::new(&KEY, b"aad", prefix);
        for chunk in &chunks[..2] {
            let (_, last) = truncated.chunk(chunk).unwrap();
            assert!(!last, "a stream missing its last chunk never reports last");
        }

        let mut replayed = StreamOpener::new(&KEY, b"aad", prefix);
        assert!(replayed.chunk(&chunks[0]).is_ok());
        assert_eq!(replayed.chunk(&chunks[0]), Err(Tampered));

        let (other_prefix, _) = stream(&[b"x"]);
        assert_eq!(StreamOpener::new(&KEY, b"aad", other_prefix).chunk(&chunks[0]), Err(Tampered));
    }
}
