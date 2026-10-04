//! Encryption (section 14): the key slot, the key derivation, nonces and sealed payloads.

use crate::error::{Error, Result};
use crate::wire::{kind, Cursor};
use aes_gcm::aead::{Aead, KeyInit, Payload};

/// A cipher suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    /// AES-256-GCM.
    Aes256Gcm,
    /// XChaCha20-Poly1305.
    XChaCha20Poly1305,
}

impl Suite {
    /// The nonce length.
    pub fn nonce_len(self) -> usize {
        match self {
            Self::Aes256Gcm => 12,
            Self::XChaCha20Poly1305 => 24,
        }
    }
}

/// The parsed key slot.
#[derive(Debug, Clone)]
pub struct KeySlot {
    /// Cipher suite.
    pub suite: Suite,
    /// Argon2 passes.
    pub t: u32,
    /// Argon2 memory in KiB.
    pub m_kib: u32,
    /// Argon2 lanes.
    pub p: u32,
    /// Argon2 salt.
    pub salt: [u8; 16],
    /// Whether a keyfile is mixed in.
    pub keyfile_required: bool,
    /// The wrapped archive key and tag.
    pub wrapped: [u8; 48],
    /// Keyed BLAKE3 of the archive id.
    pub check: [u8; 32],
}

/// The key slot payload length.
pub const KEY_SLOT_LEN: usize = 111;

/// `Refused` (field `argon2_m`) when the slot's Argon2 memory exceeds the reader's `memory`;
/// checked only when credentials are given (section 6 step 3).
pub fn check_argon2_memory(slot: &KeySlot, memory: u64) -> Result<()> {
    let need = u64::from(slot.m_kib) * 1024;
    if need > memory {
        return Err(Error::new(
            "Refused",
            format!("the archive needs argon2_m of {need} bytes; this reader allows {memory}"),
        ));
    }
    Ok(())
}

/// Parses a key slot payload in the order of section 14 ("Reading the key slot").
pub fn parse_key_slot(payload: &[u8]) -> Result<KeySlot> {
    let bad = |r: &str| Error::new("BadKeySlot", format!("bad key slot: {r}"));
    if payload.len() != KEY_SLOT_LEN {
        return Err(bad("length"));
    }
    let mut c = Cursor::new(payload, "key slot");
    let suite = match c.u8()? {
        1 => Suite::Aes256Gcm,
        2 => Suite::XChaCha20Poly1305,
        _ => return Err(bad("suite")),
    };
    if c.u8()? != 1 {
        return Err(bad("kdf"));
    }
    let argon = |r: &str| Error::new("BadArgon2", format!("argon2 parameter out of bounds: {r}"));
    let t = c.u32()?;
    let m_kib = c.u32()?;
    let p = c.u32()?;
    if !(1..=64).contains(&t) {
        return Err(argon("t"));
    }
    if m_kib < 8192 {
        return Err(argon("m"));
    }
    if !(1..=64).contains(&p) {
        return Err(argon("p"));
    }
    let salt = c.array()?;
    let keyfile_required = match c.u8()? {
        0 => false,
        1 => true,
        _ => return Err(bad("keyfile_required")),
    };
    let wrapped = c.array()?;
    let check = c.array()?;
    Ok(KeySlot {
        suite,
        t,
        m_kib,
        p,
        salt,
        keyfile_required,
        wrapped,
        check,
    })
}

fn aead_open(suite: Suite, key: &[u8; 32], nonce: &[u8], ct: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    let payload = Payload { msg: ct, aad };
    match suite {
        Suite::Aes256Gcm => {
            let c = aes_gcm::Aes256Gcm::new_from_slice(key).ok()?;
            let n = aes_gcm::Nonce::<aes_gcm::aead::consts::U12>::try_from(nonce).ok()?;
            c.decrypt(&n, payload).ok()
        }
        Suite::XChaCha20Poly1305 => {
            let c = chacha20poly1305::XChaCha20Poly1305::new_from_slice(key).ok()?;
            let n = chacha20poly1305::XNonce::try_from(nonce).ok()?;
            c.decrypt(&n, payload).ok()
        }
    }
}

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) -> Result<()> {
    hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .map_err(|_| Error::new("Internal", "hkdf output length"))
}

/// Unwraps the archive key: Argon2id, optionally the keyfile mix, the key wrap and the check.
pub fn unwrap_key(
    slot: &KeySlot,
    password: &[u8],
    keyfile: Option<&[u8]>,
    archive_id: &[u8; 16],
    header_flags: u32,
) -> Result<[u8; 32]> {
    let wrong = || Error::new("WrongKey", "wrong password or keyfile");
    let params = argon2::Params::new(slot.m_kib, slot.t, slot.p, Some(32))
        .map_err(|e| Error::new("BadArgon2", format!("argon2 parameters: {e}")))?;
    let a2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut kek = [0u8; 32];
    a2.hash_password_into(password, &slot.salt, &mut kek)
        .map_err(|e| Error::new("BadArgon2", format!("argon2: {e}")))?;
    if slot.keyfile_required {
        let kf = keyfile.ok_or_else(wrong)?;
        let mut ikm = Vec::with_capacity(64);
        ikm.extend_from_slice(&kek);
        ikm.extend_from_slice(blake3::hash(kf).as_bytes());
        hkdf32(&ikm, &slot.salt, b"LitePack lpk v1 keyfile", &mut kek)?;
    }
    let mut aad = b"LitePack lpk v1 keywrap".to_vec();
    aad.extend_from_slice(archive_id);
    aad.extend_from_slice(&header_flags.to_le_bytes());
    let nonce = vec![0u8; slot.suite.nonce_len()];
    let k = aead_open(slot.suite, &kek, &nonce, &slot.wrapped, &aad).ok_or_else(wrong)?;
    let key: [u8; 32] = k.as_slice().try_into().map_err(|_| wrong())?;
    if blake3::keyed_hash(&key, archive_id).as_bytes() != &slot.check {
        return Err(wrong());
    }
    Ok(key)
}

/// Opens sealed frames of one archive.
#[derive(Debug, Clone)]
pub struct Sealer {
    key: [u8; 32],
    suite: Suite,
    archive_id: [u8; 16],
}

impl Sealer {
    /// A sealer for an archive key.
    pub fn new(key: [u8; 32], suite: Suite, archive_id: [u8; 16]) -> Self {
        Self {
            key,
            suite,
            archive_id,
        }
    }

    /// The nonce of a frame (section 14).
    pub fn nonce(&self, frame_kind: u16, sequence: u64, salt: &[u8; 16]) -> Result<Vec<u8>> {
        let mut info = b"LitePack lpk v1 nonce".to_vec();
        info.extend_from_slice(&frame_kind.to_le_bytes());
        info.extend_from_slice(&sequence.to_le_bytes());
        info.extend_from_slice(salt);
        let mut out = vec![0u8; self.suite.nonce_len()];
        hkdf32(&self.key, &self.archive_id, &info, &mut out)?;
        Ok(out)
    }

    /// Opens a sealed payload (`nonce | ciphertext | tag`) of a frame.
    pub fn open(
        &self,
        frame_kind: u16,
        sequence: u64,
        salt: &[u8; 16],
        stored: &[u8],
    ) -> Result<Vec<u8>> {
        let fail = || {
            Error::new(
                "AuthenticationFailed",
                format!("authentication failed for frame kind {frame_kind} sequence {sequence}"),
            )
        };
        let nl = self.suite.nonce_len();
        if stored.len() < nl + 16 {
            return Err(fail());
        }
        let nonce = self.nonce(frame_kind, sequence, salt)?;
        if stored[..nl] != nonce[..] {
            return Err(fail());
        }
        let mut aad = b"LitePack lpk v1 AD".to_vec();
        aad.extend_from_slice(&self.archive_id);
        aad.extend_from_slice(&frame_kind.to_le_bytes());
        aad.extend_from_slice(&sequence.to_le_bytes());
        aad.extend_from_slice(&(stored.len() as u64).to_le_bytes());
        aead_open(self.suite, &self.key, &nonce, &stored[nl..], &aad).ok_or_else(fail)
    }
}

/// Whether a frame kind is sealed in an encrypted archive (section 14 table).
pub fn sealed_kind(frame_kind: u16, listable: bool) -> Option<bool> {
    match frame_kind {
        kind::ENTRY_TABLE => Some(!listable),
        kind::CHUNK_DATA | kind::RECORDS | kind::INDEX => Some(true),
        kind::RECOVERY | kind::TRAILER | kind::KEY_SLOT => Some(false),
        _ => None,
    }
}

/// Checks the SEALED flag of a known frame against the table.
pub fn check_seal_flag(
    frame_kind: u16,
    sealed: bool,
    encrypted: bool,
    listable: bool,
) -> Result<()> {
    let want = encrypted && sealed_kind(frame_kind, listable).unwrap_or(sealed);
    if sealed && !want {
        return Err(Error::new(
            "UnexpectedSealedFrame",
            format!("frame kind {frame_kind} is sealed but must not be"),
        ));
    }
    if !sealed && want {
        return Err(Error::new(
            "UnsealedFrame",
            format!("frame kind {frame_kind} is not sealed but must be"),
        ));
    }
    Ok(())
}
