//! A deterministic, portable cousin of the `archive_mutate` fuzz target (`fuzz/`): seeded
//! mutations (byte XOR over the full range, insert, delete, append, truncate) of every
//! committed vector, then open, verify, extract, keyless recovery scan and repair.
//! Nothing may panic; every error is fine.
//!
//! `LPK_MUTATE_ITERS` raises the iteration count per vector (default 150). A long run
//! is meant for `--release`, which has no debug assertions or overflow checks, so it
//! does not replace the coverage-guided fuzz run (`fuzz/README.md`).
#![allow(clippy::unwrap_used)]

mod common;

use common::recovery::RECOVERY_VECTOR;
use common::{vectors_dir, DICT_FILE, VECTORS};
use lpk_format::{
    repair, Archive, Credentials, Frame, Header, KeySlot, MemoryPriors, ReadFrame, ReadLimits,
    Resources, KEY_SLOT_LEN,
};
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

/// A mutated key slot may ask for a costly Argon2: such inputs are skipped, as in the fuzz targets.
fn key_slot_too_costly(data: &[u8]) -> bool {
    if data.len() <= Header::LEN {
        return false;
    }
    let mut r = &data[Header::LEN..];
    let limits = ReadLimits {
        max_payload: KEY_SLOT_LEN as u64,
    };
    match Frame::read(&mut r, &limits) {
        Ok(Some(ReadFrame::Known(f))) => KeySlot::parse(&f.payload)
            .map(|s| s.argon2.t > 2 || s.argon2.m_kib > 16384)
            .unwrap_or(false),
        _ => false,
    }
}

fn exercise(bytes: &[u8], creds: Option<&Credentials>) {
    if let Ok(mut a) = Archive::open_with(Cursor::new(bytes.to_vec()), &tight(), creds) {
        let mut store = MemoryPriors::new();
        store.insert(std::fs::read(vectors_dir().join(DICT_FILE)).unwrap());
        a.set_priors(Box::new(store));
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
    if key_slot_too_costly(bytes) {
        return;
    }
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

/// One edit; returns its description for the failure message.
fn mutate(rng: &mut Rng, bytes: &mut Vec<u8>) -> String {
    let r = rng.next();
    let len = bytes.len();
    // A third of the edits land in the first or last 512 bytes (header, trailer).
    let off = match (r >> 3) % 6 {
        0 => (r >> 8) as usize % 512.min(len),
        1 => len - 1 - (r >> 8) as usize % 512.min(len),
        _ => (r >> 8) as usize % len,
    };
    let val = (r >> 40) as u8;
    match r % 8 {
        0..=3 => {
            let v = val.max(1);
            bytes[off] ^= v;
            format!("xor {off} {v:#x}")
        }
        4 => {
            bytes.insert(off, val);
            format!("insert {off} {val:#x}")
        }
        5 => {
            let n = 1 + (r >> 48) as usize % 16;
            let end = (off + n).min(len);
            bytes.drain(off..end);
            format!("delete {off}..{end}")
        }
        6 => {
            let n = 1 + (r >> 48) as usize % 64;
            let tail: Vec<u8> = (0..n).map(|i| (r >> (i % 8 * 8)) as u8).collect();
            bytes.extend_from_slice(&tail);
            format!("append {n} bytes")
        }
        _ => {
            bytes.truncate(off.max(1));
            format!("truncate {off}")
        }
    }
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
    names.push(RECOVERY_VECTOR);
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
            let mut edits = Vec::new();
            for _ in 0..1 + rng.next() % 4 {
                if bytes.is_empty() {
                    break;
                }
                edits.push(mutate(&mut rng, &mut bytes));
            }
            let res = catch_unwind(AssertUnwindSafe(|| run_all(&bytes, creds.as_ref())));
            assert!(res.is_ok(), "{name} iteration {i}: panic after {edits:?}");
        }
    }
}
