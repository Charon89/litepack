#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::EntryTable;

fuzz_target!(|data: &[u8]| {
    if let Ok(t) = EntryTable::parse(data) {
        for e in t.iter() {
            let _ = e;
        }
        let _ = t.validate();
    }
});
