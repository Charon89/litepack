//! A small zlib (RFC 1950) / Deflate (RFC 1951) inflater with an output bound.
//!
//! Only the stream-header check of `jpeg-reconstruct` (section 8, check 5) needs it: it reads the
//! zlib-compressed Lepton header far enough to find the JPEG frame header. Written from the RFCs.

/// Why inflation stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The stream is malformed or ends early.
    Bad,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            bit: 0,
            nbits: 0,
        }
    }

    fn need(&mut self, n: u32) -> Result<(), Stop> {
        while self.nbits < n {
            let b = *self.data.get(self.pos).ok_or(Stop::Bad)?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }

    fn bits(&mut self, n: u32) -> Result<u32, Stop> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }

    fn align(&mut self) {
        self.bit = 0;
        self.nbits = 0;
    }
}

/// A canonical Huffman decoding table (counts per length and symbols in code order).
struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Self, Stop> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        count[0] = 0;
        let mut left: i32 = 1;
        for &c in &count[1..] {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err(Stop::Bad);
            }
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[usize::from(offs[usize::from(l)])] = s as u16;
                offs[usize::from(l)] += 1;
            }
        }
        Ok(Self { count, symbol })
    }

    fn decode(&self, b: &mut Bits<'_>) -> Result<u16, Stop> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= b.bits(1)? as i32;
            let count = i32::from(self.count[len]);
            if code - count < first {
                return self
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or(Stop::Bad);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Stop::Bad)
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Inflates a zlib stream into at most `limit` bytes. Returns the bytes produced and whether it
/// stopped because of damage (the bytes produced before the damage are kept). Reaching `limit`
/// is not an error; the stream's checksum is not checked.
pub fn zlib_prefix(data: &[u8], limit: usize) -> (Vec<u8>, Option<Stop>) {
    let mut out = Vec::new();
    let r = zlib(data, limit, &mut out);
    (out, r.err())
}

fn zlib(data: &[u8], limit: usize, out: &mut Vec<u8>) -> Result<(), Stop> {
    let (cmf, flg) = match data {
        [a, b, ..] => (*a, *b),
        _ => return Err(Stop::Bad),
    };
    if cmf & 0x0F != 8 || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return Err(Stop::Bad);
    }
    let mut b = Bits::new(&data[2..]);
    loop {
        let last = b.bits(1)?;
        match b.bits(2)? {
            0 => {
                b.align();
                let p = b.pos;
                let hdr = b.data.get(p..p + 4).ok_or(Stop::Bad)?;
                let len = u16::from_le_bytes([hdr[0], hdr[1]]);
                let nlen = u16::from_le_bytes([hdr[2], hdr[3]]);
                if len != !nlen {
                    return Err(Stop::Bad);
                }
                let s = b.data.get(p + 4..p + 4 + usize::from(len));
                let s = s.ok_or(Stop::Bad)?;
                let take = s.len().min(limit - out.len());
                out.extend_from_slice(&s[..take]);
                b.pos = p + 4 + usize::from(len);
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                let lit = Huff::new(&l)?;
                let dist = Huff::new(&[5u8; 30])?;
                codes(&mut b, &lit, &dist, limit, out)?;
            }
            2 => {
                let (lit, dist) = dynamic(&mut b)?;
                codes(&mut b, &lit, &dist, limit, out)?;
            }
            _ => return Err(Stop::Bad),
        }
        if out.len() >= limit || last == 1 {
            return Ok(());
        }
    }
}

fn dynamic(b: &mut Bits<'_>) -> Result<(Huff, Huff), Stop> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = b.bits(5)? as usize + 257;
    let ndist = b.bits(5)? as usize + 1;
    let ncode = b.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(Stop::Bad);
    }
    let mut cl = [0u8; 19];
    for &o in ORDER.iter().take(ncode) {
        cl[o] = b.bits(3)? as u8;
    }
    let ch = Huff::new(&cl)?;
    let mut lengths = vec![0u8; nlen + ndist];
    let mut i = 0;
    while i < nlen + ndist {
        let sym = ch.decode(b)?;
        let (val, rep) = match sym {
            0..=15 => (sym as u8, 1),
            16 => {
                let prev = *lengths.get(i.wrapping_sub(1)).ok_or(Stop::Bad)?;
                (prev, 3 + b.bits(2)? as usize)
            }
            17 => (0, 3 + b.bits(3)? as usize),
            _ => (0, 11 + b.bits(7)? as usize),
        };
        if i + rep > nlen + ndist {
            return Err(Stop::Bad);
        }
        lengths[i..i + rep].fill(val);
        i += rep;
    }
    if lengths[256] == 0 {
        return Err(Stop::Bad);
    }
    Ok((Huff::new(&lengths[..nlen])?, Huff::new(&lengths[nlen..])?))
}

fn codes(
    b: &mut Bits<'_>,
    lit: &Huff,
    dist: &Huff,
    limit: usize,
    out: &mut Vec<u8>,
) -> Result<(), Stop> {
    loop {
        if out.len() >= limit {
            return Ok(());
        }
        let sym = lit.decode(b)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            _ => {
                let s = usize::from(sym - 257);
                let len = usize::from(*LBASE.get(s).ok_or(Stop::Bad)?)
                    + b.bits(u32::from(LEXT[s]))? as usize;
                let d = usize::from(dist.decode(b)?);
                let dd = usize::from(*DBASE.get(d).ok_or(Stop::Bad)?)
                    + b.bits(u32::from(DEXT[d]))? as usize;
                if dd > out.len() {
                    return Err(Stop::Bad);
                }
                for _ in 0..len {
                    if out.len() >= limit {
                        return Ok(());
                    }
                    out.push(out[out.len() - dd]);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stored_block_and_limit() {
        // zlib header 78 01, a final stored block of "hello", adler omitted.
        let s = [
            0x78, 0x01, 0x01, 0x05, 0x00, 0xFA, 0xFF, b'h', b'e', b'l', b'l', b'o',
        ];
        assert_eq!(zlib_prefix(&s, 100), (b"hello".to_vec(), None));
        assert_eq!(zlib_prefix(&s, 3), (b"hel".to_vec(), None));
        assert_eq!(zlib_prefix(&s[..9], 100).1, Some(Stop::Bad));
        assert_eq!(zlib_prefix(&[0x78, 0x02], 100).1, Some(Stop::Bad));
    }

    #[test]
    fn fixed_huffman_block() {
        // zlib.compress(b"aaaaaaaaaa") = 78 9c 4b 4c 84 01 00 14 e1 03 cb
        let s = [
            0x78, 0x9c, 0x4b, 0x4c, 0x84, 0x01, 0x00, 0x14, 0xe1, 0x03, 0xcb,
        ];
        assert_eq!(zlib_prefix(&s, 100), (b"aaaaaaaaaa".to_vec(), None));
    }
}
