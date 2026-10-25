//! The vp8-cross replay (PLAN LIC-2, task 1): every case of the oracle fixtures
//! `crates/lpk-core/tests/fixtures/vp8-cross/cases.jsonl.gz` (schema in its README), in both
//! directions: the writer's bytes equal the fixture's bytes, and the reader returns every value
//! of the operation sequence from the fixture's bytes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use lpk_codecs_vp8::{Branch, VPXBoolReader, VPXBoolWriter, Vp8Decode, Vp8Encode};

#[derive(Clone, Copy)]
enum Kind {
    Bit,
    NBits,
    Unary,
}

struct Op {
    kind: Kind,
    group: usize,
    width: u64,
    value: u64,
}

struct Case {
    id: u64,
    family: String,
    groups: Vec<usize>,
    ops: Vec<Op>,
    bytes: Vec<u8>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("lpk-core")
        .join("tests")
        .join("fixtures")
        .join("vp8-cross")
        .join("cases.jsonl.gz")
}

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2));
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn load() -> Vec<Case> {
    let file = std::fs::File::open(fixture_path()).expect("the vp8-cross fixtures");
    let reader = BufReader::new(flate2::read::GzDecoder::new(file));
    let mut cases = Vec::new();
    for line in reader.lines() {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let ops = v["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                let o = o.as_array().unwrap();
                let kind = match o[0].as_str().unwrap() {
                    "bit" => Kind::Bit,
                    "nbits" => Kind::NBits,
                    "unary" => Kind::Unary,
                    k => panic!("unknown kind {k}"),
                };
                Op {
                    kind,
                    group: o[1].as_u64().unwrap() as usize,
                    width: o[2].as_u64().unwrap(),
                    // u64, never through f64
                    value: o[3].as_u64().expect("a u64 value"),
                }
            })
            .collect();
        cases.push(Case {
            id: v["id"].as_u64().unwrap(),
            family: v["family"].as_str().unwrap().to_string(),
            groups: v["groups"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g.as_u64().unwrap() as usize)
                .collect(),
            ops,
            bytes: hex(v["bytes"].as_str().unwrap()),
        });
    }
    cases
}

fn fresh(groups: &[usize]) -> Vec<Vec<Branch>> {
    groups.iter().map(|&n| vec![Branch::new(); n]).collect()
}

fn write(case: &Case) -> Vec<u8> {
    let mut groups = fresh(&case.groups);
    let mut out = Vec::new();
    {
        let mut w = VPXBoolWriter::new(&mut out).unwrap();
        for op in &case.ops {
            let g = &mut groups[op.group];
            match op.kind {
                Kind::Bit => w.put(op.value != 0, &mut g[op.width as usize]).unwrap(),
                Kind::NBits => w.put_n_bits(op.value, op.width as usize, g).unwrap(),
                Kind::Unary => w.put_unary_encoded(op.value as usize, g).unwrap(),
            }
        }
        Vp8Encode::finish(&mut w).unwrap();
    }
    out
}

/// The index of the first operation whose value the reader does not return, if any.
fn read(case: &Case) -> Option<usize> {
    let mut groups = fresh(&case.groups);
    let mut r = VPXBoolReader::new(&case.bytes[..]).ok()?;
    for (k, op) in case.ops.iter().enumerate() {
        let g = &mut groups[op.group];
        let ok = match op.kind {
            Kind::Bit => r.get(&mut g[op.width as usize]).ok() == Some(op.value != 0),
            Kind::NBits => {
                let mask = if op.width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << op.width) - 1
                };
                r.get_n_bits(op.width as usize, g).ok() == Some(op.value & mask)
            }
            Kind::Unary => r.get_unary_encoded(g).ok() == Some(op.value as usize),
        };
        if !ok {
            return Some(k);
        }
    }
    None
}

#[test]
fn every_vp8_cross_case_replays_in_both_directions() {
    let cases = load();
    assert_eq!(cases.len(), 5415, "the fixture holds 5,415 cases");
    // family -> (cases, writer equal, reader equal)
    let mut tally: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut empty = 0;
    for (n, case) in cases.iter().enumerate() {
        assert_eq!(case.id, n as u64);
        empty += usize::from(case.bytes.is_empty());
        let t = tally.entry(case.family.clone()).or_default();
        t.0 += 1;
        let written = write(case);
        if written == case.bytes {
            t.1 += 1;
        } else if failures.len() < 10 {
            failures.push(format!(
                "case {} ({}): writer {} bytes, fixture {} bytes",
                case.id,
                case.family,
                written.len(),
                case.bytes.len()
            ));
        }
        match read(case) {
            None => t.2 += 1,
            Some(k) if failures.len() < 10 => {
                failures.push(format!(
                    "case {} ({}): reader differs at operation {k}",
                    case.id, case.family
                ));
            }
            Some(_) => {}
        }
    }
    println!("vp8-cross replay (family: cases, writer equal, reader equal): {tally:?}");
    assert_eq!(empty, 1011, "the fixture has 1,011 empty streams");
    assert!(failures.is_empty(), "{failures:#?}\n{tally:?}");
    let expected = [("a", 2400), ("b", 3002), ("c", 13)];
    for (family, n) in expected {
        assert_eq!(
            tally.get(family).copied(),
            Some((n, n, n)),
            "family {family}"
        );
    }
}
