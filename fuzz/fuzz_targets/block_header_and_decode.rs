#![no_main]
use libfuzzer_sys::fuzz_target;
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
    let registry = Registry::v1();
    let limits = tight();
    for records in [0u64, 4] {
        if let Ok((header, used)) = BlockHeader::parse(data, 0, records) {
            let _ = decode_block(&registry, &header, 0, &data[used..], &limits);
        }
    }
});
