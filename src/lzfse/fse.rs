//! Finite State Entropy (tANS) primitives — port of lzfse_fse.{h,c}.
//!
//! Only the 64-bit I/O stream variant exists here (FSE_IOSTREAM_64), matching
//! Apple's build on every 64-bit target. The stream is a little-endian
//! LSB-first bit sequence, so the port is endian-independent while remaining
//! bit-exact with the reference on Apple platforms.

#[inline]
pub fn mask_lsb64(x: u64, nbits: i32) -> u64 {
    debug_assert!((0..=64).contains(&nbits));
    if nbits >= 64 {
        x
    } else {
        x & ((1u64 << nbits) - 1)
    }
}

// ------------------------------------------------------------------
//  Bit streams
// ------------------------------------------------------------------

/// Output stream: bits accumulate LSB-first, flushed to bytes.
pub struct FseOutStream {
    pub accum: u64,
    /// Number of valid bits in `accum`; goes negative (down to -7) after
    /// `finish` pads the final byte.
    pub accum_nbits: i32,
}

impl FseOutStream {
    pub fn new() -> Self {
        FseOutStream { accum: 0, accum_nbits: 0 }
    }

    /// Accumulate `n` bits `b`. Caller must flush before exceeding 64 bits.
    #[inline]
    pub fn push(&mut self, n: i32, b: u64) {
        debug_assert!(self.accum_nbits + n <= 64);
        self.accum |= b << self.accum_nbits;
        self.accum_nbits += n;
    }

    /// Write full bytes to `dst` at `*pos`, leaving 0-7 bits in the
    /// accumulator. Always stores 8 bytes (later writes overwrite the slack),
    /// exactly like the reference.
    #[inline]
    pub fn flush(&mut self, dst: &mut [u8], pos: &mut usize) {
        let nbits = self.accum_nbits & -8;
        dst[*pos..*pos + 8].copy_from_slice(&self.accum.to_le_bytes());
        *pos += (nbits >> 3) as usize;
        self.accum >>= nbits; // nbits < 64: accumulator never fills completely
        self.accum_nbits -= nbits;
    }

    /// Write the last bytes, zero-padding to a byte boundary;
    /// `accum_nbits` ends up in [-7, 0].
    #[inline]
    pub fn finish(&mut self, dst: &mut [u8], pos: &mut usize) {
        let nbits = (self.accum_nbits + 7) & -8;
        dst[*pos..*pos + 8].copy_from_slice(&self.accum.to_le_bytes());
        *pos += (nbits >> 3) as usize;
        self.accum = 0;
        self.accum_nbits -= nbits;
    }
}

/// Input stream, read *backwards* through the payload.
#[derive(Clone, Copy, Default)]
pub struct FseInStream {
    pub accum: u64,
    pub accum_nbits: i32,
}

/// Load up to 8 bytes at `pos` (little-endian), zero-padding past the end of
/// `src`. The reference does an unconditional 8-byte load here but only ever
/// consumes bits backed by in-range bytes, so the padding is unobservable.
#[inline]
fn load8_padded(src: &[u8], pos: usize) -> u64 {
    if pos + 8 <= src.len() {
        u64::from_le_bytes(src[pos..pos + 8].try_into().unwrap())
    } else {
        let mut b = [0u8; 8];
        let n = src.len().saturating_sub(pos);
        b[..n].copy_from_slice(&src[pos..pos + n]);
        u64::from_le_bytes(b)
    }
}

impl FseInStream {
    /// Initialize so the accumulator holds 56..=63 bits, reading backwards
    /// from `*pbuf`. `n` is the encoder's final `accum_nbits` in [-7, 0].
    ///
    /// `n` reaches here straight from a stream header, so the additions below
    /// use checked arithmetic: the C original lets them wrap (signed overflow
    /// UB that happens to be benign there, since the range test right after
    /// rejects the result), but in Rust an unchecked add panics under overflow
    /// checks. Callers also validate the field; this is the backstop.
    pub fn init(&mut self, n: i32, pbuf: &mut usize, buf_start: usize, src: &[u8]) -> Result<(), ()> {
        if n != 0 {
            if *pbuf < buf_start + 8 {
                return Err(());
            }
            *pbuf -= 8;
            self.accum = u64::from_le_bytes(src[*pbuf..*pbuf + 8].try_into().unwrap());
            self.accum_nbits = n.checked_add(64).ok_or(())?;
        } else {
            if *pbuf < buf_start + 7 {
                return Err(());
            }
            *pbuf -= 7;
            let mut b = [0u8; 8];
            b[..7].copy_from_slice(&src[*pbuf..*pbuf + 7]);
            self.accum = u64::from_le_bytes(b);
            self.accum_nbits = n.checked_add(56).ok_or(())?;
        }
        if !(56..64).contains(&self.accum_nbits) || (self.accum >> self.accum_nbits) != 0 {
            return Err(());
        }
        Ok(())
    }

    /// Refill to 56..=63 bits, reading backwards; error if we'd pass
    /// `buf_start`.
    #[inline]
    pub fn flush(&mut self, pbuf: &mut usize, buf_start: usize, src: &[u8]) -> Result<(), ()> {
        let nbits = (63 - self.accum_nbits) & -8;
        let dec = (nbits >> 3) as usize;
        if *pbuf < buf_start + dec {
            return Err(());
        }
        *pbuf -= dec;
        let incoming = load8_padded(src, *pbuf);
        self.accum = (self.accum << nbits) | mask_lsb64(incoming, nbits);
        self.accum_nbits += nbits;
        Ok(())
    }

    /// Pull `n` bits (MSB side of the valid window).
    #[inline]
    pub fn pull(&mut self, n: i32) -> u64 {
        debug_assert!(n >= 0 && n <= self.accum_nbits);
        self.accum_nbits -= n;
        let result = self.accum >> self.accum_nbits;
        self.accum = mask_lsb64(self.accum, self.accum_nbits);
        result
    }
}

// ------------------------------------------------------------------
//  Encode / decode primitives
// ------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
pub struct FseEncoderEntry {
    pub s0: i16,     // first state requiring a k-bit shift
    pub k: i16,      // states >= s0 shift k bits, others k-1
    pub delta0: i16, // next-state increment if s >= s0
    pub delta1: i16, // next-state increment if s < s0
}

/// Value-decoder entry: state bits + extra value bits in one pull.
#[derive(Clone, Copy, Default)]
pub struct FseValueDecoderEntry {
    pub total_bits: u8,
    pub value_bits: u8,
    pub delta: i16,
    pub vbase: i32,
}

#[inline]
pub fn fse_encode(pstate: &mut u16, table: &[FseEncoderEntry], out: &mut FseOutStream, symbol: u8) {
    let s = *pstate as i32;
    let e = table[symbol as usize];
    let hi = s >= e.s0 as i32;
    let nbits = if hi { e.k as i32 } else { e.k as i32 - 1 };
    let delta = if hi { e.delta0 } else { e.delta1 };
    out.push(nbits, mask_lsb64(s as u64, nbits));
    *pstate = (delta as i32).wrapping_add(s >> nbits) as u16;
}

/// Decode one symbol from a packed literal-decoder entry
/// (`k | symbol << 8 | delta << 16`, exactly the reference's memory layout).
#[inline]
pub fn fse_decode(pstate: &mut u16, table: &[i32], input: &mut FseInStream) -> u8 {
    let e = table[*pstate as usize];
    *pstate = ((e >> 16) as u16).wrapping_add(input.pull(e & 0xff) as u16);
    ((e >> 8) & 0xff) as u8
}

#[inline]
pub fn fse_value_decode(
    pstate: &mut u16,
    table: &[FseValueDecoderEntry],
    input: &mut FseInStream,
) -> i32 {
    let entry = table[*pstate as usize];
    let state_and_value_bits = input.pull(entry.total_bits as i32) as u32;
    *pstate = (entry.delta as i32)
        .wrapping_add((state_and_value_bits >> entry.value_bits) as i32) as u16;
    entry
        .vbase
        .wrapping_add(mask_lsb64(state_and_value_bits as u64, entry.value_bits as i32) as i32)
}

// ------------------------------------------------------------------
//  Table construction
// ------------------------------------------------------------------

pub fn fse_check_freq(freq: &[u16], number_of_states: usize) -> bool {
    freq.iter().map(|&f| f as usize).sum::<usize>() <= number_of_states
}

pub fn fse_init_encoder_table(nstates: i32, freq: &[u16], t: &mut [FseEncoderEntry]) {
    let mut offset = 0i32;
    let n_clz = (nstates as u32).leading_zeros() as i32;
    for (i, &fu) in freq.iter().enumerate() {
        let f = fu as i32;
        if f == 0 {
            continue;
        }
        let k = (f as u32).leading_zeros() as i32 - n_clz; // n <= f<<k < 2n
        t[i].s0 = ((f << k) - nstates) as i16;
        t[i].k = k as i16;
        t[i].delta0 = (offset - f + (nstates >> k)) as i16;
        // k == 0 means f == nstates and delta1 is unreachable (s0 == 0);
        // hardware shift-by--1 yields nstates >> 31 == 0 in the reference.
        t[i].delta1 = (offset - f + if k > 0 { nstates >> (k - 1) } else { 0 }) as i16;
        offset += f;
    }
}

/// Build the packed literal decoder table. Only `sum(freq)` entries are
/// written sequentially; the rest of `t` keeps its previous contents, exactly
/// like the reference (which reuses the block state between blocks).
pub fn fse_init_decoder_table(nstates: i32, freq: &[u16], t: &mut [i32]) -> Result<(), ()> {
    let n_clz = (nstates as u32).leading_zeros() as i32;
    let mut sum_of_freq = 0i32;
    let mut idx = 0usize;
    for (i, &fu) in freq.iter().enumerate() {
        let f = fu as i32;
        if f == 0 {
            continue;
        }
        sum_of_freq += f;
        if sum_of_freq > nstates {
            return Err(());
        }
        let k = (f as u32).leading_zeros() as i32 - n_clz;
        let j0 = ((2 * nstates) >> k) - f;
        for j in 0..f {
            let (kk, delta) = if j < j0 {
                (k, (((f + j) << k) - nstates) as i16)
            } else {
                (k - 1, ((j - j0) << (k - 1)) as i16)
            };
            t[idx] = (kk as u8 as i32) | ((i as i32) << 8) | ((delta as i32) << 16);
            idx += 1;
        }
    }
    Ok(())
}

/// Build a value decoder table (same partial-write semantics as above).
pub fn fse_init_value_decoder_table(
    nstates: i32,
    freq: &[u16],
    symbol_vbits: &[u8],
    symbol_vbase: &[i32],
    t: &mut [FseValueDecoderEntry],
) {
    let n_clz = (nstates as u32).leading_zeros() as i32;
    let mut idx = 0usize;
    for (i, &fu) in freq.iter().enumerate() {
        let f = fu as i32;
        if f == 0 {
            continue;
        }
        let k = (f as u32).leading_zeros() as i32 - n_clz;
        let j0 = ((2 * nstates) >> k) - f;
        for j in 0..f {
            let mut e = FseValueDecoderEntry {
                value_bits: symbol_vbits[i],
                vbase: symbol_vbase[i],
                ..Default::default()
            };
            if j < j0 {
                e.total_bits = k as u8 + e.value_bits;
                e.delta = (((f + j) << k) - nstates) as i16;
            } else {
                e.total_bits = (k - 1) as u8 + e.value_bits;
                e.delta = ((j - j0) << (k - 1)) as i16;
            }
            t[idx] = e;
            idx += 1;
        }
    }
}

/// Remove states from symbols until exactly `nstates` are used.
fn fse_adjust_freqs(freq: &mut [u16], mut overrun: i32) {
    let mut shift = 3i32;
    while overrun != 0 {
        debug_assert!(shift >= 0);
        for sym in 0..freq.len() {
            if freq[sym] > 1 {
                let mut n = (freq[sym] as i32 - 1) >> shift;
                if n > overrun {
                    n = overrun;
                }
                freq[sym] = (freq[sym] as i32 - n) as u16;
                overrun -= n;
                if overrun == 0 {
                    break;
                }
            }
        }
        shift -= 1;
    }
}

/// Normalize occurrence counts `t` to frequencies summing to `nstates`.
pub fn fse_normalize_freq(nstates: i32, t: &[u32], freq: &mut [u16]) {
    let mut s_count = 0u32;
    for &x in t {
        s_count = s_count.wrapping_add(x);
    }
    let shift = (nstates as u32).leading_zeros() as i32 - 1;
    let highprec_step: u32 = if s_count == 0 { 0 } else { (1u32 << 31) / s_count };

    let mut remaining = nstates;
    let mut max_freq = 0i32;
    let mut max_freq_sym = 0usize;
    for i in 0..t.len() {
        // Round-to-nearest rescale in u32 arithmetic, exactly as the reference.
        let mut f = ((t[i].wrapping_mul(highprec_step) >> shift).wrapping_add(1) >> 1) as i32;
        if f == 0 && t[i] != 0 {
            f = 1;
        }
        freq[i] = f as u16;
        remaining -= f;
        if f > max_freq {
            max_freq = f;
            max_freq_sym = i;
        }
    }

    if -remaining < (max_freq >> 2) {
        freq[max_freq_sym] = (freq[max_freq_sym] as i32 + remaining) as u16;
    } else {
        fse_adjust_freqs(freq, -remaining);
    }
}
