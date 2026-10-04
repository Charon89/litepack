//! Encryption (spec section 14): cipher suites, the key slot, derived nonces,
//! associated data and the frame sealer.

use crate::error::FormatError;
use crate::frame::FrameKind;
use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use aes_gcm::Aes256Gcm;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::XChaCha20Poly1305;
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Length of every key in bytes.
pub const KEY_LEN: usize = 32;
/// Length of the authentication tag in bytes.
pub const TAG_LEN: usize = 16;
/// Length of the Argon2 salt in bytes.
pub const SALT_LEN: usize = 16;
/// Encoded length of the key slot payload.
pub const KEY_SLOT_LEN: usize = 1 + 1 + 4 + 4 + 4 + SALT_LEN + 1 + (KEY_LEN + TAG_LEN) + 32;
/// The `kdf` value of Argon2id.
pub const KDF_ARGON2ID: u8 = 1;
/// Sequence the index frame is sealed under: the trailer cannot name the
/// index's position, so the index uses this constant, which no counted frame
/// reaches.
pub const INDEX_SEQUENCE: u64 = u64::MAX;

/// The sequence the index of generation `generation` is sealed under:
/// `INDEX_SEQUENCE - generation`, so no two generations' indexes share a nonce
/// (spec section 15).
pub fn index_sequence(generation: u64) -> u64 {
    INDEX_SEQUENCE.saturating_sub(generation)
}

/// Largest `t` a reader accepts.
pub const MAX_ARGON2_T: u32 = 64;
/// Largest `p` a reader accepts.
pub const MAX_ARGON2_P: u32 = 64;
/// Smallest `m` in KiB.
pub const MIN_ARGON2_M_KIB: u32 = 8192;

const NONCE_INFO: &[u8] = b"LitePack lpk v1 nonce";
const AD_PREFIX: &[u8] = b"LitePack lpk v1 AD";
const WRAP_AD_PREFIX: &[u8] = b"LitePack lpk v1 keywrap";
const KEYFILE_INFO: &[u8] = b"LitePack lpk v1 keyfile";

/// Cipher suite of an encrypted archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Suite {
    /// AES-256-GCM, 12-byte nonces.
    AesGcm = 1,
    /// XChaCha20-Poly1305, 24-byte nonces.
    XChaCha = 2,
}

impl Suite {
    /// The suite for a raw value.
    pub fn from_u8(v: u8) -> Option<Suite> {
        match v {
            1 => Some(Suite::AesGcm),
            2 => Some(Suite::XChaCha),
            _ => None,
        }
    }

    /// Nonce length in bytes.
    pub fn nonce_len(self) -> usize {
        match self {
            Suite::AesGcm => 12,
            Suite::XChaCha => 24,
        }
    }

    /// Name used in the spec and by the tools.
    pub fn name(self) -> &'static str {
        match self {
            Suite::AesGcm => "AES-256-GCM",
            Suite::XChaCha => "XChaCha20-Poly1305",
        }
    }

    /// Bytes a sealed payload adds to the plain one: nonce and tag.
    pub fn overhead(self) -> usize {
        self.nonce_len() + TAG_LEN
    }
}

fn aead_encrypt(
    suite: Suite,
    key: &[u8],
    nonce: &[u8],
    ad: &[u8],
    msg: &[u8],
) -> Result<Vec<u8>, ()> {
    fn go<C: Aead + KeyInit>(
        key: &[u8],
        nonce: &[u8],
        ad: &[u8],
        msg: &[u8],
    ) -> Result<Vec<u8>, ()> {
        let c = C::new_from_slice(key).map_err(|_| ())?;
        let n = Nonce::<C>::try_from(nonce).map_err(|_| ())?;
        c.encrypt(&n, Payload { msg, aad: ad }).map_err(|_| ())
    }
    match suite {
        Suite::AesGcm => go::<Aes256Gcm>(key, nonce, ad, msg),
        Suite::XChaCha => go::<XChaCha20Poly1305>(key, nonce, ad, msg),
    }
}

fn aead_decrypt(
    suite: Suite,
    key: &[u8],
    nonce: &[u8],
    ad: &[u8],
    ct: &[u8],
) -> Result<Vec<u8>, ()> {
    fn go<C: Aead + KeyInit>(
        key: &[u8],
        nonce: &[u8],
        ad: &[u8],
        ct: &[u8],
    ) -> Result<Vec<u8>, ()> {
        let c = C::new_from_slice(key).map_err(|_| ())?;
        let n = Nonce::<C>::try_from(nonce).map_err(|_| ())?;
        c.decrypt(&n, Payload { msg: ct, aad: ad }).map_err(|_| ())
    }
    match suite {
        Suite::AesGcm => go::<Aes256Gcm>(key, nonce, ad, ct),
        Suite::XChaCha => go::<XChaCha20Poly1305>(key, nonce, ad, ct),
    }
}

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// Passes over the memory.
    pub t: u32,
    /// Memory in KiB.
    pub m_kib: u32,
    /// Lanes.
    pub p: u32,
}

impl Default for Argon2Params {
    /// RFC 9106's second recommendation: `t = 3`, 64 MiB, `p = 4`.
    fn default() -> Self {
        Argon2Params {
            t: 3,
            m_kib: 65536,
            p: 4,
        }
    }
}

impl Argon2Params {
    /// Check the bounds: `1 <= t <= 64`, `8192 <= m_kib`, `1 <= p <= 64`.
    pub fn validate(&self) -> Result<(), FormatError> {
        let bad = |reason| Err(FormatError::BadArgon2 { reason });
        if self.t < 1 || self.t > MAX_ARGON2_T {
            return bad("t");
        }
        if self.m_kib < MIN_ARGON2_M_KIB {
            return bad("m");
        }
        if self.p < 1 || self.p > MAX_ARGON2_P {
            return bad("p");
        }
        Ok(())
    }
}

/// A password and an optional keyfile; the bytes are wiped when it is dropped.
#[derive(Clone)]
pub struct Credentials {
    /// The password bytes.
    pub password: Vec<u8>,
    /// The keyfile's bytes (a second factor), if used.
    pub keyfile: Option<Vec<u8>>,
}

impl Credentials {
    /// A password without a keyfile.
    pub fn password(password: impl Into<Vec<u8>>) -> Self {
        Credentials {
            password: password.into(),
            keyfile: None,
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("keyfile", &self.keyfile.is_some())
            .finish_non_exhaustive()
    }
}

impl Zeroize for Credentials {
    fn zeroize(&mut self) {
        self.password.zeroize();
        if let Some(k) = &mut self.keyfile {
            k.zeroize();
        }
        self.keyfile = None;
    }
}

impl Drop for Credentials {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for Credentials {}

/// The 32-byte archive key; wiped when dropped.
#[derive(Clone)]
pub struct ArchiveKey([u8; KEY_LEN]);

impl std::fmt::Debug for ArchiveKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArchiveKey(..)")
    }
}

impl Zeroize for ArchiveKey {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl Drop for ArchiveKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for ArchiveKey {}

impl ArchiveKey {
    /// A key from raw bytes.
    pub fn from_bytes(b: [u8; KEY_LEN]) -> Self {
        ArchiveKey(b)
    }

    /// Draw a fresh key from `rng`.
    pub fn generate(rng: &mut dyn RngCore) -> Self {
        let mut b = [0u8; KEY_LEN];
        rng.fill_bytes(&mut b);
        ArchiveKey(b)
    }

    /// True when this key matches the slot's check value (constant time).
    pub fn check(&self, archive_id: &[u8; 16], slot: &KeySlot) -> bool {
        blake3::keyed_hash(&self.0, archive_id) == blake3::Hash::from_bytes(slot.check)
    }
}

/// The nonce of a sealed frame: `HKDF-SHA256(key, salt = archive_id, info =
/// "LitePack lpk v1 nonce" || kind (u16 LE) || sequence (u64 LE))`, cut to `len`
/// bytes (12 or 24).
pub fn derive_nonce(
    key: &ArchiveKey,
    archive_id: &[u8; 16],
    kind: u16,
    sequence: u64,
    len: usize,
) -> Vec<u8> {
    let mut info = Vec::with_capacity(NONCE_INFO.len() + 10);
    info.extend_from_slice(NONCE_INFO);
    info.extend_from_slice(&kind.to_le_bytes());
    info.extend_from_slice(&sequence.to_le_bytes());
    let mut out = vec![0u8; len];
    // `len` is at most 24, far below the HKDF limit.
    if Hkdf::<Sha256>::new(Some(archive_id), &key.0)
        .expand(&info, &mut out)
        .is_err()
    {
        out.clear();
    }
    out
}

/// The associated data of a sealed frame: `"LitePack lpk v1 AD" || archive_id ||
/// kind (u16 LE) || sequence (u64 LE) || payload_len (u64 LE)`.
pub fn associated_data(
    archive_id: &[u8; 16],
    kind: u16,
    sequence: u64,
    payload_len: u64,
) -> Vec<u8> {
    let mut v = Vec::with_capacity(AD_PREFIX.len() + 16 + 18);
    v.extend_from_slice(AD_PREFIX);
    v.extend_from_slice(archive_id);
    v.extend_from_slice(&kind.to_le_bytes());
    v.extend_from_slice(&sequence.to_le_bytes());
    v.extend_from_slice(&payload_len.to_le_bytes());
    v
}

/// `"LitePack lpk v1 keywrap" || archive_id || header_flags (u32 LE)`: the
/// header flags are bound, so a flipped `ENCRYPTED` or `LISTABLE` bit fails at
/// the key slot.
fn wrap_ad(archive_id: &[u8; 16], header_flags: u32) -> Vec<u8> {
    let mut v = WRAP_AD_PREFIX.to_vec();
    v.extend_from_slice(archive_id);
    v.extend_from_slice(&header_flags.to_le_bytes());
    v
}

/// KEK = Argon2id(password, salt, t, m, p); with a keyfile, `HKDF-SHA256(ikm =
/// argon2_output || BLAKE3(keyfile), salt, info = "LitePack lpk v1 keyfile")`.
fn derive_kek(
    params: &Argon2Params,
    salt: &[u8; SALT_LEN],
    creds: &Credentials,
    use_keyfile: bool,
) -> Result<[u8; KEY_LEN], FormatError> {
    params.validate()?;
    let p = Params::new(params.m_kib, params.t, params.p, Some(KEY_LEN)).map_err(|_| {
        FormatError::BadArgon2 {
            reason: "parameters rejected",
        }
    })?;
    let mut out = [0u8; KEY_LEN];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
        .hash_password_into(&creds.password, salt, &mut out)
        .map_err(|_| FormatError::BadArgon2 {
            reason: "argon2 failed",
        })?;
    if !use_keyfile {
        return Ok(out);
    }
    let Some(kf) = &creds.keyfile else {
        out.zeroize();
        return Err(FormatError::WrongKey);
    };
    let mut ikm = Vec::with_capacity(2 * KEY_LEN);
    ikm.extend_from_slice(&out);
    ikm.extend_from_slice(blake3::hash(kf).as_bytes());
    out.zeroize();
    let mut kek = [0u8; KEY_LEN];
    let r = Hkdf::<Sha256>::new(Some(salt), &ikm).expand(KEYFILE_INFO, &mut kek);
    ikm.zeroize();
    r.map_err(|_| FormatError::BadArgon2 {
        reason: "hkdf failed",
    })?;
    Ok(kek)
}

/// The key slot: the archive key wrapped under a key derived from the credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySlot {
    /// Cipher suite of the archive.
    pub suite: Suite,
    /// Argon2id cost.
    pub argon2: Argon2Params,
    /// Argon2 salt.
    pub salt: [u8; SALT_LEN],
    /// True when a keyfile is part of the key derivation.
    pub keyfile_required: bool,
    /// The archive key encrypted under the KEK (key, then tag).
    pub wrapped_key: [u8; KEY_LEN + TAG_LEN],
    /// `BLAKE3` keyed with the archive key of the archive id.
    pub check: [u8; 32],
}

impl KeySlot {
    /// Wrap `key` under a KEK derived from `creds` with a fresh salt from `rng`.
    pub fn create(
        suite: Suite,
        argon2: Argon2Params,
        creds: &Credentials,
        archive_id: &[u8; 16],
        header_flags: u32,
        key: &ArchiveKey,
        rng: &mut dyn RngCore,
    ) -> Result<KeySlot, FormatError> {
        argon2.validate()?;
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        let keyfile_required = creds.keyfile.is_some();
        let mut kek = derive_kek(&argon2, &salt, creds, keyfile_required)?;
        let zero = vec![0u8; suite.nonce_len()];
        let wrapped = aead_encrypt(
            suite,
            &kek,
            &zero,
            &wrap_ad(archive_id, header_flags),
            &key.0,
        );
        kek.zeroize();
        let wrapped = wrapped.map_err(|_| FormatError::BadKeySlot { reason: "wrap" })?;
        let wrapped_key: [u8; KEY_LEN + TAG_LEN] = wrapped
            .try_into()
            .map_err(|_| FormatError::BadKeySlot { reason: "wrap" })?;
        Ok(KeySlot {
            suite,
            argon2,
            salt,
            keyfile_required,
            wrapped_key,
            check: *blake3::keyed_hash(&key.0, archive_id).as_bytes(),
        })
    }

    /// The payload bytes of the `KeySlot` frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(KEY_SLOT_LEN);
        v.push(self.suite as u8);
        v.push(KDF_ARGON2ID);
        v.extend_from_slice(&self.argon2.t.to_le_bytes());
        v.extend_from_slice(&self.argon2.m_kib.to_le_bytes());
        v.extend_from_slice(&self.argon2.p.to_le_bytes());
        v.extend_from_slice(&self.salt);
        v.push(u8::from(self.keyfile_required));
        v.extend_from_slice(&self.wrapped_key);
        v.extend_from_slice(&self.check);
        v
    }

    /// Parse and validate the payload of a `KeySlot` frame.
    pub fn parse(b: &[u8]) -> Result<KeySlot, FormatError> {
        let bad = |reason| FormatError::BadKeySlot { reason };
        if b.len() != KEY_SLOT_LEN {
            return Err(bad("length"));
        }
        let suite = Suite::from_u8(b[0]).ok_or(bad("suite"))?;
        if b[1] != KDF_ARGON2ID {
            return Err(bad("kdf"));
        }
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let argon2 = Argon2Params {
            t: u32_at(2),
            m_kib: u32_at(6),
            p: u32_at(10),
        };
        argon2.validate()?;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&b[14..30]);
        let keyfile_required = match b[30] {
            0 => false,
            1 => true,
            _ => return Err(bad("keyfile_required")),
        };
        let mut wrapped_key = [0u8; KEY_LEN + TAG_LEN];
        wrapped_key.copy_from_slice(&b[31..79]);
        let mut check = [0u8; 32];
        check.copy_from_slice(&b[79..111]);
        Ok(KeySlot {
            suite,
            argon2,
            salt,
            keyfile_required,
            wrapped_key,
            check,
        })
    }

    /// Recover the archive key. A wrong password, a wrong or missing keyfile
    /// and a damaged slot are all `WrongKey`.
    pub fn unwrap(
        &self,
        archive_id: &[u8; 16],
        header_flags: u32,
        creds: &Credentials,
    ) -> Result<ArchiveKey, FormatError> {
        let mut kek = derive_kek(&self.argon2, &self.salt, creds, self.keyfile_required)?;
        let zero = vec![0u8; self.suite.nonce_len()];
        let plain = aead_decrypt(
            self.suite,
            &kek,
            &zero,
            &wrap_ad(archive_id, header_flags),
            &self.wrapped_key,
        );
        kek.zeroize();
        let mut plain = plain.map_err(|()| FormatError::WrongKey)?;
        let key: Result<[u8; KEY_LEN], _> = plain.as_slice().try_into();
        plain.zeroize();
        let key = ArchiveKey(key.map_err(|_| FormatError::WrongKey)?);
        if !key.check(archive_id, self) {
            return Err(FormatError::WrongKey);
        }
        Ok(key)
    }
}

/// Seals and opens frame payloads under the archive key.
#[derive(Debug, Clone)]
pub struct Sealer {
    suite: Suite,
    key: ArchiveKey,
    archive_id: [u8; 16],
}

impl Sealer {
    /// A sealer for one archive.
    pub fn new(suite: Suite, key: ArchiveKey, archive_id: [u8; 16]) -> Self {
        Sealer {
            suite,
            key,
            archive_id,
        }
    }

    /// The suite in use.
    pub fn suite(&self) -> Suite {
        self.suite
    }

    /// Seal `payload` of a frame of `kind` at `sequence`: `nonce | ciphertext |
    /// tag`. The associated data names the sealed payload's length.
    pub fn seal(&self, payload: &[u8], kind: u16, sequence: u64) -> Result<Vec<u8>, FormatError> {
        let nl = self.suite.nonce_len();
        let nonce = derive_nonce(&self.key, &self.archive_id, kind, sequence, nl);
        let sealed_len = (payload.len() + nl + TAG_LEN) as u64;
        let ad = associated_data(&self.archive_id, kind, sequence, sealed_len);
        let ct = aead_encrypt(self.suite, &self.key.0, &nonce, &ad, payload)
            .map_err(|()| FormatError::SealFailed { kind, sequence })?;
        let mut out = nonce;
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open a sealed payload; `expected_len` is the frame's `payload_len` (the
    /// sealed length). A wrong nonce, tag, kind, sequence or length is
    /// `AuthenticationFailed`.
    pub fn open(
        &self,
        sealed: &[u8],
        kind: u16,
        sequence: u64,
        expected_len: u64,
    ) -> Result<Vec<u8>, FormatError> {
        let fail = FormatError::AuthenticationFailed { kind, sequence };
        let nl = self.suite.nonce_len();
        if sealed.len() as u64 != expected_len || sealed.len() < nl + TAG_LEN {
            return Err(fail);
        }
        let (nonce, ct) = sealed.split_at(nl);
        if nonce != derive_nonce(&self.key, &self.archive_id, kind, sequence, nl) {
            return Err(fail);
        }
        let ad = associated_data(&self.archive_id, kind, sequence, expected_len);
        aead_decrypt(self.suite, &self.key.0, nonce, &ad, ct).map_err(|()| fail)
    }
}

/// Whether the archive seals a frame of this kind: `Some(true)` it must be
/// sealed, `Some(false)` it must not be, `None` no rule (unknown kinds).
pub fn sealing_rule(kind: u16, listable: bool) -> Option<bool> {
    match FrameKind::from_u16(kind)? {
        FrameKind::ChunkData | FrameKind::Records | FrameKind::Index => Some(true),
        FrameKind::EntryTable => Some(!listable),
        FrameKind::KeySlot | FrameKind::Recovery | FrameKind::Trailer => Some(false),
    }
}

/// The Markdown table of the key slot payload, pasted verbatim into the spec.
pub fn key_slot_table() -> String {
    format!(
        "| Field | Size | Meaning |\n|---|---|---|\n\
         | suite | 1 | cipher suite: 1 = AES-256-GCM, 2 = XChaCha20-Poly1305 |\n\
         | kdf | 1 | key derivation: {KDF_ARGON2ID} = Argon2id |\n\
         | argon2_t | 4 | Argon2 passes, little-endian; 1 to {MAX_ARGON2_T} |\n\
         | argon2_m_kib | 4 | Argon2 memory in KiB, little-endian; at least {MIN_ARGON2_M_KIB} |\n\
         | argon2_p | 4 | Argon2 lanes, little-endian; 1 to {MAX_ARGON2_P} |\n\
         | salt | {SALT_LEN} | Argon2 salt |\n\
         | keyfile_required | 1 | 0 or 1: a keyfile is mixed into the key derivation |\n\
         | wrapped_key | {} | the archive key encrypted under the KEK (32 bytes of ciphertext, then the 16-byte tag) |\n\
         | check | 32 | BLAKE3 keyed with the archive key, of the archive id |\n",
        KEY_LEN + TAG_LEN
    )
}

/// The Markdown table of which frames are sealed, pasted verbatim into the spec.
pub fn sealing_rules_table() -> String {
    let mut s = String::from("| Kind | Name | Sealed in an encrypted archive |\n|---|---|---|\n");
    for k in FrameKind::ALL {
        let what = match sealing_rule(k as u16, false) {
            Some(true) => "yes",
            _ => "no",
        };
        let what = if k == FrameKind::EntryTable {
            "yes; no in a listable archive"
        } else {
            what
        };
        s.push_str(&format!("| {} | {} | {} |\n", k as u16, k.name(), what));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [u8; 16] = [7; 16];

    struct Counter(u64);

    impl RngCore for Counter {
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 11
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for b in dest {
                *b = (self.next_u64() >> 8) as u8;
            }
        }
    }

    fn small() -> Argon2Params {
        Argon2Params {
            t: 1,
            m_kib: 8192,
            p: 1,
        }
    }

    #[test]
    fn nonce_known_answer() {
        let key = ArchiveKey::from_bytes([0x11; 32]);
        let n12 = derive_nonce(&key, &[0x22; 16], 2, 5, 12);
        let n24 = derive_nonce(&key, &[0x22; 16], 2, 5, 24);
        assert_eq!(n12.len(), 12);
        assert_eq!(&n24[..12], &n12[..]);
        let hex: String = n12.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, KAT_NONCE12);
    }

    // Pinned and cross-checked against an independent HKDF-SHA256 (Python hmac); any change to the
    // derivation changes this value and breaks every sealed archive.
    const KAT_NONCE12: &str = "b8ad28ccb405d2d32dd4a80c";

    #[test]
    fn ad_layout() {
        let ad = associated_data(&ID, 0x0102, 0x0304, 0x0506);
        assert_eq!(&ad[..18], b"LitePack lpk v1 AD");
        assert_eq!(&ad[18..34], &ID);
        assert_eq!(&ad[34..36], &[2, 1]);
        assert_eq!(&ad[36..44], &0x0304u64.to_le_bytes());
        assert_eq!(&ad[44..52], &0x0506u64.to_le_bytes());
        assert_eq!(ad.len(), 52);
    }

    #[test]
    fn nonces_are_distinct_per_kind_and_sequence() {
        let key = ArchiveKey::from_bytes([3; 32]);
        let mut seen = std::collections::HashSet::new();
        for kind in 1..=7u16 {
            for seq in 0..500u64 {
                assert!(seen.insert(derive_nonce(&key, &ID, kind, seq, 12)));
            }
        }
        assert!(seen.insert(derive_nonce(&key, &ID, 5, INDEX_SEQUENCE, 12)));
    }

    #[test]
    fn key_slot_round_trip_both_suites() {
        for suite in [Suite::AesGcm, Suite::XChaCha] {
            let key = ArchiveKey::generate(&mut Counter(1));
            let creds = Credentials::password(b"hunter2".to_vec());
            let slot =
                KeySlot::create(suite, small(), &creds, &ID, 1, &key, &mut Counter(9)).unwrap();
            let bytes = slot.encode();
            assert_eq!(bytes.len(), KEY_SLOT_LEN);
            assert_eq!(KeySlot::parse(&bytes).unwrap(), slot);
            assert!(slot.unwrap(&ID, 1, &creds).unwrap().0 == key.0);
            assert!(key.check(&ID, &slot));
            let wrong = Credentials::password(b"hunter3".to_vec());
            assert!(matches!(
                slot.unwrap(&ID, 1, &wrong),
                Err(FormatError::WrongKey)
            ));
            // The archive id is bound.
            assert!(matches!(
                slot.unwrap(&[8; 16], 1, &creds),
                Err(FormatError::WrongKey)
            ));
        }
    }

    #[test]
    fn keyfile_is_part_of_the_key() {
        let key = ArchiveKey::generate(&mut Counter(2));
        let creds = Credentials {
            password: b"pw".to_vec(),
            keyfile: Some(b"file-bytes".to_vec()),
        };
        let slot = KeySlot::create(
            Suite::AesGcm,
            small(),
            &creds,
            &ID,
            1,
            &key,
            &mut Counter(3),
        )
        .unwrap();
        assert!(slot.keyfile_required);
        assert!(slot.unwrap(&ID, 1, &creds).unwrap().0 == key.0);
        // The header flags are bound.
        assert!(matches!(
            slot.unwrap(&ID, 3, &creds),
            Err(FormatError::WrongKey)
        ));
        let no_file = Credentials::password(b"pw".to_vec());
        assert!(matches!(
            slot.unwrap(&ID, 1, &no_file),
            Err(FormatError::WrongKey)
        ));
        let other = Credentials {
            password: b"pw".to_vec(),
            keyfile: Some(b"other".to_vec()),
        };
        assert!(matches!(
            slot.unwrap(&ID, 1, &other),
            Err(FormatError::WrongKey)
        ));
    }

    #[test]
    fn key_slot_parse_rules() {
        let key = ArchiveKey::generate(&mut Counter(2));
        let creds = Credentials::password(b"pw".to_vec());
        let good = KeySlot::create(
            Suite::AesGcm,
            small(),
            &creds,
            &ID,
            1,
            &key,
            &mut Counter(3),
        )
        .unwrap()
        .encode();
        let with = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut b = good.clone();
            f(&mut b);
            KeySlot::parse(&b)
        };
        assert!(matches!(
            with(&|b| b.push(0)),
            Err(FormatError::BadKeySlot { reason: "length" })
        ));
        assert!(matches!(
            with(&|b| b[0] = 9),
            Err(FormatError::BadKeySlot { reason: "suite" })
        ));
        assert!(matches!(
            with(&|b| b[1] = 2),
            Err(FormatError::BadKeySlot { reason: "kdf" })
        ));
        assert!(matches!(
            with(&|b| b[30] = 2),
            Err(FormatError::BadKeySlot {
                reason: "keyfile_required"
            })
        ));
        assert!(matches!(
            with(&|b| b[2..6].copy_from_slice(&0u32.to_le_bytes())),
            Err(FormatError::BadArgon2 { reason: "t" })
        ));
        assert!(matches!(
            with(&|b| b[6..10].copy_from_slice(&8191u32.to_le_bytes())),
            Err(FormatError::BadArgon2 { reason: "m" })
        ));
        assert!(matches!(
            with(&|b| b[10..14].copy_from_slice(&0u32.to_le_bytes())),
            Err(FormatError::BadArgon2 { reason: "p" })
        ));
        // A damaged wrapped key is a wrong key.
        let mut bad = KeySlot::parse(&good).unwrap();
        bad.wrapped_key[0] ^= 1;
        assert!(matches!(
            bad.unwrap(&ID, 1, &creds),
            Err(FormatError::WrongKey)
        ));
    }

    #[test]
    fn argon2_bounds() {
        assert!(Argon2Params::default().validate().is_ok());
        let d = Argon2Params::default();
        assert!(Argon2Params { t: 0, ..d }.validate().is_err());
        assert!(Argon2Params { t: 65, ..d }.validate().is_err());
        assert!(Argon2Params { m_kib: 8191, ..d }.validate().is_err());
        assert!(Argon2Params { p: 0, ..d }.validate().is_err());
        assert!(Argon2Params { p: 65, ..d }.validate().is_err());
        assert!(small().validate().is_ok());
    }

    #[test]
    fn sealer_round_trip_and_binding() {
        for suite in [Suite::AesGcm, Suite::XChaCha] {
            let s = Sealer::new(suite, ArchiveKey::from_bytes([5; 32]), ID);
            let plain = b"some frame payload".to_vec();
            let sealed = s.seal(&plain, 2, 4).unwrap();
            assert_eq!(sealed.len(), plain.len() + suite.overhead());
            let n = sealed.len() as u64;
            assert_eq!(s.open(&sealed, 2, 4, n).unwrap(), plain);
            for (kind, seq, len) in [(3, 4, n), (2, 5, n), (2, 4, n + 1)] {
                assert!(matches!(
                    s.open(&sealed, kind, seq, len),
                    Err(FormatError::AuthenticationFailed { .. })
                ));
            }
            for i in 0..sealed.len() {
                let mut t = sealed.clone();
                t[i] ^= 1;
                assert!(s.open(&t, 2, 4, n).is_err(), "byte {i}");
            }
            assert!(s.open(&sealed[..10], 2, 4, 10).is_err());
            // An empty payload seals to nonce and tag only.
            let e = s.seal(&[], 3, 0).unwrap();
            assert_eq!(e.len(), suite.overhead());
            assert!(s.open(&e, 3, 0, e.len() as u64).unwrap().is_empty());
        }
    }

    #[test]
    fn secrets_are_zeroized() {
        fn zod<T: ZeroizeOnDrop>() {}
        zod::<Credentials>();
        zod::<ArchiveKey>();
        let mut c = Credentials {
            password: b"secret".to_vec(),
            keyfile: Some(b"kf".to_vec()),
        };
        c.zeroize();
        assert!(c.password.is_empty() && c.keyfile.is_none());
        let mut k = ArchiveKey::from_bytes([9; 32]);
        k.zeroize();
        assert_eq!(k.0, [0; 32]);
        assert!(!format!("{c:?}").contains("secret"));
    }

    #[test]
    fn tables() {
        assert!(key_slot_table().contains("| wrapped_key | 48 |"));
        let t = sealing_rules_table();
        assert!(t.contains("| 2 | ChunkData | yes |"));
        assert!(t.contains("| 4 | Recovery | no |"));
        assert!(t.contains("| 1 | EntryTable | yes; no in a listable archive |"));
        assert_eq!(sealing_rule(1, true), Some(false));
        assert_eq!(sealing_rule(0x9000, false), None);
    }
}
