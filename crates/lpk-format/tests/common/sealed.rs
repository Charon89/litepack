//! Sealed (encrypted) vectors and helpers shared by the tests.
#![allow(dead_code, clippy::unwrap_used)]

use super::pattern;
use lpk_format::{Argon2Params, Credentials, SealOptions, Suite, Writer, WriterOptions};
use std::io::Write;

/// A deterministic RNG (SplitMix64) for reproducible sealed archives.
pub struct SeedRng(pub u64);

impl rand::RngCore for SeedRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let b = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
    }
}

pub const SEALED_VECTORS: [&str; 4] = [
    "sealed-aes.lpk",
    "sealed-xchacha.lpk",
    "sealed-listable.lpk",
    "sealed-keyfile.lpk",
];

pub const SEALED_KEYFILE: &str = "sealed-keyfile.key";

pub fn sealed_keyfile_bytes() -> Vec<u8> {
    pattern(99, 200)
}

pub fn sealed_password(name: &str) -> &'static str {
    match name {
        "sealed-aes.lpk" => "correct horse",
        "sealed-xchacha.lpk" => "battery staple",
        "sealed-listable.lpk" => "list me",
        "sealed-keyfile.lpk" => "two factors",
        other => panic!("no sealed vector {other}"),
    }
}

pub fn small_argon2() -> Argon2Params {
    Argon2Params {
        t: 1,
        m_kib: 8192,
        p: 1,
    }
}

pub fn sealed_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("s/a.txt", pattern(81, 700)),
        ("s/b.txt", pattern(82, 9_000)),
        ("s/c.txt", pattern(83, 20_000)),
    ]
}

pub fn seal_options(
    suite: Suite,
    listable: bool,
    password: &str,
    keyfile: Option<Vec<u8>>,
) -> WriterOptions {
    WriterOptions {
        chunk_size: 4096,
        block_size: 8 * 1024,
        archive_id: [0x6B; 16],
        seal: Some(SealOptions {
            suite,
            argon2: small_argon2(),
            listable,
            credentials: Credentials {
                password: password.as_bytes().to_vec(),
                keyfile,
            },
        }),
        ..WriterOptions::default()
    }
}

pub fn sealed_options(name: &str) -> WriterOptions {
    let pw = sealed_password(name);
    match name {
        "sealed-aes.lpk" => seal_options(Suite::AesGcm, false, pw, None),
        "sealed-xchacha.lpk" => seal_options(Suite::XChaCha, false, pw, None),
        "sealed-listable.lpk" => seal_options(Suite::AesGcm, true, pw, None),
        _ => seal_options(Suite::AesGcm, false, pw, Some(sealed_keyfile_bytes())),
    }
}

pub fn write_archive_seeded(
    options: WriterOptions,
    files: &[(&str, Vec<u8>)],
    seed: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rng = SeedRng(seed);
    let mut w = Writer::new_with_rng(&mut out as &mut dyn Write, options, &mut rng).unwrap();
    for (path, data) in files {
        w.add_file(
            path,
            lpk_format::EntryFlags::EMPTY,
            1_000,
            &mut data.as_slice(),
        )
        .unwrap();
    }
    w.finish().unwrap();
    out
}

pub fn build_sealed_vector(name: &str) -> Vec<u8> {
    write_archive_seeded(sealed_options(name), &sealed_files(), 0x5EA1)
}

pub fn vectors_toml() -> String {
    let mut s = String::from("# Test passwords of the sealed vectors (not secrets).\n");
    for name in SEALED_VECTORS {
        s.push_str(&format!(
            "\n[\"{name}\"]\npassword = \"{}\"\n",
            sealed_password(name)
        ));
        if name == "sealed-keyfile.lpk" {
            s.push_str(&format!("keyfile = \"{SEALED_KEYFILE}\"\n"));
        }
    }
    s
}
