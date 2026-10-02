//! `encrypted-random`: incompressible data, plain and AES-encrypted.
//!
//! `plain.bin` is the ChaCha20 keystream under a fixed key and nonce (a cryptographic generator
//! with a fixed seed); `encrypted.bin` is the same bytes encrypted with AES-256-CTR under a fixed
//! key and counter block. Both keys come from BLAKE3 hashes of fixed labels, so the files are the
//! same on every machine. The keys are public test constants, not secrets.

use std::io::Write;

use anyhow::{Context, Result};
use chacha20::cipher::{KeyIvInit, StreamCipher};

use super::Output;
use crate::corpus::registry::EncryptedRandomSpec;

/// File name of the random bytes.
pub const PLAIN: &str = "plain.bin";
/// File name of the same bytes under AES-256-CTR.
pub const ENCRYPTED: &str = "encrypted.bin";

/// Fixed key material for `label` (`len` bytes of the BLAKE3 hash of the label).
pub fn fixed_bytes(label: &str, len: usize) -> Vec<u8> {
    blake3::hash(label.as_bytes()).as_bytes()[..len].to_vec()
}

/// The two ciphers; also used by the tests to decrypt.
pub fn ciphers() -> Result<(chacha20::ChaCha20, ctr::Ctr128BE<aes::Aes256>)> {
    let rng = chacha20::ChaCha20::new_from_slices(
        &fixed_bytes("lpk-bench corpus encrypted-random v1: chacha20 key", 32),
        &fixed_bytes("lpk-bench corpus encrypted-random v1: chacha20 nonce", 12),
    )
    .map_err(|e| anyhow::anyhow!("chacha20 setup: {e}"))?;
    let aes = ctr::Ctr128BE::<aes::Aes256>::new_from_slices(
        &fixed_bytes("lpk-bench corpus encrypted-random v1: aes-256 key", 32),
        &fixed_bytes("lpk-bench corpus encrypted-random v1: aes-ctr iv", 16),
    )
    .map_err(|e| anyhow::anyhow!("aes setup: {e}"))?;
    Ok((rng, aes))
}

pub fn build(spec: &EncryptedRandomSpec, out: &mut Output<'_>) -> Result<()> {
    let (mut rng, mut aes) = ciphers()?;
    let plain_path = out.path_for(PLAIN)?;
    let enc_path = out.path_for(ENCRYPTED)?;
    let mut plain = std::io::BufWriter::new(
        std::fs::File::create(&plain_path)
            .with_context(|| format!("creating {}", plain_path.display()))?,
    );
    let mut enc = std::io::BufWriter::new(
        std::fs::File::create(&enc_path)
            .with_context(|| format!("creating {}", enc_path.display()))?,
    );
    let (mut hp, mut he) = (blake3::Hasher::new(), blake3::Hasher::new());
    let mut buf = vec![0u8; 1 << 20];
    let mut left = spec.bytes;
    while left > 0 {
        let n = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
        let chunk = &mut buf[..n];
        chunk.fill(0);
        rng.apply_keystream(chunk);
        plain.write_all(chunk)?;
        hp.update(chunk);
        aes.apply_keystream(chunk);
        enc.write_all(chunk)?;
        he.update(chunk);
        left -= n as u64;
    }
    plain.flush()?;
    enc.flush()?;
    out.record(PLAIN, spec.bytes, hp.finalize().to_hex().to_string());
    out.record(ENCRYPTED, spec.bytes, he.finalize().to_hex().to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{read_all, run, source};
    use super::*;
    use crate::corpus::registry::SourceSpec;

    fn src(bytes: u64) -> crate::corpus::registry::Source {
        source(
            "enc",
            "encrypted-random",
            &[],
            SourceSpec::EncryptedRandom(EncryptedRandomSpec { bytes }),
        )
    }

    #[test]
    fn two_runs_are_identical_and_the_encrypted_file_decrypts_to_the_random_one() {
        // Not a multiple of the chunk size, spanning several chunks.
        let bytes = (3 << 20) + 12_345;
        let a = tempfile::tempdir().expect("tmp");
        let b = tempfile::tempdir().expect("tmp");
        let ma = run(a.path(), &src(bytes), &[]).expect("run a");
        let mb = run(b.path(), &src(bytes), &[]).expect("run b");
        assert_eq!(ma, mb, "manifest entries must repeat");
        let files = read_all(a.path(), "encrypted-random", "enc");
        assert_eq!(
            files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(),
            [ENCRYPTED, PLAIN]
        );
        assert_eq!(files, read_all(b.path(), "encrypted-random", "enc"));
        let (enc, plain) = (&files[0].1, &files[1].1);
        assert_eq!(enc.len() as u64, bytes);
        assert_eq!(plain.len() as u64, bytes);
        assert_ne!(enc, plain);

        let (_, mut aes) = ciphers().expect("ciphers");
        let mut back = enc.clone();
        aes.apply_keystream(&mut back);
        assert_eq!(
            &back, plain,
            "AES-CTR decryption must give back the random file"
        );
        // The recorded hashes are those of the files on disk.
        for m in &ma {
            let data = if m.path.ends_with(PLAIN) { plain } else { enc };
            assert_eq!(m.blake3, blake3::hash(data).to_hex().to_string());
            assert_eq!(m.bytes, bytes);
        }
        // Random-looking: every byte value occurs.
        let mut seen = [false; 256];
        for &x in plain.iter().take(1 << 16) {
            seen[x as usize] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }
}
