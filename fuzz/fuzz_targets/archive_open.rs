#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::Credentials;
use lpk_format_fuzz::{exercise, key_slot_too_costly};

fuzz_target!(|data: &[u8]| {
    if key_slot_too_costly(data) {
        return;
    }
    exercise(data, None);
    // Argon2 above the cost bound was skipped, memory above the limit is refused.
    exercise(data, Some(&Credentials::password(b"fuzz".to_vec())));
});
