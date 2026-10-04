//! Helpers shared by the whole-archive fuzz targets.
use lpk_format::{
    Archive, Credentials, Frame, Header, KeySlot, MemoryPriors, ReadFrame, ReadLimits, Resources,
    KEY_SLOT_LEN,
};
use std::io::Cursor;

/// The dictionary `zstd-dict.lpk` needs.
pub const DICT: &[u8] = include_bytes!("../../crates/lpk-format/tests/vectors/zstd-dict.prior");

/// Tight limits: a decoder may allocate at most these.
pub fn tight() -> Resources {
    Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    }
}

/// True when the bytes after the header parse as a key slot whose Argon2 cost would make a
/// run slow (more than 2 passes or 16 MiB): such inputs are skipped.
pub fn key_slot_too_costly(data: &[u8]) -> bool {
    if data.len() <= Header::LEN {
        return false;
    }
    let mut r = &data[Header::LEN..];
    let limits = ReadLimits {
        max_payload: KEY_SLOT_LEN as u64,
    };
    match Frame::read(&mut r, &limits) {
        Ok(Some(ReadFrame::Known(f))) => match KeySlot::parse(&f.payload) {
            Ok(slot) => slot.argon2.t > 2 || slot.argon2.m_kib > 16384,
            Err(_) => false,
        },
        _ => false,
    }
}

/// Open (with the dictionary prior set), verify, and extract every entry into a sink.
pub fn exercise(data: &[u8], creds: Option<&Credentials>) {
    let Ok(mut a) = Archive::open_with(Cursor::new(data.to_vec()), &tight(), creds) else {
        return;
    };
    let mut store = MemoryPriors::new();
    store.insert(DICT.to_vec());
    a.set_priors(Box::new(store));
    let _ = a.verify();
    let _ = a.history();
    let Ok(table) = a.entry_table() else {
        return;
    };
    let Ok(t) = table.table() else {
        return;
    };
    let entries: Vec<_> = t.iter().filter_map(Result::ok).collect();
    for e in entries {
        let _ = a.extract(&e, &mut std::io::sink());
    }
}
