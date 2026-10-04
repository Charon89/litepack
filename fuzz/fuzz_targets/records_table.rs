#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::RecordsTable;

fuzz_target!(|data: &[u8]| {
    if let Ok(t) = RecordsTable::parse(data) {
        for r in t.iter() {
            let _ = r;
        }
        let _ = t.get(0);
        let _ = t.get(u64::MAX);
        let _ = t.validate();
    }
});
