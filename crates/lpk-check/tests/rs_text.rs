//! Section 13's Reed-Solomon code, as `lpk-check` implements it from the text, checked against
//! the recovery shards the reference writer stored in `recovery-groups.lpk` (E1-15).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use lpk_check::recovery::{self, RecoveryHead};
use lpk_check::{rs, wire, Archive, Options};

fn vectors() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lpk-format/tests/vectors")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(vectors().join(name)).unwrap()
}

struct Group {
    head: RecoveryHead,
    recovery: Vec<u8>,
}

fn groups(data: &[u8]) -> (Archive, Vec<Group>) {
    let a = Archive::open(data.to_vec(), Options::default()).unwrap();
    let mut out = Vec::new();
    for l in &a.index.recovery {
        let end = (l.offset + l.len) as usize;
        let f = wire::parse_frame(&data[..end], l.offset as usize, u64::MAX).unwrap();
        let (head, _, rec) = recovery::parse_payload(f.payload, a.trailer.index_offset).unwrap();
        out.push(Group {
            head,
            recovery: rec.to_vec(),
        });
    }
    (a, out)
}

fn shards(data: &[u8], h: &RecoveryHead) -> Vec<Vec<u8>> {
    let sl = h.shard_len as usize;
    let start = h.cover_offset as usize;
    let cover = &data[start..start + h.cover_len as usize];
    cover
        .chunks(sl)
        .map(|c| {
            let mut v = c.to_vec();
            v.resize(sl, 0);
            v
        })
        .collect()
}

/// The encoding map, read from the text, reproduces every stored recovery shard.
#[test]
fn encoding_map_reproduces_the_stored_recovery_shards() {
    let data = read("recovery-groups.lpk");
    let (_, gs) = groups(&data);
    assert!(gs.len() >= 2, "the vector has several groups");
    for (n, g) in gs.iter().enumerate() {
        let d = shards(&data, &g.head);
        assert_eq!(d.len(), g.head.data_shards as usize);
        let refs: Vec<&[u8]> = d.iter().map(Vec::as_slice).collect();
        let y = rs::encode(g.head.group_shards, g.head.recovery_shards, &refs).unwrap();
        assert_eq!(y.concat(), g.recovery, "group {n}");
    }
}

fn damage(data: &mut [u8], h: &RecoveryHead, shard: u32) {
    let at = h.cover_offset as usize + shard as usize * h.shard_len as usize;
    data[at] ^= 0x5A;
}

fn repair(data: &[u8]) -> (recovery::Report, Vec<u8>) {
    let a = Archive::open(data.to_vec(), Options::default()).unwrap();
    let mut copy = data.to_vec();
    let rep = recovery::scan(
        data,
        &a.index.recovery,
        &a.index.generations,
        a.trailer.index_offset,
        Some(&mut copy),
        1 << 31,
    )
    .unwrap();
    (rep, copy)
}

/// Up to `recovery_shards` damaged shards in every group rebuild (any positions); one more is
/// `Unrepairable` and leaves that group's bytes as they were.
#[test]
fn erasures_up_to_capacity_rebuild_and_one_more_does_not() {
    let good = read("recovery-groups.lpk");
    let (_, gs) = groups(&good);
    for pick_last in [false, true] {
        let mut bad = good.clone();
        let mut want = 0;
        for g in &gs {
            let h = &g.head;
            let e = h.recovery_shards.min(h.data_shards);
            for t in 0..e {
                let s = if pick_last { h.data_shards - 1 - t } else { t };
                damage(&mut bad, h, s);
            }
            want += e as usize;
        }
        let (rep, copy) = repair(&bad);
        assert!(rep.error.is_none(), "{:?}", rep.error);
        assert_eq!((rep.damaged, rep.repaired), (want, want));
        assert!(copy == good);
    }
    // One group with one shard more than its capacity.
    let g = gs
        .iter()
        .position(|g| g.head.data_shards > g.head.recovery_shards)
        .expect("a group with more data shards than recovery shards");
    let h = &gs[g].head;
    let mut bad = good.clone();
    for s in 0..=h.recovery_shards {
        damage(&mut bad, h, s);
    }
    let (rep, copy) = repair(&bad);
    let err = rep.error.expect("unrepairable");
    assert_eq!(err.class, "Unrepairable");
    assert_eq!(rep.repaired, 0);
    assert!(copy == bad);
}
