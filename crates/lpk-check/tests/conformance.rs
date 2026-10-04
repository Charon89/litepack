//! The E1-14 gate (docs/spec/CONFORMANCE.md) run by the independent decoder over every vector.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use lpk_check::extract::extract_all;
use lpk_check::{journal, keyless, recovery, Archive, Options};
use toml::{Table, Value};

fn vectors() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-format/tests/vectors")
}

fn table(name: &str) -> Table {
    let s = std::fs::read_to_string(vectors().join(name)).unwrap();
    s.parse::<Table>().unwrap()
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(vectors().join(name)).unwrap()
}

/// The inputs CONFORMANCE.md names for a vector: password, keyfile, prior.
fn inputs(name: &str) -> (Options, Vec<String>) {
    let v = table("vectors.toml");
    let mut o = Options::default();
    let mut args = Vec::new();
    if let Some(t) = v.get(name).and_then(Value::as_table) {
        if let Some(p) = t.get("password").and_then(Value::as_str) {
            o.password = Some(p.as_bytes().to_vec());
            args.extend(["--password".to_string(), p.to_string()]);
        }
        if let Some(k) = t.get("keyfile").and_then(Value::as_str) {
            o.keyfile = Some(read(k));
            args.extend([
                "--keyfile".to_string(),
                vectors().join(k).display().to_string(),
            ]);
        }
    }
    if name == "zstd-dict.lpk" {
        o.add_prior(read("zstd-dict.prior"));
        args.extend([
            "--prior".to_string(),
            vectors().join("zstd-dict.prior").display().to_string(),
        ]);
    }
    (o, args)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

/// `path size blake3` of every file entry, decoded in entry-table order.
fn decoded_lines(a: &mut Archive) -> Result<Vec<String>, lpk_check::Error> {
    let entries = a.entries()?;
    let mut out = Vec::new();
    for e in &entries {
        let b = a.read_file(e)?;
        out.push(format!(
            "{} {} {}",
            e.path,
            b.len(),
            blake3::hash(&b).to_hex()
        ));
    }
    Ok(out)
}

fn bin(args: &[String]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_lpk-check"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lpk-check-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Gate items 1 to 4 for every vector that has a `files` list.
#[test]
fn every_vector_opens_extracts_verifies_and_lists() {
    let exp = table("expected.toml");
    let mut failures = Vec::new();
    let mut passed = 0;
    for (name, t) in &exp {
        let Some(files) = t.get("files") else {
            continue;
        };
        let files = strings(files);
        let (opts, args) = inputs(name);
        let path = vectors().join(name).display().to_string();
        let mut fail = |what: String| failures.push(format!("{name}: {what}"));
        // 1, 2: open and extract bit-exactly in entry order.
        let mut a = match Archive::open(read(name), opts.clone()) {
            Ok(a) => a,
            Err(e) => {
                fail(format!("open: {} {e}", e.class));
                continue;
            }
        };
        match decoded_lines(&mut a) {
            Ok(l) if l == files => {}
            Ok(l) => fail(format!("files {l:?}")),
            Err(e) => fail(format!("decode: {} {e}", e.class)),
        }
        // Extraction to disk gives the same bytes.
        let dir = temp(name);
        let mut a2 = Archive::open(read(name), opts.clone()).unwrap();
        match extract_all(&mut a2, &dir) {
            Ok((_, None)) => {
                for line in &files {
                    let mut it = line.split(' ');
                    let (p, size, h) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
                    let b = std::fs::read(dir.join(p)).unwrap_or_default();
                    if b.len().to_string() != size || blake3::hash(&b).to_hex().as_str() != h {
                        fail(format!("extracted {p} differs"));
                    }
                }
            }
            Ok((_, Some(e))) | Err(e) => fail(format!("extract: {} {e}", e.class)),
        }
        let _ = std::fs::remove_dir_all(&dir);
        // 3: verify is clean, with the counts of the reference format.
        let want_verify = t["verify"].as_str().unwrap();
        match a.verify() {
            Ok(s) => {
                let got = format!(
                    "ok: {} entries, {} chunks, {} blocks\n",
                    s.entries, s.chunks, s.blocks
                );
                if got != want_verify {
                    fail(format!("verify summary {got:?}"));
                }
            }
            Err(e) => fail(format!("verify: {} {e}", e.class)),
        }
        // 4: the tool's list and verify output equal the reference's.
        for (cmd, key) in [("list", "list"), ("verify", "verify")] {
            let mut v = args.clone();
            v.extend([cmd.to_string(), path.clone()]);
            let (code, out, err) = bin(&v);
            if code != 0 || out != t[key].as_str().unwrap() {
                fail(format!("tool {cmd}: exit {code}, {out:?} {err:?}"));
            }
        }
        if !failures.iter().any(|f| f.starts_with(&format!("{name}:"))) {
            passed += 1;
        }
    }
    assert!(failures.is_empty(), "failures: {failures:#?}");
    assert_eq!(passed, 13, "every vector with a files list passes");
}

/// Gate item 5: the latest generation and rollbacks to generations 0 and 1, on copies.
#[test]
fn journal_rollback() {
    let exp = table("expected.toml");
    let t = exp["journal-3gen.lpk"].as_table().unwrap();
    let data = read("journal-3gen.lpk");
    let chain = journal::history(&data).unwrap();
    assert_eq!(chain.len(), 3);
    assert_eq!(
        chain[0].generation as i64,
        t["latest_generation"].as_integer().unwrap()
    );
    let a = Archive::open(data.clone(), Options::default()).unwrap();
    assert_eq!(a.trailer.generation, 2);
    for (g, key) in [
        (0u64, "files_at_generation_0"),
        (1, "files_at_generation_1"),
    ] {
        let len = journal::rollback_len(&data, g).unwrap();
        let copy = data[..len].to_vec();
        let mut a = Archive::open(copy.clone(), Options::default()).unwrap();
        assert_eq!(a.trailer.generation, g);
        assert_eq!(
            decoded_lines(&mut a).unwrap(),
            strings(&t[key]),
            "generation {g}"
        );
        a.verify().unwrap();
        // The tool writes the same copy.
        let dir = temp(&format!("rollback-{g}"));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("copy.lpk");
        let (code, _, err) = bin(&[
            "rollback".into(),
            vectors().join("journal-3gen.lpk").display().to_string(),
            g.to_string(),
            out.display().to_string(),
        ]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), copy);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert_eq!(
        journal::rollback_len(&data, 3).map_err(|e| e.class),
        Err("NoSuchGeneration")
    );
}

/// Gate item 6: the truncated and hash-flipped vectors fail with their class and extract nothing.
#[test]
fn malformed_vectors() {
    let exp = table("expected.toml");
    let t = exp["malformed-truncated.lpk"].as_table().unwrap();
    let e = Archive::open(read("malformed-truncated.lpk"), Options::default()).unwrap_err();
    assert_eq!(e.class, t["error"].as_str().unwrap());
    assert_eq!(e.detail, t["message"].as_str().unwrap());
    let dir = temp("truncated");
    let (code, _, err) = bin(&[
        "extract".into(),
        vectors()
            .join("malformed-truncated.lpk")
            .display()
            .to_string(),
        dir.display().to_string(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("input truncated in trailer"), "{err}");
    assert!(!dir.exists());

    let t = exp["malformed-hashflip.lpk"].as_table().unwrap();
    let mut a = Archive::open(read("malformed-hashflip.lpk"), Options::default()).unwrap();
    let e = a.verify().unwrap_err();
    assert_eq!(e.class, t["error"].as_str().unwrap());
    assert_eq!(e.detail, t["message"].as_str().unwrap());
    let dir = temp("hashflip");
    let (done, err) = extract_all(&mut a, &dir).unwrap();
    assert!(done.is_empty() && err.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// E1-14d ruling 1 (spec section 9, CONFORMANCE "malformed-hashflip"): a block
/// frame whose hash fails is that frame's `HashMismatch` (kind 2) when a file
/// of the block is extracted, as for `verify`; `ChunkMismatch` is only for an
/// intact frame whose chunk differs.
#[test]
fn hashflip_extract_is_a_hash_mismatch() {
    let exp = table("expected.toml");
    let t = exp["malformed-hashflip.lpk"].as_table().unwrap();
    let mut a = Archive::open(read("malformed-hashflip.lpk"), Options::default()).unwrap();
    let entries = a.entries().unwrap();
    for en in &entries {
        let e = a.read_file(en).unwrap_err();
        assert_eq!(e.class, "HashMismatch");
        assert_eq!(e.detail, t["message"].as_str().unwrap());
    }
}

/// Gate item 7: recovery check, damage report, partial extraction and repair.
#[test]
fn recovery_vectors() {
    let exp = table("expected.toml");
    let t = exp["recovery-groups.lpk"].as_table().unwrap();
    let path = vectors().join("recovery-groups.lpk").display().to_string();
    let (code, out, _) = bin(&["check".into(), path]);
    assert_eq!((code, out.as_str()), (0, t["check"].as_str().unwrap()));

    let t = exp["malformed-recovery-damaged.lpk"].as_table().unwrap();
    let dpath = vectors().join("malformed-recovery-damaged.lpk");
    let (code, out, err) = bin(&["check".into(), dpath.display().to_string()]);
    assert_eq!(i64::from(code), t["check_exit"].as_integer().unwrap());
    assert_eq!(out, t["check_stdout"].as_str().unwrap());
    assert_eq!(err, t["check_stderr"].as_str().unwrap());

    let data = read("malformed-recovery-damaged.lpk");
    let mut a = Archive::open(data.clone(), Options::default()).unwrap();
    let rep = recovery::scan(
        &data,
        &a.index.recovery,
        &a.index.generations,
        a.trailer.index_offset,
        None,
        1 << 31,
    )
    .unwrap();
    assert_eq!(
        rep.frames as i64,
        t["recovery_frames"].as_integer().unwrap()
    );
    assert_eq!(
        rep.damaged as i64,
        t["shards_damaged"].as_integer().unwrap()
    );
    // Every file no damaged chunk touches extracts correctly; the others fail.
    let good = strings(&exp["recovery-groups.lpk"]["files"]);
    let entries = a.entries().unwrap();
    let mut extracted = 0;
    for (en, line) in entries.iter().zip(&good) {
        let touched = en.chunks.iter().any(|&c| {
            let b = a.index.blocks[a.index.chunk_block[c as usize]];
            rep.damaged_shards
                .iter()
                .any(|d| b.frame_offset < d.end && d.start < b.frame_offset + b.frame_len)
        });
        match a.read_file(en) {
            Ok(b) => {
                assert!(!touched, "{} extracted although damaged", en.path);
                assert_eq!(
                    &format!("{} {} {}", en.path, b.len(), blake3::hash(&b).to_hex()),
                    line
                );
                extracted += 1;
            }
            Err(e) => assert!(touched, "{} failed: {e}", en.path),
        }
    }
    assert!(extracted >= 1);
    // Repair restores recovery-groups.lpk byte for byte.
    let dir = temp("repair");
    std::fs::create_dir_all(&dir).unwrap();
    let out_path = dir.join("fixed.lpk");
    let (code, out, err) = bin(&[
        "repair".into(),
        dpath.display().to_string(),
        out_path.display().to_string(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&format!(
        "repaired shards: {}",
        t["shards_repaired_by_repair"].as_integer().unwrap()
    )));
    let fixed = std::fs::read(&out_path).unwrap();
    assert!(fixed == read(t["repaired_copy_equals"].as_str().unwrap()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The "Needs" column: without the prior, credentials or keyfile the vectors refuse properly.
#[test]
fn needs_are_enforced() {
    let mut a = Archive::open(read("zstd-dict.lpk"), Options::default()).unwrap();
    assert_eq!(a.verify().map_err(|e| e.class), Err("MissingPrior"));

    let (mut o, _) = inputs("sealed-keyfile.lpk");
    o.keyfile = None;
    assert_eq!(
        Archive::open(read("sealed-keyfile.lpk"), o)
            .map_err(|e| e.class)
            .err(),
        Some("WrongKey")
    );
    let (mut o, _) = inputs("sealed-aes.lpk");
    o.password = Some(b"wrong".to_vec());
    assert_eq!(
        Archive::open(read("sealed-aes.lpk"), o)
            .map_err(|e| e.class)
            .err(),
        Some("WrongKey")
    );
    assert_eq!(
        Archive::open(read("sealed-aes.lpk"), Options::default())
            .map_err(|e| e.class)
            .err(),
        Some("PasswordRequired")
    );

    // Listable: the listing needs no password; extraction does.
    let exp = table("expected.toml");
    let path = vectors().join("sealed-listable.lpk").display().to_string();
    let (code, out, _) = bin(&["list".into(), path.clone()]);
    assert_eq!(
        (code, out.as_str()),
        (0, exp["sealed-listable.lpk"]["list"].as_str().unwrap())
    );
    let none = Options::default();
    assert_eq!(
        keyless::list(&read("sealed-listable.lpk"), &none)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        keyless::list(&read("sealed-aes.lpk"), &none)
            .map_err(|e| e.class)
            .err(),
        Some("PasswordRequired")
    );
    let dir = temp("listable");
    let (code, _, err) = bin(&["extract".into(), path.clone(), dir.display().to_string()]);
    assert_eq!(code, 1);
    assert!(err.contains("password"), "{err}");
    // Keyless verify (CONFORMANCE output formats): frame hashes only.
    let (code, out, err) = bin(&["verify".into(), path]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "ok (frame hashes only, chunks not checked without the password): 3 entries\n"
    );
    for v in ["sealed-aes.lpk", "sealed-xchacha.lpk", "sealed-keyfile.lpk"] {
        let (frames, entries) = keyless::verify(&read(v), &none).unwrap();
        assert!(frames > 0 && entries.is_none());
        let w = keyless::walk(&read(v), &none).unwrap();
        assert!(w.recovery.is_empty() && w.generations.len() == 1);
    }
}

/// Revised section 2: LISTABLE without ENCRYPTED is `BadHeaderFlags`; section 6 step 3: a key
/// slot in a plain archive is `UnexpectedKeySlot`; the index must end where the trailer starts.
#[test]
fn stricter_open_rules() {
    let mut d = read("zstd-basic.lpk");
    d[12] = 2;
    assert_eq!(
        Archive::open(d, Options::default())
            .map_err(|e| e.class)
            .err(),
        Some("BadHeaderFlags")
    );
    let mut d = read("zstd-basic.lpk");
    d[32] = 7;
    d[33] = 0;
    assert_eq!(
        Archive::open(d, Options::default())
            .map_err(|e| e.class)
            .err(),
        Some("UnexpectedKeySlot")
    );
    // A trailer whose index_len leaves a gap before the trailer (trailer hash recomputed).
    let mut d = read("zstd-basic.lpk");
    let t = d.len() - 133;
    let p = t + 5;
    let il = u64::from_le_bytes(d[p + 8..p + 16].try_into().unwrap());
    d[p + 8..p + 16].copy_from_slice(&(il - 1).to_le_bytes());
    let h = blake3::hash(&d[p..p + 96]);
    d[p + 96..p + 128].copy_from_slice(h.as_bytes());
    assert_eq!(
        Archive::open(d, Options::default())
            .map_err(|e| e.class)
            .err(),
        Some("BadFrameLocation")
    );
}

/// Robustness (CONFORMANCE.md "Fuzz and robustness"): every single-bit flip and many truncations
/// of three vectors are refused or decoded without a panic, and a flip never verifies clean.
#[test]
fn mutations_never_panic_or_pass() {
    for name in ["zstd-basic.lpk", "lzma-basic.lpk", "zstd-dict.lpk"] {
        let (opts, _) = inputs(name);
        let data = read(name);
        // The low flag byte of every frame: MUST_UNDERSTAND (bit 0) on a known kind changes
        // nothing and the frame envelope is not hashed (reported as a spec observation).
        let mut flag_bytes = Vec::new();
        let mut pos = 32;
        while pos < data.len() {
            let f = lpk_check::wire::parse_frame(&data, pos, u64::MAX).unwrap();
            flag_bytes.push(pos + 2);
            pos = f.end;
        }
        // Bytes 10 and 11 are version_minor: any value is accepted (section 2) and nothing binds it.
        for i in (0..data.len()).filter(|i| !(10..12).contains(i)) {
            let mut m = data.clone();
            let bit = 1u8 << (i % 8);
            m[i] ^= bit;
            if let Ok(mut a) = Archive::open(m, opts.clone()) {
                let harmless = bit == 1 && flag_bytes.contains(&i);
                assert!(
                    a.verify().is_err() || harmless,
                    "{name}: flip at {i} verified clean"
                );
            }
        }
        for len in (0..data.len()).step_by(7) {
            let r = Archive::open(data[..len].to_vec(), opts.clone());
            assert!(r.is_err(), "{name}: truncation to {len} opened");
        }
    }
}

// ---- Revision 1.1: `jpeg-peel.lpk` (CONFORMANCE "jpeg-peel.lpk", spec section 8) ----

const JPEG: &str = "jpeg-peel.lpk";

fn jpeg_path() -> String {
    vectors().join(JPEG).display().to_string()
}

/// The payload range of the frame at `offset`.
fn payload_range(d: &[u8], offset: usize) -> std::ops::Range<usize> {
    let f = lpk_check::wire::parse_frame(d, offset, u64::MAX).unwrap();
    f.end - 32 - f.payload.len()..f.end - 32
}

/// Rewrites the 32-byte hash of the frame at `offset` after its payload was changed in place.
fn rehash_frame(d: &mut [u8], offset: usize) {
    let p = payload_range(d, offset);
    let h = blake3::hash(&d[p.clone()]);
    d[p.end..p.end + 32].copy_from_slice(h.as_bytes());
}

/// Changes byte `at_from_end` (counted back from the body's end) of record 0's body, then
/// recomputes its `body_hash` and the `Records` frame hash.
fn patch_record_body(at_from_end: usize, f: impl Fn(u8) -> u8) -> Vec<u8> {
    let mut d = read(JPEG);
    let a = Archive::open(d.clone(), Options::default()).unwrap();
    let off = a.index.records.unwrap().offset as usize;
    let p = payload_range(&d, off);
    // record_count (1 byte), kind, flags, body_len (varint), body, body_hash.
    let mut pos = p.start + 5;
    let (mut len, mut shift) = (0usize, 0);
    loop {
        let b = d[pos];
        pos += 1;
        len |= usize::from(b & 0x7F) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
    }
    let body = pos..pos + len;
    let i = body.end - at_from_end;
    d[i] = f(d[i]);
    let h = blake3::hash(&d[body.clone()]);
    d[body.end..body.end + 32].copy_from_slice(h.as_bytes());
    rehash_frame(&mut d, off);
    d
}

fn verify_class(d: Vec<u8>) -> (String, String) {
    let mut a = Archive::open(d, Options::default()).unwrap();
    let e = a.verify().unwrap_err();
    (e.class.to_string(), e.detail)
}

/// A revision 1.1 decoder: `files` and `verify` of `expected-1.1.toml`, by the library and the
/// tool; `list` and `info` of `expected.toml` (the same facts for every revision).
#[test]
fn jpeg_peel_revision_1_1() {
    let exp = table("expected-1.1.toml");
    let t = exp[JPEG].as_table().unwrap();
    let files = strings(&t["files"]);
    let mut a = Archive::open(read(JPEG), Options::default()).unwrap();
    assert_eq!(a.header.version_minor, 1);
    assert_eq!(decoded_lines(&mut a).unwrap(), files);
    let s = a.verify().unwrap();
    assert_eq!(
        format!(
            "ok: {} entries, {} chunks, {} blocks\n",
            s.entries, s.chunks, s.blocks
        ),
        t["verify"].as_str().unwrap()
    );
    let (code, out, err) = bin(&["verify".into(), jpeg_path()]);
    assert_eq!(
        (code, out.as_str()),
        (0, t["verify"].as_str().unwrap()),
        "{err}"
    );
    // Extraction to disk, by the library and by the tool.
    for tool in [false, true] {
        let dir = temp(&format!("jpeg-{tool}"));
        if tool {
            let (code, _, err) = bin(&["extract".into(), jpeg_path(), dir.display().to_string()]);
            assert_eq!(code, 0, "{err}");
        } else {
            let mut a = Archive::open(read(JPEG), Options::default()).unwrap();
            let (done, err) = extract_all(&mut a, &dir).unwrap();
            assert!(err.is_none() && done.len() == 1);
        }
        for line in &files {
            let mut it = line.split(' ');
            let (p, size, h) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
            let b = std::fs::read(dir.join(p)).unwrap();
            assert_eq!(b.len().to_string(), size);
            assert_eq!(blake3::hash(&b).to_hex().as_str(), h);
            assert_eq!(&b[..2], &[0xFF, 0xD8], "a JPEG");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    let old = table("expected.toml");
    let t0 = old[JPEG].as_table().unwrap();
    for (cmd, key) in [("list", "list"), ("info", "info")] {
        let (code, out, err) = bin(&[cmd.into(), jpeg_path()]);
        assert_eq!(
            (code, out.as_str()),
            (0, t0[key].as_str().unwrap()),
            "{err}"
        );
    }
    // No recovery frames: `check` finds nothing to scan.
    let (code, out, _) = bin(&["check".into(), jpeg_path()]);
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "recovery frames: 0, unusable: 0, damaged shards: 0, repaired shards: 0\n"
        )
    );
}

/// The `expected.toml` keys of a revision 1.0 reader, by this decoder in its 1.0 mode: it opens,
/// lists and reports `UnimplementedPrimitive` 7 from `verify` and from extracting `photo.jpg`.
#[test]
fn jpeg_peel_revision_1_0_mode() {
    let exp = table("expected.toml");
    let t = exp[JPEG].as_table().unwrap();
    let o = Options {
        revision_1_0: true,
        ..Options::default()
    };
    let mut a = Archive::open(read(JPEG), o).unwrap();
    let e = a.verify().unwrap_err();
    assert_eq!(e.class, t["error_1_0"].as_str().unwrap());
    assert_eq!(e.detail, t["message_1_0"].as_str().unwrap());
    let entries = a.entries().unwrap();
    let e = a.read_file(&entries[0]).unwrap_err();
    assert_eq!(e.class, "UnimplementedPrimitive");
    let (code, out, err) = bin(&["--revision-1-0".into(), "verify".into(), jpeg_path()]);
    assert_eq!(i64::from(code), t["verify_exit_1_0"].as_integer().unwrap());
    assert_eq!(err, t["verify_stderr_1_0"].as_str().unwrap());
    assert!(out.is_empty());
    let (code, out, _) = bin(&["--revision-1-0".into(), "list".into(), jpeg_path()]);
    assert_eq!((code, out.as_str()), (0, t["list"].as_str().unwrap()));
}

/// Section 8 check 5: the memory term computed from the Lepton stream's own header is the one the
/// writer declared (`decode_memory` = `max_block_plain` + term + the fixed term, one image).
#[test]
fn jpeg_peel_memory_term() {
    let d = read(JPEG);
    let a = Archive::open(d.clone(), Options::default()).unwrap();
    let records = a.records().unwrap();
    let lpk_check::record::Body::Jpeg(j) = &records[0].body else {
        panic!("a jpeg record")
    };
    let bl = a.index.blocks[1];
    let f = lpk_check::wire::parse_frame(&d, bl.frame_offset as usize, u64::MAX).unwrap();
    let h = lpk_check::block::parse_block(f.payload, &mut || Ok(1)).unwrap();
    assert_eq!(h.steps, vec![lpk_check::block::Prim::Jpeg { record_id: 0 }]);
    assert_eq!(h.plain_len, j.primary_len);
    let frame = lpk_check::jpeg::stream_frame(0, h.encoded, j.primary_len).unwrap();
    let env = a.index.envelope;
    assert_eq!(
        env.max_block_plain + frame.memory_term() + lpk_check::jpeg::FIXED_TERM,
        env.decode_memory
    );
    // A reader whose memory is below the declared decode_memory refuses at open (section 7).
    let mut o = Options::default();
    o.resources.memory = env.decode_memory - 1;
    let e = Archive::open(d, o).unwrap_err();
    assert_eq!(e.class, "Refused");
    assert!(e.detail.contains("decode_memory"));
}

/// Section 2's version rule: any minor is accepted and the blocks decide; a primitive this reader
/// does not run is `UnimplementedPrimitive` with its id, before any step runs.
#[test]
fn later_minor_and_unknown_reconstruction() {
    let mut d = read(JPEG);
    d[10..12].copy_from_slice(&7u16.to_le_bytes());
    let mut a = Archive::open(d, Options::default()).unwrap();
    assert_eq!(a.verify().unwrap().blocks, 2);
    // A 1.0 archive is decoded as before, whatever its minor says.
    let mut d = read("zstd-basic.lpk");
    d[10..12].copy_from_slice(&1u16.to_le_bytes());
    Archive::open(d, Options::default())
        .unwrap()
        .verify()
        .unwrap();
    // The jpeg block's primitive changed to deflate-reconstruct (0x0008), frame hash recomputed.
    let mut d = read(JPEG);
    let a = Archive::open(d.clone(), Options::default()).unwrap();
    let off = a.index.blocks[1].frame_offset as usize;
    let p = payload_range(&d, off);
    assert_eq!(&d[p.start..p.start + 3], &[1, 7, 0]);
    d[p.start + 1] = 8;
    rehash_frame(&mut d, off);
    assert_eq!(
        verify_class(d),
        (
            "UnimplementedPrimitive".into(),
            "primitive 0x0008 is not implemented by this reader".into()
        )
    );
}

/// Section 8 checks 3, 6 and 8 on mutated copies (hashes recomputed so the damage reaches the step).
#[test]
fn jpeg_reconstruct_negative_cases() {
    // lepton_version (the byte before original_hash) set to 1.
    let d = patch_record_body(33, |_| 1);
    assert_eq!(
        verify_class(d),
        ("BadRecord".into(), "bad record 0: lepton_version".into())
    );
    // original_hash changed.
    let d = patch_record_body(1, |b| b ^ 1);
    assert_eq!(
        verify_class(d),
        ("BadRecord".into(), "bad record 0: original_hash".into())
    );
    // A byte of the Lepton stream changed: the library refuses it, or the output differs.
    let d0 = read(JPEG);
    let a = Archive::open(d0.clone(), Options::default()).unwrap();
    let off = a.index.blocks[1].frame_offset as usize;
    let p = payload_range(&d0, off);
    // Payload offsets: 10 is in `encoded_len`, 11 the stream's magic, 40 its zlib header data.
    for (at, want) in [
        (10, "BlockLengthMismatch"),
        (11, "BadRecord"),
        (40, "BadRecord"),
        ((p.end - p.start) / 2, "BadRecord"),
    ] {
        let mut d = d0.clone();
        d[p.start + at] ^= 0x10;
        rehash_frame(&mut d, off);
        let (class, detail) = verify_class(d);
        assert_eq!(class, want, "payload byte {at}: {detail}");
    }
    // A byte near the stream's end that the library does not use: the output is the same and
    // `original_hash` holds, so the copy verifies and extracts the original bytes.
    let mut d = d0.clone();
    d[p.end - 5] ^= 0x10;
    rehash_frame(&mut d, off);
    let mut m = Archive::open(d, Options::default()).unwrap();
    let want = strings(&table("expected-1.1.toml")[JPEG]["files"]);
    assert_eq!(decoded_lines(&mut m).unwrap(), want);
    // The lower block damaged: extraction of the peeled file reports that frame's HashMismatch.
    let mut d = d0.clone();
    let b0 = a.index.blocks[0];
    d[(b0.frame_offset + b0.frame_len / 2) as usize] ^= 1;
    let mut a = Archive::open(d, Options::default()).unwrap();
    let entries = a.entries().unwrap();
    assert_eq!(a.read_file(&entries[0]).unwrap_err().class, "HashMismatch");
}

/// Robustness on the revision 1.1 vector: bit flips (every 11th byte) never panic or verify clean.
#[test]
fn jpeg_mutations_never_panic_or_pass() {
    let data = read(JPEG);
    let mut flag_bytes = Vec::new();
    let mut pos = 32;
    while pos < data.len() {
        let f = lpk_check::wire::parse_frame(&data, pos, u64::MAX).unwrap();
        flag_bytes.push(pos + 2);
        pos = f.end;
    }
    for i in (0..data.len())
        .step_by(11)
        .filter(|i| !(10..12).contains(i))
    {
        let mut m = data.clone();
        let bit = 1u8 << (i % 8);
        m[i] ^= bit;
        if let Ok(mut a) = Archive::open(m, Options::default()) {
            let harmless = bit == 1 && flag_bytes.contains(&i);
            assert!(
                a.verify().is_err() || harmless,
                "flip at {i} verified clean"
            );
        }
    }
}
