#![no_main]
//! Structure-aware: the input picks a committed vector and a list of byte
//! mutations to apply to it, so the fuzzer starts from a valid archive and every
//! reader path is reached before the first damaged byte is.
//!
//! Layout: byte 0 = flags and vector (bits 0..4 vector index, bit 7 = truncate the
//! archive at the first mutation's offset); then 5-byte records: offset (u32 LE,
//! taken modulo the archive length) and the value XORed into that byte (0 is
//! replaced by 0xFF so every record changes something).
use libfuzzer_sys::fuzz_target;
use lpk_format::{repair, Archive, Credentials, Resources};
use std::io::Cursor;

macro_rules! vector {
    ($name:literal) => {
        include_bytes!(concat!("../../crates/lpk-format/tests/vectors/", $name))
    };
}

/// (bytes, password, keyfile)
type Vector = (&'static [u8], Option<&'static str>, Option<&'static [u8]>);

fn vectors() -> [Vector; 12] {
    let key: &'static [u8] = vector!("sealed-keyfile.key");
    [
        (vector!("zstd-basic.lpk"), None, None),
        (vector!("zstd-multiblock.lpk"), None, None),
        (vector!("zstd-dict.lpk"), None, None),
        (vector!("zstd-window.lpk"), None, None),
        (vector!("lzma-basic.lpk"), None, None),
        (vector!("lzma-multiblock.lpk"), None, None),
        (vector!("lzma-props.lpk"), None, None),
        (vector!("sealed-aes.lpk"), Some("correct horse"), None),
        (vector!("sealed-xchacha.lpk"), Some("battery staple"), None),
        (vector!("sealed-listable.lpk"), Some("list me"), None),
        (
            vector!("sealed-keyfile.lpk"),
            Some("two factors"),
            Some(key),
        ),
        (vector!("journal-3gen.lpk"), None, None),
    ]
}

fn tight() -> Resources {
    Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    }
}

fn open_and_extract(bytes: &[u8], creds: Option<&Credentials>) {
    let Ok(mut a) = Archive::open_with(Cursor::new(bytes.to_vec()), &tight(), creds) else {
        return;
    };
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

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let table = vectors();
    let (orig, password, keyfile) = table[usize::from(first & 0x0F) % table.len()];
    let mut bytes = orig.to_vec();
    let mut first_offset = None;
    for rec in rest.chunks_exact(5).take(64) {
        let off = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]) as usize % bytes.len();
        first_offset.get_or_insert(off);
        bytes[off] ^= if rec[4] == 0 { 0xFF } else { rec[4] };
    }
    if first & 0x80 != 0 {
        if let Some(off) = first_offset {
            bytes.truncate(off);
        }
    }

    let creds = password.map(|p| Credentials {
        password: p.as_bytes().to_vec(),
        keyfile: keyfile.map(<[u8]>::to_vec),
    });
    open_and_extract(&bytes, creds.as_ref());
    open_and_extract(&bytes, None);

    // Keyless recovery: scan and repair into memory.
    if let Ok(mut a) = Archive::open_with(Cursor::new(bytes.clone()), &tight(), None) {
        let _ = a.check_recovery();
    }
    let mut out = Cursor::new(Vec::new());
    let _ = repair(Cursor::new(bytes), &mut out, &tight());
});
