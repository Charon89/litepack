#![no_main]
use libfuzzer_sys::fuzz_target;
use lpk_format::{Credentials, KeySlot};

fuzz_target!(|data: &[u8]| {
    if let Ok(slot) = KeySlot::parse(data) {
        // Keep the run cheap: skip slots that ask for a costly Argon2.
        if slot.argon2.m_kib > 16384 || slot.argon2.t > 2 {
            return;
        }
        let _ = slot.unwrap(&[0x5A; 16], 0, &Credentials::password(b"fuzz".to_vec()));
    }
});
