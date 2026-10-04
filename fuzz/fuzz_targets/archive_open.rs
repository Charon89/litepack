#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::{Archive, Credentials, Resources};
use std::io::Cursor;

fn tight() -> Resources {
    Resources {
        max_window: 1 << 24,
        max_bwt_block: 1 << 20,
        max_block_plain: 1 << 22,
        max_frame_payload: 1 << 22,
        memory: 1 << 26,
    }
}

fn exercise(data: &[u8], creds: Option<&Credentials>) {
    let Ok(mut a) = Archive::open_with(Cursor::new(data.to_vec()), &tight(), creds) else {
        return;
    };
    let _ = a.verify();
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
    exercise(data, None);
    // Argon2 memory above the tight limit is refused, so this stays cheap.
    exercise(data, Some(&Credentials::password(b"fuzz".to_vec())));
});
