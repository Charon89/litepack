#![no_main]
use libfuzzer_sys::fuzz_target;
//! The block header and the graph over arbitrary bytes, under the 1.0 registry and under the
//! full reader's decoders (revision 1.3's `bwt` runs without an archive; the reconstruction
//! decoders report their record as out of range).
use lpk_format::{decode_block, BlockHeader, Registry, Resources};

fn tight() -> Resources {
    Resources {
        max_window: 1 << 20,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    }
}

fuzz_target!(|data: &[u8]| {
    let mut full = Registry::v1();
    lpk_core::register_full_decoders(&mut full);
    let limits = tight();
    for registry in [Registry::v1(), full] {
        for records in [0u64, 4] {
            if let Ok((header, used)) = BlockHeader::parse(data, 0, records) {
                let _ = decode_block(&registry, &header, 0, &data[used..], &limits);
            }
        }
    }
});
