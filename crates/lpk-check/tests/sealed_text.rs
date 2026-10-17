//! Section 14 checked against the text: the order of the key slot checks, the keyless walk's
//! outcomes, and the outcomes the text lists for a modified frame (E1-15 phase 2). Every input is
//! a sealed vector changed in the test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use lpk_check::wire::{kind, read_varint, FF_SEALED, HEADER_LEN};
use lpk_check::{keyless, Archive, Error, Options};

fn read(name: &str) -> Vec<u8> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-format/tests/vectors");
    std::fs::read(dir.join(name)).unwrap()
}

/// One frame of the file as the envelope gives it.
#[derive(Debug, Clone, Copy)]
struct F {
    off: usize,
    kind: u16,
    /// Offset and width of the `payload_len` varint.
    len_at: usize,
    len_width: usize,
    plen: usize,
    /// Start of the payload.
    pay: usize,
    end: usize,
    seq: u64,
}

fn frames(d: &[u8]) -> Vec<F> {
    let mut out = Vec::new();
    let mut pos = HEADER_LEN;
    let mut seq = 0;
    while pos < d.len() {
        let mut p = pos + 4;
        let plen = read_varint(d, &mut p, "test").unwrap() as usize;
        let f = F {
            off: pos,
            kind: u16::from_le_bytes([d[pos], d[pos + 1]]),
            len_at: pos + 4,
            len_width: p - pos - 4,
            plen,
            pay: p,
            end: p + plen + 32,
            seq,
        };
        out.push(f);
        pos = f.end;
        seq += 1;
    }
    out
}

fn first(d: &[u8], k: u16) -> F {
    frames(d).into_iter().find(|f| f.kind == k).unwrap()
}

fn varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn rehash(d: &mut [u8], f: F) {
    let h = *blake3::hash(&d[f.pay..f.pay + f.plen]).as_bytes();
    d[f.pay + f.plen..f.pay + f.plen + 32].copy_from_slice(&h);
}

fn set_flags(d: &mut [u8], f: F, flags: u16) {
    d[f.off + 2..f.off + 4].copy_from_slice(&flags.to_le_bytes());
}

fn flags(d: &[u8], f: F) -> u16 {
    u16::from_le_bytes([d[f.off + 2], d[f.off + 3]])
}

/// Replaces a frame's `payload_len` varint (the file may change length).
fn set_len(d: &mut Vec<u8>, f: F, plen: u64) {
    d.splice(f.len_at..f.len_at + f.len_width, varint(plen));
}

fn with_key(password: &str) -> Options {
    Options {
        password: Some(password.as_bytes().to_vec()),
        ..Options::default()
    }
}

fn open_err(d: Vec<u8>, o: Options) -> Error {
    Archive::open(d, o).expect_err("must fail")
}

/// Opens with the key and runs everything that reads the blocks and the entry table.
fn full_read(d: Vec<u8>, o: Options) -> Result<(), Error> {
    let mut a = Archive::open(d, o)?;
    a.verify()?;
    a.entries()?;
    Ok(())
}

// ---- The key slot, "Reading the key slot", in the text's order ----

#[test]
fn key_slot_first_frame_of_another_kind_is_missing_key_slot() {
    let mut d = read("sealed-aes.lpk");
    let slot = frames(&d)[0];
    assert_eq!(slot.kind, kind::KEY_SLOT);
    d[slot.off..slot.off + 2].copy_from_slice(&kind::CHUNK_DATA.to_le_bytes());
    assert_eq!(open_err(d, Options::default()).class, "MissingKeySlot");
}

#[test]
fn key_slot_payload_len_above_111_is_bad_key_slot_length() {
    let mut d = read("sealed-aes.lpk");
    let slot = frames(&d)[0];
    assert_eq!(slot.plen, 111);
    set_len(&mut d, slot, 112);
    let e = open_err(d, Options::default());
    assert_eq!(
        (e.class, e.detail.ends_with("length")),
        ("BadKeySlot", true)
    );
}

/// Rebuilds the key slot with a payload one byte shorter and a valid hash.
fn short_slot(sealed: bool) -> Vec<u8> {
    let d = read("sealed-aes.lpk");
    let slot = frames(&d)[0];
    let mut payload = d[slot.pay..slot.pay + 110].to_vec();
    let mut out = d[..slot.off].to_vec();
    let fl = if sealed { FF_SEALED } else { 0 };
    out.extend_from_slice(&kind::KEY_SLOT.to_le_bytes());
    out.extend_from_slice(&fl.to_le_bytes());
    out.extend(varint(110));
    let h = *blake3::hash(&payload).as_bytes();
    out.append(&mut payload);
    out.extend_from_slice(&h);
    out.extend_from_slice(&d[slot.end..]);
    out
}

#[test]
fn key_slot_sealed_flag_comes_before_the_length() {
    assert_eq!(
        open_err(short_slot(true), Options::default()).class,
        "UnexpectedSealedFrame"
    );
    let e = open_err(short_slot(false), Options::default());
    assert_eq!(
        (e.class, e.detail.ends_with("length")),
        ("BadKeySlot", true)
    );
    // The SEALED flag on the slot as written.
    let mut d = read("sealed-aes.lpk");
    let slot = frames(&d)[0];
    set_flags(&mut d, slot, FF_SEALED);
    assert_eq!(
        open_err(d, Options::default()).class,
        "UnexpectedSealedFrame"
    );
}

/// Changes key slot bytes (offset in the payload, value) and rehashes the slot.
fn slot_with(changes: &[(usize, &[u8])]) -> Error {
    let mut d = read("sealed-aes.lpk");
    let slot = frames(&d)[0];
    for (at, v) in changes {
        d[slot.pay + at..slot.pay + at + v.len()].copy_from_slice(v);
    }
    rehash(&mut d, slot);
    open_err(d, Options::default())
}

#[test]
fn key_slot_field_checks_in_the_text_order() {
    let reason = |e: Error| (e.class, e.detail.rsplit(' ').next().unwrap().to_string());
    let bks = |r: &str| ("BadKeySlot", r.to_string());
    let ba = |r: &str| ("BadArgon2", r.to_string());
    let zero = 0u32.to_le_bytes();
    let big = 65u32.to_le_bytes();
    // suite, then kdf.
    assert_eq!(reason(slot_with(&[(0, &[3]), (1, &[2])])), bks("suite"));
    assert_eq!(reason(slot_with(&[(1, &[2]), (2, &zero)])), bks("kdf"));
    // argon2_t, then argon2_m_kib, then argon2_p.
    assert_eq!(
        reason(slot_with(&[(2, &zero), (6, &zero), (10, &zero)])),
        ba("t")
    );
    assert_eq!(reason(slot_with(&[(2, &big)])), ba("t"));
    assert_eq!(
        reason(slot_with(&[(6, &8191u32.to_le_bytes()), (10, &zero)])),
        ba("m")
    );
    assert_eq!(reason(slot_with(&[(10, &big), (30, &[2])])), ba("p"));
    // keyfile_required last.
    assert_eq!(reason(slot_with(&[(30, &[2])])), bks("keyfile_required"));
}

// ---- The keyless walk (step 2) ----

#[test]
fn keyless_walk_length_overflowing_64_bits_is_bad_frame_location_frames() {
    let mut d = read("sealed-aes.lpk");
    let f = frames(&d)[1];
    set_len(&mut d, f, u64::MAX);
    let e = keyless::walk(&d, &Options::default()).unwrap_err();
    assert_eq!(e, Error::bad_location("frames"));
}

#[test]
fn keyless_walk_frame_past_the_file_is_truncated_frames() {
    let mut d = read("sealed-aes.lpk");
    let f = frames(&d)[1];
    let n = d.len() as u64;
    set_len(&mut d, f, n);
    let e = keyless::walk(&d, &Options::default()).unwrap_err();
    assert_eq!(e, Error::truncated("frames"));
    // The walk skips payloads unread: a damaged payload does not stop it.
    let mut d = read("sealed-aes.lpk");
    let f = frames(&d)[1];
    d[f.pay] ^= 1;
    let w = keyless::walk(&d, &Options::default()).unwrap();
    assert_eq!(w.frames as usize, frames(&d).len() - 1);
}

#[test]
fn keyless_walk_second_key_slot_is_unexpected_key_slot() {
    let mut d = read("sealed-aes.lpk");
    let f = frames(&d)[1];
    d[f.off..f.off + 2].copy_from_slice(&kind::KEY_SLOT.to_le_bytes());
    let e = keyless::walk(&d, &Options::default()).unwrap_err();
    assert_eq!(e.class, "UnexpectedKeySlot");
}

#[test]
fn keyless_listable_without_entry_table_is_bad_frame_location() {
    let mut d = read("sealed-listable.lpk");
    assert_eq!(keyless::list(&d, &Options::default()).map(|_| ()), Ok(()));
    let f = first(&d, kind::ENTRY_TABLE);
    // A reserved kind without MUST_UNDERSTAND: skipped, so no entry table is left.
    d[f.off..f.off + 2].copy_from_slice(&0x0100u16.to_le_bytes());
    let e = keyless::list(&d, &Options::default()).unwrap_err();
    assert_eq!(e, Error::bad_location("entry table"));
}

// ---- A modified frame (step 3), with the key ----

fn aes() -> (Vec<u8>, Options) {
    (read("sealed-aes.lpk"), with_key("correct horse"))
}

#[test]
fn modified_payload_nonce_or_tag_fails_the_frame_hash() {
    let (d0, o) = aes();
    assert_eq!(full_read(d0.clone(), o.clone()), Ok(()));
    let f = first(&d0, kind::CHUNK_DATA);
    for at in [f.pay, f.pay + 20, f.pay + f.plen - 1] {
        let mut d = d0.clone();
        d[at] ^= 0x01;
        assert_eq!(full_read(d, o.clone()).unwrap_err().class, "HashMismatch");
    }
}

#[test]
fn modified_payload_nonce_or_tag_with_the_hash_fails_the_tag() {
    let (d0, o) = aes();
    let f = first(&d0, kind::CHUNK_DATA);
    // Nonce (12 bytes for suite 1), ciphertext, tag.
    for at in [f.pay, f.pay + 12, f.pay + f.plen - 1] {
        let mut d = d0.clone();
        d[at] ^= 0x01;
        rehash(&mut d, f);
        let e = full_read(d, o.clone()).unwrap_err();
        assert_eq!(e.class, "AuthenticationFailed");
        let names = format!("kind {} sequence {}", kind::CHUNK_DATA, f.seq);
        assert!(e.detail.contains(&names), "{}", e.detail);
    }
}

#[test]
fn cleared_sealed_bit_is_unsealed_frame() {
    let (mut d, o) = aes();
    let f = first(&d, kind::CHUNK_DATA);
    let fl = flags(&d, f);
    assert_eq!(fl & FF_SEALED, FF_SEALED);
    set_flags(&mut d, f, fl & !FF_SEALED);
    assert_eq!(full_read(d, o).unwrap_err().class, "UnsealedFrame");
}

#[test]
fn set_sealed_bit_on_a_frame_never_sealed_is_unexpected_sealed_frame() {
    // The clear entry table of a listable archive, read with the key.
    let mut d = read("sealed-listable.lpk");
    let o = with_key("list me");
    assert_eq!(full_read(d.clone(), o.clone()), Ok(()));
    let f = first(&d, kind::ENTRY_TABLE);
    assert_eq!(flags(&d, f) & FF_SEALED, 0);
    set_flags(&mut d, f, FF_SEALED);
    assert_eq!(full_read(d, o).unwrap_err().class, "UnexpectedSealedFrame");
}

#[test]
fn changed_kind_at_a_recorded_location_is_wrong_frame_kind() {
    let (mut d, o) = aes();
    let f = first(&d, kind::CHUNK_DATA);
    d[f.off..f.off + 2].copy_from_slice(&kind::RECORDS.to_le_bytes());
    assert_eq!(full_read(d, o).unwrap_err().class, "WrongFrameKind");
}

#[test]
fn changed_payload_len_is_bad_frame_location_or_a_frame_error() {
    let (d0, o) = aes();
    let f = first(&d0, kind::CHUNK_DATA);
    // Longer than recorded: the bound cuts it.
    let mut d = d0.clone();
    assert_eq!(varint(f.plen as u64 + 1).len(), f.len_width);
    set_len(&mut d, f, f.plen as u64 + 1);
    assert_eq!(
        full_read(d, o.clone()).unwrap_err().class,
        "BadFrameLocation"
    );
    // Shorter: read and hashed first (a frame error of section 3).
    let mut d = d0.clone();
    assert_eq!(varint(f.plen as u64 - 1).len(), f.len_width);
    set_len(&mut d, f, f.plen as u64 - 1);
    assert_eq!(full_read(d, o).unwrap_err().class, "HashMismatch");
}

#[test]
fn rewritten_clear_entry_table_is_entry_table_mismatch() {
    let mut d = read("sealed-listable.lpk");
    let o = with_key("list me");
    let f = first(&d, kind::ENTRY_TABLE);
    d[f.pay + f.plen - 1] ^= 0x01;
    rehash(&mut d, f);
    let a = Archive::open(d, o).unwrap();
    assert_eq!(a.entries().unwrap_err().class, "EntryTableMismatch");
}
