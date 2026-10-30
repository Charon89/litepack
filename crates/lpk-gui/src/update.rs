//! The update check's gate (E5-7; `docs/notes/e5-design.md` section 4, `docs/notes/e7-design.md`
//! section 6), plain Rust on every OS: the request itself is `tauri-plugin-updater`'s, made from
//! Rust by the host (`src/updates.rs`, Windows).
//!
//! The key and the endpoint live in one place, `tauri.conf.json`'s `plugins.updater`, which
//! E7-7 fills with the owner's public key and the release URL. Until then the key is
//! [`PLACEHOLDER_KEY`], and a key that is not a minisign public key fails closed: no request and
//! no install, never an unverified one. With the check off in the settings, nothing is
//! requested at all.

/// The key `tauri.conf.json` carries until E7-7 puts the owner's in.
pub const PLACEHOLDER_KEY: &str = "E7-7 replaces this with the release signing public key";

/// What the gate decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// The user turned the check off: no request.
    Off,
    /// No usable public key: no request (fail closed).
    NoKey,
    /// Ask the endpoint.
    Check,
}

/// The gate for a check with the setting `enabled` and the configured `pubkey`.
pub fn gate(enabled: bool, pubkey: &str) -> Gate {
    if !enabled {
        Gate::Off
    } else if !key_is_usable(pubkey) {
        Gate::NoKey
    } else {
        Gate::Check
    }
}

/// Whether `pubkey` is a Tauri updater public key: the base64 of a minisign public key file,
/// whose second line is the base64 of `Ed`, an 8-byte key id and a 32-byte Ed25519 key.
pub fn key_is_usable(pubkey: &str) -> bool {
    let Some(text) = base64(pubkey.trim()).and_then(|b| String::from_utf8(b).ok()) else {
        return false;
    };
    let mut lines = text.lines();
    let comment_ok = lines
        .next()
        .is_some_and(|l| l.starts_with("untrusted comment:"));
    let key = lines.next().and_then(|l| base64(l.trim()));
    comment_ok && key.is_some_and(|k| k.len() == 42 && k.starts_with(b"Ed"))
}

/// The updater's configuration in `tauri.conf.json`: its endpoints and public key.
pub fn config(conf: &serde_json::Value) -> (Vec<String>, String) {
    let u = &conf["plugins"]["updater"];
    let endpoints = u["endpoints"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    (
        endpoints,
        u["pubkey"].as_str().unwrap_or_default().to_owned(),
    )
}

/// Standard base64 with padding; `None` if `s` is not.
fn base64(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let chunks = s.len() / 4;
    for (i, q) in s.chunks(4).enumerate() {
        let pad = q.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && i + 1 != chunks) {
            return None;
        }
        let mut n = 0u32;
        for &c in &q[..4 - pad] {
            n = (n << 6) | val(c)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minisign public key made for these tests alone: random bytes, no private half exists.
    pub const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDQ2MkQ5NjQ3QTlDRTk0NDUKUldSRmxNNnBSNVl0UnVQbkVQMTlDd25BSGY4V083dHVYY3U0ajEralhBZ2RaazZKcE9RaWJGNVkK";

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("TWFu").unwrap(), b"Man");
        assert_eq!(base64("TWE=").unwrap(), b"Ma");
        assert_eq!(base64("TQ==").unwrap(), b"M");
        for bad in ["", "TWF", "TQ=a", "T===", "TQ==TWFu", "TW!u"] {
            assert!(base64(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_placeholder_and_malformed_keys_fail_closed() {
        assert!(key_is_usable(TEST_KEY));
        for bad in [
            PLACEHOLDER_KEY,
            "",
            "   ",
            "RWRFlM6pR5YtRuPnEP19CwnAHf8WO7tuXcu4j1+jXAgdZk6JpOQibF5Y",
            "dW50cnVzdGVk",
        ] {
            assert!(!key_is_usable(bad), "{bad}");
            assert_eq!(gate(true, bad), Gate::NoKey);
        }
        assert_eq!(gate(true, TEST_KEY), Gate::Check);
        assert_eq!(gate(false, TEST_KEY), Gate::Off);
        assert_eq!(gate(false, PLACEHOLDER_KEY), Gate::Off);
    }

    #[test]
    fn the_shipped_configuration_fails_closed_until_e7_7() {
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let (endpoints, key) = config(&conf);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(key, PLACEHOLDER_KEY);
        assert_eq!(gate(true, &key), Gate::NoKey);
    }
}
