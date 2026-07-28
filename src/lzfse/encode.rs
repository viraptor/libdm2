//! LZFSE encoder — port of lzfse_encode_base.c / lzfse_encode.c, following
//! the libcompression variant of the algorithm rather than the published
//! sources (see the module docs and deepmap2.md for the two differences).
//!
//! The bvxn path (inputs of 8..4095 bytes) uses our lzvn encoder, which
//! emits a compatible but not byte-identical payload; deepmap2 never routes
//! through it.

use super::fse::*;
use super::*;

const LZFSE_ENCODE_HASH_BITS: u32 = 14;
const LZFSE_ENCODE_HASH_VALUES: usize = 1 << LZFSE_ENCODE_HASH_BITS;
const LZFSE_ENCODE_HASH_WIDTH: usize = 4;
const LZFSE_ENCODE_GOOD_MATCH: u32 = 40;
const LZFSE_ENCODE_LZVN_THRESHOLD: usize = 4096;
const LZVN_ENCODE_MIN_SRC_SIZE: usize = 8;
/// Forward match length limit; matches may still expand backwards past it.
const LZFSE_ENCODE_MAX_MATCH_LENGTH: u32 = 100 * LZFSE_ENCODE_MAX_M_VALUE;
const INVALID_POS: i32 = (-4 * LZFSE_ENCODE_MAX_D_VALUE) as i32;

/// Knuth multiplicative hash of 4 bytes.
#[inline]
fn hash_x(x: u32) -> u32 {
    x.wrapping_mul(2654435761) >> (32 - LZFSE_ENCODE_HASH_BITS)
}

#[inline]
fn l_base_from_value(value: i32) -> u8 {
    match value {
        0..=15 => value as u8,
        16..=19 => 16,
        20..=27 => 17,
        28..=59 => 18,
        _ => 19,
    }
}

#[inline]
fn m_base_from_value(value: i32) -> u8 {
    match value {
        0..=15 => value as u8,
        16..=23 => 16,
        24..=55 => 17,
        56..=311 => 18,
        _ => 19,
    }
}

#[inline]
fn d_base_from_value(value: i32) -> u8 {
    #[rustfmt::skip]
    const SYM: [u8; 256] = [
        0,  1,  2,  3,  4,  4,  5,  5,  6,  6,  7,  7,  8,  8,  8,  8,  9,  9,
        9,  9,  10, 10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12,
        13, 13, 13, 13, 13, 13, 13, 13, 14, 14, 14, 14, 14, 14, 14, 14, 15, 15,
        15, 15, 15, 15, 15, 15, 16, 16, 16, 16, 16, 17, 18, 19, 20, 20, 21, 21,
        22, 22, 23, 23, 24, 24, 24, 24, 25, 25, 25, 25, 26, 26, 26, 26, 27, 27,
        27, 27, 28, 28, 28, 28, 28, 28, 28, 28, 29, 29, 29, 29, 29, 29, 29, 29,
        30, 30, 30, 30, 30, 30, 30, 30, 31, 31, 31, 31, 31, 31, 31, 31, 32, 32,
        32, 32, 32, 33, 34, 35, 36, 36, 37, 37, 38, 38, 39, 39, 40, 40, 40, 40,
        41, 41, 41, 41, 42, 42, 42, 42, 43, 43, 43, 43, 44, 44, 44, 44, 44, 44,
        44, 44, 45, 45, 45, 45, 45, 45, 45, 45, 46, 46, 46, 46, 46, 46, 46, 46,
        47, 47, 47, 47, 47, 47, 47, 47, 48, 48, 48, 48, 48, 49, 50, 51, 52, 52,
        53, 53, 54, 54, 55, 55, 56, 56, 56, 56, 57, 57, 57, 57, 58, 58, 58, 58,
        59, 59, 59, 59, 60, 60, 60, 60, 60, 60, 60, 60, 61, 61, 61, 61, 61, 61,
        61, 61, 62, 62, 62, 62, 62, 62, 62, 62, 63, 63, 63, 63, 63, 63, 63, 63,
        0,  0,  0,  0,
    ];
    let mut index = 0i32;
    let mut in_range = (0..60).contains(&value) as i32;
    index |= ((value >> 0) + 0) & -in_range;
    in_range = (60..1020).contains(&value) as i32;
    index |= (((value - 60) >> 4) + 64) & -in_range;
    in_range = (1020..16380).contains(&value) as i32;
    index |= (((value - 1020) >> 8) + 128) & -in_range;
    in_range = (16380..262140).contains(&value) as i32;
    index |= (((value - 16380) >> 12) + 192) & -in_range;
    SYM[(index & 255) as usize]
}

/// Fixed Huffman code for freq table entries; returns (bits, nbits).
#[inline]
fn encode_v1_freq_value(value: i32) -> (u32, i32) {
    match value {
        0 => (0, 2),
        1 => (2, 2),
        2 => (1, 3),
        3 => (5, 3),
        4 => (3, 5),
        5 => (11, 5),
        6 => (19, 5),
        7 => (27, 5),
        8..=23 => (7 + (((value - 8) as u32) << 4), 8),
        _ => ((((value - 24) as u32) << 4) + 15, 14), // 24..1047
    }
}

#[inline]
fn set_field(v: u32, offset: u32) -> u64 {
    (v as u64) << offset
}

#[derive(Clone, Copy)]
struct HistoryLine {
    pos: [i32; LZFSE_ENCODE_HASH_WIDTH],
    value: [u32; LZFSE_ENCODE_HASH_WIDTH],
}

#[derive(Clone, Copy, Default)]
struct LzMatch {
    pos: i64,
    r: i64, // "ref" in the reference: earlier location with the same content
    length: u32,
}

struct Encoder<'a> {
    src: &'a [u8],
    /// translate() offset: real index into `src` = src_off + relative offset.
    src_off: usize,
    src_end: i64,
    src_literal: i64,
    src_encode_i: i64,
    src_encode_end: i64,
    dst: Vec<u8>,
    dst_pos: usize,
    dst_end: usize,
    pending: LzMatch,
    n_matches: u32,
    n_literals: u32,
    l_values: Vec<u32>,
    m_values: Vec<u32>,
    d_values: Vec<u32>,
    literals: Vec<u8>,
    history: Vec<HistoryLine>,
}

impl<'a> Encoder<'a> {
    fn new(src: &'a [u8], dst_size: usize) -> Self {
        Encoder {
            src,
            src_off: 0,
            src_end: 0,
            src_literal: 0,
            src_encode_i: 0,
            src_encode_end: 0,
            // +8 slack: fse_out_finish always stores 8 bytes; the reference
            // relies on the caller's buffer for this. Checks still use dst_end.
            dst: vec![0u8; dst_size + 8],
            dst_pos: 0,
            dst_end: dst_size,
            pending: LzMatch::default(),
            n_matches: 0,
            n_literals: 0,
            l_values: vec![0; LZFSE_MATCHES_PER_BLOCK],
            m_values: vec![0; LZFSE_MATCHES_PER_BLOCK],
            d_values: vec![0; LZFSE_MATCHES_PER_BLOCK],
            literals: vec![0; LZFSE_LITERALS_PER_BLOCK],
            history: vec![
                HistoryLine { pos: [INVALID_POS; 4], value: [0; 4] };
                LZFSE_ENCODE_HASH_VALUES
            ],
        }
    }

    #[inline]
    fn src_byte(&self, off: i64) -> u8 {
        self.src[(self.src_off as i64 + off) as usize]
    }

    #[inline]
    fn src_load4(&self, off: i64) -> u32 {
        load4(self.src, (self.src_off as i64 + off) as usize)
    }

    #[inline]
    fn src_load8(&self, off: i64) -> u64 {
        load8(self.src, (self.src_off as i64 + off) as usize)
    }

    /// lzfse_encode_translate: move the window forward by `delta`.
    fn translate(&mut self, delta: i64) {
        debug_assert!(delta >= 0);
        if delta == 0 {
            return;
        }
        self.src_off += delta as usize;
        self.src_end -= delta;
        self.src_encode_i -= delta;
        self.src_encode_end -= delta;
        self.src_literal -= delta;
        self.pending.pos -= delta;
        self.pending.r -= delta;
        for line in self.history.iter_mut() {
            for p in line.pos.iter_mut() {
                let new_pos = *p as i64 - delta;
                *p = if new_pos < INVALID_POS as i64 { INVALID_POS } else { new_pos as i32 };
            }
        }
    }

    /// Commit one L,M,D part. Capacity checks are the caller's job
    /// (push_match), matching Apple's inlined structure.
    fn push_lmd(&mut self, l: u32, m: u32, d: u32) {
        let n = self.n_matches as usize;
        self.n_matches += 1;
        self.l_values[n] = l;
        self.m_values[n] = m;
        self.d_values[n] = d;
        let lit_start = self.n_literals as usize;
        let src_start = (self.src_off as i64 + self.src_literal) as usize;
        self.literals[lit_start..lit_start + l as usize]
            .copy_from_slice(&self.src[src_start..src_start + l as usize]);
        self.n_literals += l;
        self.src_literal += (l + m) as i64;
    }

    /// Can another match fit? (Both split parts and whole pushes use this.)
    #[inline]
    fn matches_have_room(&self) -> bool {
        self.n_matches as usize + 1 + 8 <= LZFSE_MATCHES_PER_BLOCK
    }

    /// lzfsePushMatch (Apple libcompression variant): split the match into
    /// encodable L,M,D parts and push them, reverting everything if any part
    /// does not fit. Unlike the open-source lzfse_push_match, the
    /// literal-capacity check keeps a RUNNING accumulator that re-adds the
    /// 16-byte safety margin for every part, so it drifts 16 bytes per part
    /// above the true literal count - observable in where block boundaries
    /// land.
    fn push_match(&mut self, m: &LzMatch) -> bool {
        let n_matches0 = self.n_matches;
        let n_literals0 = self.n_literals;
        let src_literal0 = self.src_literal;

        let mut l = (m.pos - self.src_literal) as u32;
        let mut mm = m.length;
        let d = (m.pos - m.r) as u32;
        let mut cap = self.n_literals; // running literal-cap accumulator

        let mut ok = true;
        while ok && l > LZFSE_ENCODE_MAX_L_VALUE {
            cap += LZFSE_ENCODE_MAX_L_VALUE + 16;
            if !self.matches_have_room() || cap as usize > LZFSE_LITERALS_PER_BLOCK {
                ok = false;
            } else {
                // D=1 because it's the most frequent, but M=0 means it is unused
                self.push_lmd(LZFSE_ENCODE_MAX_L_VALUE, 0, 1);
                l -= LZFSE_ENCODE_MAX_L_VALUE;
            }
        }
        while ok && mm > LZFSE_ENCODE_MAX_M_VALUE {
            cap += l + 16;
            if !self.matches_have_room() || cap as usize > LZFSE_LITERALS_PER_BLOCK {
                ok = false;
            } else {
                self.push_lmd(l, LZFSE_ENCODE_MAX_M_VALUE, d);
                l = 0;
                mm -= LZFSE_ENCODE_MAX_M_VALUE;
            }
        }
        if ok && (l > 0 || mm > 0) {
            cap += l + 16;
            if !self.matches_have_room() || cap as usize > LZFSE_LITERALS_PER_BLOCK {
                ok = false;
            } else {
                self.push_lmd(l, mm, d);
            }
        }
        if !ok {
            self.n_matches = n_matches0;
            self.n_literals = n_literals0;
            self.src_literal = src_literal0;
            return false;
        }
        true
    }

    fn backend_match(&mut self, m: &LzMatch) -> bool {
        if self.push_match(m) {
            return true;
        }
        if !self.encode_matches() {
            return false;
        }
        self.push_match(m)
    }

    fn backend_literals(&mut self, l: i64) -> bool {
        let pos = self.src_literal + l;
        let m = LzMatch { pos, r: pos - 1, length: 0 };
        self.backend_match(&m)
    }

    fn backend_end_of_stream(&mut self) -> bool {
        if !self.encode_matches() {
            return false;
        }
        if self.dst_pos + 4 > self.dst_end {
            return false;
        }
        self.dst[self.dst_pos..self.dst_pos + 4]
            .copy_from_slice(&LZFSE_ENDOFSTREAM_BLOCK_MAGIC.to_le_bytes());
        self.dst_pos += 4;
        true
    }

    /// lzfse_encode_matches: emit the accumulated block. On failure (dst
    /// full) the state is reverted exactly as in the reference.
    fn encode_matches(&mut self) -> bool {
        if self.n_literals == 0 && self.n_matches == 0 {
            return true;
        }
        let dst0 = self.dst_pos;
        let n_literals0 = self.n_literals;
        if self.encode_matches_inner() {
            return true;
        }
        // Revert the d_prev encoding
        let mut d_prev = 0u32;
        for i in 0..self.n_matches as usize {
            let d = self.d_values[i];
            if d == 0 {
                self.d_values[i] = d_prev;
            } else {
                d_prev = d;
            }
        }
        self.n_literals = n_literals0;
        self.dst_pos = dst0;
        false
    }

    fn encode_matches_inner(&mut self) -> bool {
        // Pad with 0x00 literals to a multiple of 4 (four interleaved streams)
        while self.n_literals & 3 != 0 {
            let n = self.n_literals as usize;
            self.literals[n] = 0;
            self.n_literals += 1;
        }

        // Encode previous distance
        let mut d_prev = 0u32;
        for i in 0..self.n_matches as usize {
            let d = self.d_values[i];
            if d == d_prev {
                self.d_values[i] = 0;
            } else {
                d_prev = d;
            }
        }

        // Occurrence tables for all 4 streams
        let mut l_occ = [0u32; LZFSE_ENCODE_L_SYMBOLS];
        let mut m_occ = [0u32; LZFSE_ENCODE_M_SYMBOLS];
        let mut d_occ = [0u32; LZFSE_ENCODE_D_SYMBOLS];
        let mut literal_occ = [0u32; LZFSE_ENCODE_LITERAL_SYMBOLS];
        let mut l_sum = 0u32;
        let mut m_sum = 0u32;
        for i in 0..self.n_matches as usize {
            let l = self.l_values[i];
            l_sum = l_sum.wrapping_add(l);
            l_occ[l_base_from_value(l as i32) as usize] += 1;
        }
        for i in 0..self.n_matches as usize {
            let m = self.m_values[i];
            m_sum = m_sum.wrapping_add(m);
            m_occ[m_base_from_value(m as i32) as usize] += 1;
        }
        for i in 0..self.n_matches as usize {
            d_occ[d_base_from_value(self.d_values[i] as i32) as usize] += 1;
        }
        for i in 0..self.n_literals as usize {
            literal_occ[self.literals[i] as usize] += 1;
        }

        // Room for a full V2 header?
        if self.dst_pos + V2_HEADER_FULL_SIZE > self.dst_end {
            return false;
        }
        let header_base = self.dst_pos;
        let n_raw_bytes = m_sum.wrapping_add(l_sum);
        let header_n_matches = self.n_matches;
        let header_n_literals = self.n_literals;

        // Normalize occurrence tables to freq tables
        let mut l_freq = [0u16; LZFSE_ENCODE_L_SYMBOLS];
        let mut m_freq = [0u16; LZFSE_ENCODE_M_SYMBOLS];
        let mut d_freq = [0u16; LZFSE_ENCODE_D_SYMBOLS];
        let mut literal_freq = [0u16; LZFSE_ENCODE_LITERAL_SYMBOLS];
        fse_normalize_freq(LZFSE_ENCODE_L_STATES, &l_occ, &mut l_freq);
        fse_normalize_freq(LZFSE_ENCODE_M_STATES, &m_occ, &mut m_freq);
        fse_normalize_freq(LZFSE_ENCODE_D_STATES, &d_occ, &mut d_freq);
        fse_normalize_freq(LZFSE_ENCODE_LITERAL_STATES, &literal_occ, &mut literal_freq);

        // Compress freq tables into the V2 header
        let header_size = {
            let mut accum = 0u32;
            let mut accum_nbits = 0i32;
            let mut fdst = header_base + V2_HEADER_FIXED_SIZE;
            for &f in l_freq
                .iter()
                .chain(m_freq.iter())
                .chain(d_freq.iter())
                .chain(literal_freq.iter())
            {
                let (bits, nbits) = encode_v1_freq_value(f as i32);
                accum |= bits << accum_nbits;
                accum_nbits += nbits;
                while accum_nbits >= 8 {
                    self.dst[fdst] = (accum & 0xff) as u8;
                    accum >>= 8;
                    accum_nbits -= 8;
                    fdst += 1;
                }
            }
            if accum_nbits > 0 {
                self.dst[fdst] = (accum & 0xff) as u8;
                fdst += 1;
            }
            fdst - header_base
        };
        self.dst_pos = header_base + header_size;

        // Encoder tables
        let mut l_encoder = [FseEncoderEntry::default(); LZFSE_ENCODE_L_SYMBOLS];
        let mut m_encoder = [FseEncoderEntry::default(); LZFSE_ENCODE_M_SYMBOLS];
        let mut d_encoder = [FseEncoderEntry::default(); LZFSE_ENCODE_D_SYMBOLS];
        let mut literal_encoder = [FseEncoderEntry::default(); LZFSE_ENCODE_LITERAL_SYMBOLS];
        fse_init_encoder_table(LZFSE_ENCODE_L_STATES, &l_freq, &mut l_encoder);
        fse_init_encoder_table(LZFSE_ENCODE_M_STATES, &m_freq, &mut m_encoder);
        fse_init_encoder_table(LZFSE_ENCODE_D_STATES, &d_freq, &mut d_encoder);
        fse_init_encoder_table(LZFSE_ENCODE_LITERAL_STATES, &literal_freq, &mut literal_encoder);

        // Encode literals (backwards, 4 interleaved streams)
        let literal_bits;
        let n_literal_payload_bytes;
        let literal_state;
        {
            let mut out = FseOutStream::new();
            let mut state = [0u16; 4];
            let mut buf = self.dst_pos;
            let mut i = self.n_literals as usize; // multiple of 4
            while i > 0 {
                if buf + 16 > self.dst_end {
                    return false;
                }
                i -= 4;
                fse_encode(&mut state[3], &literal_encoder, &mut out, self.literals[i + 3]);
                fse_encode(&mut state[2], &literal_encoder, &mut out, self.literals[i + 2]);
                fse_encode(&mut state[1], &literal_encoder, &mut out, self.literals[i + 1]);
                fse_encode(&mut state[0], &literal_encoder, &mut out, self.literals[i]);
                out.flush(&mut self.dst, &mut buf);
            }
            out.finish(&mut self.dst, &mut buf);

            literal_bits = out.accum_nbits; // [-7, 0]
            n_literal_payload_bytes = (buf - self.dst_pos) as u32;
            literal_state = state;
            self.dst_pos = buf;
        }

        // Encode L,M,D (backwards)
        let lmd_bits;
        let n_lmd_payload_bytes;
        let (l_state, m_state, d_state);
        {
            let mut out = FseOutStream::new();
            let mut ls = 0u16;
            let mut ms = 0u16;
            let mut ds = 0u16;
            let mut buf = self.dst_pos;

            // 8 padding bytes at the start of the L,M,D payload
            if buf + 8 > self.dst_end {
                return false;
            }
            self.dst[buf..buf + 8].fill(0);
            buf += 8;

            let mut i = self.n_matches as usize;
            while i > 0 {
                if buf + 16 > self.dst_end {
                    return false;
                }
                i -= 1;

                // D requires 23b max
                let d_value = self.d_values[i] as i32;
                let d_symbol = d_base_from_value(d_value);
                let d_nbits = D_EXTRA_BITS[d_symbol as usize] as i32;
                let d_bits = (d_value - D_BASE_VALUE[d_symbol as usize]) as u32;
                out.push(d_nbits, d_bits as u64);
                fse_encode(&mut ds, &d_encoder, &mut out, d_symbol);

                // M requires 17b max
                let m_value = self.m_values[i] as i32;
                let m_symbol = m_base_from_value(m_value);
                let m_nbits = M_EXTRA_BITS[m_symbol as usize] as i32;
                let m_bits = (m_value - M_BASE_VALUE[m_symbol as usize]) as u32;
                out.push(m_nbits, m_bits as u64);
                fse_encode(&mut ms, &m_encoder, &mut out, m_symbol);

                // L requires 14b max
                let l_value = self.l_values[i] as i32;
                let l_symbol = l_base_from_value(l_value);
                let l_nbits = L_EXTRA_BITS[l_symbol as usize] as i32;
                let l_bits = (l_value - L_BASE_VALUE[l_symbol as usize]) as u32;
                out.push(l_nbits, l_bits as u64);
                fse_encode(&mut ls, &l_encoder, &mut out, l_symbol);
                out.flush(&mut self.dst, &mut buf);
            }
            out.finish(&mut self.dst, &mut buf);

            lmd_bits = out.accum_nbits; // [-7, 0]
            n_lmd_payload_bytes = (buf - self.dst_pos) as u32;
            l_state = ls;
            m_state = ms;
            d_state = ds;
            self.dst_pos = buf;
        }

        // Success: consume the block state
        self.n_literals = 0;
        self.n_matches = 0;

        // Pack the V2 header (lzfse_encode_v1_state)
        let v0 = set_field(header_n_literals, 0)
            | set_field(n_literal_payload_bytes, 20)
            | set_field(header_n_matches, 40)
            | set_field((7 + literal_bits) as u32, 60);
        let v1 = set_field(literal_state[0] as u32, 0)
            | set_field(literal_state[1] as u32, 10)
            | set_field(literal_state[2] as u32, 20)
            | set_field(literal_state[3] as u32, 30)
            | set_field(n_lmd_payload_bytes, 40)
            | set_field((7 + lmd_bits) as u32, 60);
        let v2 = set_field(header_size as u32, 0)
            | set_field(l_state as u32, 32)
            | set_field(m_state as u32, 42)
            | set_field(d_state as u32, 52);
        self.dst[header_base..header_base + 4]
            .copy_from_slice(&LZFSE_COMPRESSEDV2_BLOCK_MAGIC.to_le_bytes());
        self.dst[header_base + 4..header_base + 8].copy_from_slice(&n_raw_bytes.to_le_bytes());
        self.dst[header_base + 8..header_base + 16].copy_from_slice(&v0.to_le_bytes());
        self.dst[header_base + 16..header_base + 24].copy_from_slice(&v1.to_le_bytes());
        self.dst[header_base + 24..header_base + 32].copy_from_slice(&v2.to_le_bytes());

        true
    }

    /// lzfse_encode_base: the match-finding front end.
    fn encode_base(&mut self) -> bool {
        // 8 byte padding at end of buffer
        self.src_encode_end = self.src_end - 8;
        while self.src_encode_i < self.src_encode_end {
            let pos = self.src_encode_i;

            // Load 4 byte value and get hash line
            let x = self.src_load4(pos);
            let hash_idx = hash_x(x) as usize;
            let h = self.history[hash_idx];

            // Next hash line (component 0 is the most recent)
            let mut new_h = HistoryLine { pos: [0; 4], value: [0; 4] };
            new_h.pos[0] = pos as i32;
            new_h.pos[1..].copy_from_slice(&h.pos[..LZFSE_ENCODE_HASH_WIDTH - 1]);
            new_h.value[0] = x;
            new_h.value[1..].copy_from_slice(&h.value[..LZFSE_ENCODE_HASH_WIDTH - 1]);

            if !self.process_position(pos, x, &h) {
                return false; // DST full: history line not updated, i not advanced
            }

            self.history[hash_idx] = new_h;
            self.src_encode_i += 1;
        }
        true
    }

    /// Body of the encode_base loop; returns false on DST-full. A `true`
    /// return corresponds to the reference's END_POS label.
    fn process_position(&mut self, pos: i64, x: u32, h: &HistoryLine) -> bool {
        // Do not look for a match if we are still covered by a previous match
        if pos < self.src_literal {
            return true;
        }

        // Search best incoming match (length >= 4 only)
        let mut incoming = LzMatch { pos, r: 0, length: 0 };
        for k in 0..LZFSE_ENCODE_HASH_WIDTH {
            if h.value[k] != x {
                continue; // no 4 byte match
            }
            let r = h.pos[k] as i64;
            if r + LZFSE_ENCODE_MAX_D_VALUE < pos {
                continue; // too far
            }
            let mut length: u32 = 4;
            let max_length = (self.src_end - pos - 8) as u32;
            while length < max_length {
                let d = self.src_load8(r + length as i64) ^ self.src_load8(pos + length as i64);
                if d == 0 {
                    length += 8;
                    continue;
                }
                length += d.trailing_zeros() >> 3;
                break;
            }
            if length > incoming.length {
                incoming.length = length;
                incoming.r = r;
            }
        }

        if incoming.length == 0 {
            // Emit some literals if we lag too far behind the search point
            let n_literals = pos - self.src_literal;
            // Apple's libcompression fires this at a lag of 3*MAX_L (946
            // bytes; `cmp x12, #0x3b2` in lzfseEncodeBase) where the
            // open-source release uses 8*MAX_L.
            if n_literals > 3 * LZFSE_ENCODE_MAX_L_VALUE as i64 {
                if self.pending.length > 0 {
                    let p = self.pending;
                    if !self.backend_match(&p) {
                        return false;
                    }
                    self.pending = LzMatch::default();
                } else if !self.backend_literals(LZFSE_ENCODE_MAX_L_VALUE as i64) {
                    return false;
                }
            }
            return true;
        }

        if incoming.length > LZFSE_ENCODE_MAX_MATCH_LENGTH {
            incoming.length = LZFSE_ENCODE_MAX_MATCH_LENGTH;
        }

        // Expand backwards (best match only)
        while incoming.pos > self.src_literal
            && incoming.r > 0
            && self.src_byte(incoming.r - 1) == self.src_byte(incoming.pos - 1)
        {
            incoming.pos -= 1;
            incoming.r -= 1;
        }
        incoming.length += (pos - incoming.pos) as u32;

        // Match filtering heuristic (from LZVN)

        // Incoming is 'good', emit incoming
        if incoming.length >= LZFSE_ENCODE_GOOD_MATCH {
            if !self.backend_match(&incoming) {
                return false;
            }
            self.pending = LzMatch::default();
            return true;
        }

        // No pending, keep incoming
        if self.pending.length == 0 {
            self.pending = incoming;
            return true;
        }

        // No overlap: emit pending, keep incoming
        if self.pending.pos + self.pending.length as i64 <= incoming.pos {
            let p = self.pending;
            if !self.backend_match(&p) {
                return false;
            }
            self.pending = incoming;
            return true;
        }

        // Overlap: emit longest
        let emit = if incoming.length > self.pending.length { incoming } else { self.pending };
        if !self.backend_match(&emit) {
            return false;
        }
        self.pending = LzMatch::default();
        true
    }

    fn encode_finish(&mut self) -> bool {
        if self.pending.length > 0 {
            let p = self.pending;
            if !self.backend_match(&p) {
                return false;
            }
            self.pending = LzMatch::default();
        }
        let l = self.src_end - self.src_literal;
        if l > 0 && !self.backend_literals(l) {
            return false;
        }
        self.backend_end_of_stream()
    }
}

fn encode_lzfse(src: &[u8], dst_size: usize) -> Option<Vec<u8>> {
    let mut s = Encoder::new(src, dst_size);
    let mut src_size = src.len();

    if src_size >= 0xffffffff {
        // lzfse only uses 32 bits for offsets internally; process very large
        // buffers in translated chunks, exactly like the reference.
        const ENCODER_BLOCK_SIZE: i64 = 262144;
        s.src_end = ENCODER_BLOCK_SIZE;
        if !s.encode_base() {
            return None;
        }
        src_size -= ENCODER_BLOCK_SIZE as usize;
        while src_size >= ENCODER_BLOCK_SIZE as usize {
            s.src_end = 2 * ENCODER_BLOCK_SIZE;
            if !s.encode_base() {
                return None;
            }
            s.translate(ENCODER_BLOCK_SIZE);
            src_size -= ENCODER_BLOCK_SIZE as usize;
        }
        s.src_end = ENCODER_BLOCK_SIZE + src_size as i64;
    } else {
        s.src_end = src_size as i64;
    }
    if !s.encode_base() {
        return None;
    }
    if !s.encode_finish() {
        return None;
    }
    let written = s.dst_pos;
    let mut out = s.dst;
    out.truncate(written);
    Some(out)
}

fn try_uncompressed(src: &[u8], dst_size: usize) -> Option<Vec<u8>> {
    if src.len() + 12 <= dst_size && src.len() < i32::MAX as usize {
        let mut out = Vec::with_capacity(src.len() + 12);
        out.extend_from_slice(&LZFSE_UNCOMPRESSED_BLOCK_MAGIC.to_le_bytes());
        out.extend_from_slice(&(src.len() as u32).to_le_bytes());
        out.extend_from_slice(src);
        out.extend_from_slice(&LZFSE_ENDOFSTREAM_BLOCK_MAGIC.to_le_bytes());
        return Some(out);
    }
    None
}

/// Port of `lzfse_encode_buffer`: encode `src` into an LZFSE container,
/// given a destination budget of `dst_size` bytes. Returns None when the
/// reference would return 0 (nothing fits).
pub fn encode_buffer(src: &[u8], dst_size: usize) -> Option<Vec<u8>> {
    // Really small input: uncompressed block (LZVN would refuse it)
    if src.len() >= LZVN_ENCODE_MIN_SRC_SIZE && src.len() < LZFSE_ENCODE_LZVN_THRESHOLD {
        // Small input: LZVN block in container
        let extra_size = 4 + 12; // end-of-stream marker + lzvn block header
        if dst_size > extra_size {
            let payload = crate::lzvn::encode(src);
            let sz = payload.len();
            if sz != 0 && sz < src.len() && sz <= dst_size - extra_size {
                let mut out = Vec::with_capacity(sz + extra_size);
                out.extend_from_slice(&LZFSE_COMPRESSEDLZVN_BLOCK_MAGIC.to_le_bytes());
                out.extend_from_slice(&(src.len() as u32).to_le_bytes());
                out.extend_from_slice(&(sz as u32).to_le_bytes());
                out.extend_from_slice(&payload);
                out.extend_from_slice(&LZFSE_ENDOFSTREAM_BLOCK_MAGIC.to_le_bytes());
                return Some(out);
            }
        }
        return try_uncompressed(src, dst_size);
    }
    if src.len() >= LZFSE_ENCODE_LZVN_THRESHOLD {
        if let Some(out) = encode_lzfse(src, dst_size) {
            return Some(out);
        }
    }
    try_uncompressed(src, dst_size)
}
