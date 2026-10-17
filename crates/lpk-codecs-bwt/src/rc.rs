//! A binary range coder (the carry-propagating structure of the LZMA range coder, written here)
//! with 16-bit probabilities and adaptive two-rate bit models.

/// Probability scale: a probability is `P(bit = 0)` in units of 1/65536.
const PROB_BITS: u32 = 16;
const TOP: u32 = 1 << 24;
/// Keeps every probability away from 0 and 1, so both symbols keep a non-empty interval.
const P_MIN: u32 = 64;
const P_MAX: u32 = (1 << PROB_BITS) - 64;

/// An adaptive bit model: two estimates, a fast one and a slow one, averaged.
#[derive(Debug, Clone, Copy)]
pub struct Bit {
    fast: u16,
    slow: u16,
}

impl Default for Bit {
    fn default() -> Self {
        Bit {
            fast: 1 << 15,
            slow: 1 << 15,
        }
    }
}

impl Bit {
    fn p0(self) -> u32 {
        ((u32::from(self.fast) + u32::from(self.slow)) >> 1).clamp(P_MIN, P_MAX)
    }

    fn update(&mut self, bit: bool) {
        if bit {
            self.fast -= self.fast >> 4;
            self.slow -= self.slow >> 7;
        } else {
            self.fast += (u16::MAX - self.fast) >> 4;
            self.slow += (u16::MAX - self.slow) >> 7;
        }
    }
}

/// The encoder.
#[derive(Debug)]
pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl Encoder {
    pub fn new(capacity: usize) -> Self {
        Encoder {
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            out: Vec::with_capacity(capacity),
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    pub fn encode(&mut self, m: &mut Bit, bit: bool) {
        let bound = (self.range >> PROB_BITS) * m.p0();
        if bit {
            self.low += u64::from(bound);
            self.range -= bound;
        } else {
            self.range = bound;
        }
        m.update(bit);
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

/// The decoder. Reading past the end gives zero bytes: damaged input decodes to wrong bits,
/// never to a panic; the callers bound their output.
#[derive(Debug)]
pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = Decoder {
            data,
            pos: 0,
            range: u32::MAX,
            code: 0,
        };
        for _ in 0..5 {
            d.code = (d.code << 8) | u32::from(d.byte());
        }
        d
    }

    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    /// True when the decoder has read more bytes than the stream holds (beyond the flush).
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len()
    }

    pub fn decode(&mut self, m: &mut Bit) -> bool {
        let bound = (self.range >> PROB_BITS) * m.p0();
        let bit = if self.code < bound {
            self.range = bound;
            false
        } else {
            self.code -= bound;
            self.range -= bound;
            true
        };
        m.update(bit);
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | u32::from(self.byte());
        }
        bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip_with_skewed_and_random_sources() {
        let mut s = 12345u64;
        let mut bits = Vec::new();
        for i in 0..200_000u32 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            // Mostly zeros, then a random stretch, then mostly ones (carry runs).
            let bit = match i / 50_000 {
                0 => s.is_multiple_of(50),
                1 => s & 1 == 1,
                _ => !s.is_multiple_of(1000),
            };
            bits.push(bit);
        }
        let mut e = Encoder::new(0);
        let mut models = [Bit::default(); 4];
        for (i, &b) in bits.iter().enumerate() {
            e.encode(&mut models[i % 4], b);
        }
        let out = e.finish();
        let mut d = Decoder::new(&out);
        let mut models = [Bit::default(); 4];
        for (i, &b) in bits.iter().enumerate() {
            assert_eq!(d.decode(&mut models[i % 4]), b, "bit {i}");
        }
        assert!(!d.overrun());
    }

    #[test]
    fn a_constant_source_costs_little() {
        let mut e = Encoder::new(0);
        let mut m = Bit::default();
        for _ in 0..1_000_000 {
            e.encode(&mut m, false);
        }
        let out = e.finish();
        // The floor P_MIN/65536 bounds the cost at about 0.0014 bit per bit.
        assert!(out.len() < 400, "{}", out.len());
    }
}
