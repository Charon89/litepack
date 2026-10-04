//! A deterministic, portable cousin of the `archive_mutate` fuzz target (`fuzz/`): seeded
//! byte mutations of every committed vector, then open, verify, extract, keyless recovery
//! scan and repair. Nothing may panic; every error is fine. `LPK_MUTATE_ITERS` raises the
//! iteration count per vector (default 150).
#![allow(clippy::unwrap_used)]

mod common;

use common::{vectors_dir, VECTORS};
use lpk_format::{repair, Archive, Credentials, Resources};
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};

fn tight() -> Resources {
    Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn exercise(bytes: &[u8], creds: Option<&Credentials>) {
    if let Ok(mut a) = Archive::open_with(Cursor::new(bytes.to_vec()), &tight(), creds) {
        let _ = a.verify();
        let _ = a.history();
        if let Ok(table) = a.entry_table() {
            if let Ok(t) = table.table() {
                let entries: Vec<_> = t.iter().filter_map(Result::ok).collect();
                for e in entries {
                    let _ = a.extract(&e, &mut std::io::sink());
                }
            }
        }
    }
}

fn run_all(bytes: &[u8], creds: Option<&Credentials>) {
    exercise(bytes, creds);
    if creds.is_some() {
        exercise(bytes, None);
    }
    if let Ok(mut a) = Archive::open_with(Cursor::new(bytes.to_vec()), &tight(), None) {
        let _ = a.check_recovery();
    }
    let mut out = Cursor::new(Vec::new());
    let _ = repair(Cursor::new(bytes.to_vec()), &mut out, &tight());
}

#[test]
fn mutated_vectors_never_panic() {
    let iters: u64 = std::env::var("LPK_MUTATE_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let mut names: Vec<&str> = VECTORS.to_vec();
    names.extend(common::sealed::SEALED_VECTORS);
    names.push(common::journal::JOURNAL_VECTOR);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for name in names {
        let orig = std::fs::read(vectors_dir().join(name)).unwrap();
        let sealed = name.starts_with("sealed-");
        let creds = sealed.then(|| Credentials {
            password: common::sealed::sealed_password(name).as_bytes().to_vec(),
            keyfile: (name == "sealed-keyfile.lpk").then(common::sealed::sealed_keyfile_bytes),
        });
        // Argon2 makes a sealed open costly: fewer iterations there.
        let n = if sealed { iters / 10 + 1 } else { iters };
        for i in 0..n {
            let mut bytes = orig.clone();
            let count = 1 + rng.next() % 4;
            let mut edits = Vec::new();
            for _ in 0..count {
                // Half of the edits land in the first or last 512 bytes (header, trailer).
                let r = rng.next();
                let off = match r % 4 {
                    0 => (r >> 8) as usize % 512.min(bytes.len()),
                    1 => bytes.len() - 1 - (r >> 8) as usize % 512.min(bytes.len()),
                    _ => (r >> 8) as usize % bytes.len(),
                };
                let val = (r >> 40) as u8 | 1;
                bytes[off] ^= val;
                edits.push((off, val));
            }
            let cut = rng
                .next()
                .is_multiple_of(8)
                .then(|| rng.next() as usize % bytes.len());
            if let Some(c) = cut {
                bytes.truncate(c);
            }
            let res = catch_unwind(AssertUnwindSafe(|| run_all(&bytes, creds.as_ref())));
            assert!(
                res.is_ok(),
                "{name} iteration {i}: panic with edits {edits:?} (xor) and cut {cut:?}"
            );
        }
    }
}
