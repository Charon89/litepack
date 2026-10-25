//! The bootstrap block: the handle values the broker duplicated into the worker.
//!
//! Windows: the broker commits one page in the suspended child at the first free of four
//! addresses fixed before creation (named on the command line as `--lpk-bootstrap=a,b,c,d`), writes
//! this block there and resumes the child. The child takes the first committed candidate whose
//! block validates; when none does it refuses to start. Linux: the fds are named directly on the
//! command line (`--lpk-fds=role:fd,...`); the block format is not used there.
//!
//! Layout (little-endian): magic `LPKBOOT\0` (8), version u32, total length u32, count u32,
//! reserved u32 (0), `count` entries of {role u32, reserved u32 (0), value u64}, then FNV-1a 64 of
//! every preceding byte.

/// Command-line prefix naming the four candidate addresses (Windows).
pub const ARG_PREFIX: &str = "--lpk-bootstrap=";
/// Command-line prefix naming the passed fds (Linux).
pub const FDS_PREFIX: &str = "--lpk-fds=";
/// The block's magic.
pub const MAGIC: [u8; 8] = *b"LPKBOOT\0";
/// The block's version.
pub const VERSION: u32 = 1;
/// The most handles one block carries.
pub const MAX_HANDLES: usize = 16;
/// Size of the page the block lives in.
pub const PAGE: usize = 4096;
const HEADER: usize = 8 + 4 + 4 + 4 + 4;
const ENTRY: usize = 16;

/// Why a bootstrap block (or its command-line argument) was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapError {
    /// No `--lpk-bootstrap=` / `--lpk-fds=` argument.
    MissingArgument,
    /// The argument does not parse.
    BadArgument(String),
    /// No candidate address holds a committed, readable page.
    NoPage,
    /// A committed page was found but its block is malformed.
    Malformed(&'static str),
    /// The operating system refused something while taking the handles.
    Os(String),
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingArgument => write!(f, "no bootstrap argument on the command line"),
            Self::BadArgument(a) => write!(f, "bootstrap argument does not parse: {a}"),
            Self::NoPage => write!(f, "no bootstrap page at any candidate address"),
            Self::Malformed(why) => write!(f, "bootstrap block malformed: {why}"),
            Self::Os(e) => write!(f, "bootstrap: {e}"),
        }
    }
}

impl std::error::Error for BootstrapError {}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Encode `(role, value)` pairs into a block.
pub fn encode(entries: &[(u32, u64)]) -> Result<Vec<u8>, BootstrapError> {
    if entries.len() > MAX_HANDLES {
        return Err(BootstrapError::Malformed("too many handles"));
    }
    let total = HEADER + entries.len() * ENTRY + 8;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for &(role, value) in entries {
        out.extend_from_slice(&role.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    let sum = fnv1a(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    Ok(out)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// Decode and validate a block (which may be followed by padding up to the page end).
pub fn decode(page: &[u8]) -> Result<Vec<(u32, u64)>, BootstrapError> {
    if page.len() < HEADER + 8 {
        return Err(BootstrapError::Malformed("short"));
    }
    if page[..8] != MAGIC {
        return Err(BootstrapError::Malformed("magic"));
    }
    if u32_at(page, 8) != VERSION {
        return Err(BootstrapError::Malformed("version"));
    }
    let total = u32_at(page, 12) as usize;
    let count = u32_at(page, 16) as usize;
    if count > MAX_HANDLES {
        return Err(BootstrapError::Malformed("count"));
    }
    if total != HEADER + count * ENTRY + 8 || total > page.len() {
        return Err(BootstrapError::Malformed("length"));
    }
    if u32_at(page, 20) != 0 {
        return Err(BootstrapError::Malformed("reserved"));
    }
    if fnv1a(&page[..total - 8]) != u64_at(page, total - 8) {
        return Err(BootstrapError::Malformed("checksum"));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER + i * ENTRY;
        if u32_at(page, at + 4) != 0 {
            return Err(BootstrapError::Malformed("reserved"));
        }
        out.push((u32_at(page, at), u64_at(page, at + 8)));
    }
    Ok(out)
}

/// Parse `--lpk-bootstrap=0x..,0x..,0x..,0x..` into four addresses.
pub fn parse_addresses(arg: &str) -> Result<[u64; 4], BootstrapError> {
    let bad = || BootstrapError::BadArgument(arg.to_string());
    let list = arg.strip_prefix(ARG_PREFIX).ok_or_else(bad)?;
    let mut out = [0u64; 4];
    let mut n = 0;
    for part in list.split(',') {
        if n == 4 {
            return Err(bad());
        }
        let hex = part.strip_prefix("0x").ok_or_else(bad)?;
        out[n] = u64::from_str_radix(hex, 16).map_err(|_| bad())?;
        if out[n] == 0 || out[n] % PAGE as u64 != 0 {
            return Err(bad());
        }
        n += 1;
    }
    if n != 4 {
        return Err(bad());
    }
    Ok(out)
}

/// Format four addresses as the command-line argument.
pub fn format_addresses(a: &[u64; 4]) -> String {
    format!(
        "{ARG_PREFIX}0x{:x},0x{:x},0x{:x},0x{:x}",
        a[0], a[1], a[2], a[3]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_padding() {
        let e = vec![(1, 0x44), (2, 0x48), (10, 0x1234_5678_9abc)];
        let mut page = encode(&e).expect("encode");
        page.resize(PAGE, 0xAA);
        assert_eq!(decode(&page).expect("decode"), e);
        assert_eq!(
            decode(&encode(&[]).expect("encode")).expect("decode"),
            vec![]
        );
    }

    #[test]
    fn malformed_blocks_are_refused() {
        let good = encode(&[(1, 4), (2, 8)]).expect("encode");
        assert_eq!(
            decode(&[0u8; PAGE]),
            Err(BootstrapError::Malformed("magic"))
        );
        assert_eq!(decode(&good[..10]), Err(BootstrapError::Malformed("short")));
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0x01;
            assert!(decode(&b).is_err(), "flip at byte {i} accepted");
        }
        let mut b = good.clone();
        b.truncate(good.len() - 1);
        assert!(decode(&b).is_err());
        assert!(encode(&[(0, 0); MAX_HANDLES + 1]).is_err());
    }

    #[test]
    fn addresses_round_trip_and_reject_garbage() {
        let a = [0x10_0000_0000, 0x20_0001_0000, 0x6fff_0000_0000, 0x1000];
        assert_eq!(parse_addresses(&format_addresses(&a)), Ok(a));
        for bad in [
            "--lpk-bootstrap=",
            "--lpk-bootstrap=0x1000,0x2000,0x3000",
            "--lpk-bootstrap=0x1000,0x2000,0x3000,0x4000,0x5000",
            "--lpk-bootstrap=0x1001,0x2000,0x3000,0x4000",
            "--lpk-bootstrap=0x0,0x2000,0x3000,0x4000",
            "--lpk-bootstrap=1000,0x2000,0x3000,0x4000",
            "--other=0x1000,0x2000,0x3000,0x4000",
        ] {
            assert!(parse_addresses(bad).is_err(), "{bad} accepted");
        }
    }
}
