#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::ChunkTable;

fuzz_target!(|data: &[u8]| {
    if let Ok(t) = ChunkTable::parse(data) {
        for c in t.iter() {
            let _ = c;
        }
        let _ = t.get(0);
        let _ = t.get(u64::MAX);
        let _ = t.validate();
    }
});
