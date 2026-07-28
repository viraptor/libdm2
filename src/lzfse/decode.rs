//! LZFSE decoder — port of lzfse_decode_base.c / lzfse_decode.c, including
//! the embedded lzvn block decoder (lzvn_decode_base.c) used for bvxn
//! blocks. Accepts exactly the streams `lzfse_decode_buffer` accepts, with
//! identical output, truncation and error behavior.

use super::fse::*;
use super::*;

#[derive(PartialEq)]
enum Status {
    Ok,
    SrcEmpty,
    DstFull,
    Error,
}

// ------------------------------------------------------------------
//  Headers
// ------------------------------------------------------------------

struct HeaderV1 {
    /// Present in the stream but unused by the block decoder (the reference
    /// also ignores it: output length is implied by the L,M,D triplets).
    #[allow(dead_code)]
    n_raw_bytes: u32,
    n_literals: u32,
    n_matches: u32,
    n_literal_payload_bytes: u32,
    n_lmd_payload_bytes: u32,
    literal_bits: i32,
    literal_state: [u16; 4],
    lmd_bits: i32,
    l_state: u16,
    m_state: u16,
    d_state: u16,
    l_freq: [u16; LZFSE_ENCODE_L_SYMBOLS],
    m_freq: [u16; LZFSE_ENCODE_M_SYMBOLS],
    d_freq: [u16; LZFSE_ENCODE_D_SYMBOLS],
    literal_freq: [u16; LZFSE_ENCODE_LITERAL_SYMBOLS],
}

impl HeaderV1 {
    fn zeroed(n_raw_bytes: u32) -> Self {
        HeaderV1 {
            n_raw_bytes,
            n_literals: 0,
            n_matches: 0,
            n_literal_payload_bytes: 0,
            n_lmd_payload_bytes: 0,
            literal_bits: 0,
            literal_state: [0; 4],
            lmd_bits: 0,
            l_state: 0,
            m_state: 0,
            d_state: 0,
            l_freq: [0; LZFSE_ENCODE_L_SYMBOLS],
            m_freq: [0; LZFSE_ENCODE_M_SYMBOLS],
            d_freq: [0; LZFSE_ENCODE_D_SYMBOLS],
            literal_freq: [0; LZFSE_ENCODE_LITERAL_SYMBOLS],
        }
    }

    /// lzfse_check_block_header_v1 (magic is checked by the caller's dispatch).
    fn check(&self) -> bool {
        // `literal_bits`/`lmd_bits` are a raw i32 in a v1 header, so a hostile
        // stream can set them to anything; the encoder only ever emits [-7, 0]
        // (the v2 header can't express anything else — it stores bits+7 in
        // three bits). Reject the rest here: `FseInStream::init` adds 64 to
        // this value, which overflows i32 near i32::MAX. Apple's C relies on
        // signed wraparound and a later range test; in Rust that add is a
        // panic under overflow checks.
        (-7..=0).contains(&self.literal_bits)
            && (-7..=0).contains(&self.lmd_bits)
            && self.n_literals as usize <= LZFSE_LITERALS_PER_BLOCK
            && self.n_matches as usize <= LZFSE_MATCHES_PER_BLOCK
            && self.literal_state.iter().all(|&s| (s as i32) < LZFSE_ENCODE_LITERAL_STATES)
            && (self.l_state as i32) < LZFSE_ENCODE_L_STATES
            && (self.m_state as i32) < LZFSE_ENCODE_M_STATES
            && (self.d_state as i32) < LZFSE_ENCODE_D_STATES
            && fse_check_freq(&self.l_freq, LZFSE_ENCODE_L_STATES as usize)
            && fse_check_freq(&self.m_freq, LZFSE_ENCODE_M_STATES as usize)
            && fse_check_freq(&self.d_freq, LZFSE_ENCODE_D_STATES as usize)
            && fse_check_freq(&self.literal_freq, LZFSE_ENCODE_LITERAL_STATES as usize)
    }
}

#[inline]
fn get_field(v: u64, offset: i32, nbits: i32) -> u32 {
    if nbits == 32 {
        (v >> offset) as u32
    } else {
        ((v >> offset) & ((1u64 << nbits) - 1)) as u32
    }
}

/// Decode one freq-table value from the low bits of `bits`; returns
/// (value, nbits consumed).
#[inline]
fn decode_v1_freq_value(bits: u32) -> (u16, i32) {
    #[rustfmt::skip]
    const FREQ_NBITS_TABLE: [i32; 32] = [
        2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14,
        2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14,
    ];
    #[rustfmt::skip]
    const FREQ_VALUE_TABLE: [u16; 32] = [
        0, 2, 1, 4, 0, 3, 1, 0xffff, 0, 2, 1, 5, 0, 3, 1, 0xffff,
        0, 2, 1, 6, 0, 3, 1, 0xffff, 0, 2, 1, 7, 0, 3, 1, 0xffff,
    ];
    let b = (bits & 31) as usize;
    let n = FREQ_NBITS_TABLE[b];
    if n == 8 {
        return ((8 + ((bits >> 4) & 0xf)) as u16, n);
    }
    if n == 14 {
        return ((24 + ((bits >> 4) & 0x3ff)) as u16, n);
    }
    (FREQ_VALUE_TABLE[b], n)
}

/// lzfse_decode_v1: unpack a v2 header (at `src[base..base+header_size]`).
fn decode_v1(src: &[u8], base: usize, header_size: usize) -> Option<HeaderV1> {
    let v0 = load8(src, base + 8);
    let v1 = load8(src, base + 16);
    let v2 = load8(src, base + 24);

    let mut out = HeaderV1::zeroed(load4(src, base + 4));
    out.n_literals = get_field(v0, 0, 20);
    out.n_literal_payload_bytes = get_field(v0, 20, 20);
    out.literal_bits = get_field(v0, 60, 3) as i32 - 7;
    out.literal_state = [
        get_field(v1, 0, 10) as u16,
        get_field(v1, 10, 10) as u16,
        get_field(v1, 20, 10) as u16,
        get_field(v1, 30, 10) as u16,
    ];
    out.n_matches = get_field(v0, 40, 20);
    out.n_lmd_payload_bytes = get_field(v1, 40, 20);
    out.lmd_bits = get_field(v1, 60, 3) as i32 - 7;
    out.l_state = get_field(v2, 32, 10) as u16;
    out.m_state = get_field(v2, 42, 10) as u16;
    out.d_state = get_field(v2, 52, 10) as u16;

    // Freq tables: fixed Huffman coded, read LSB-first from the header tail
    let mut fsrc = base + V2_HEADER_FIXED_SIZE;
    let fsrc_end = base + header_size; // first byte after header
    if fsrc_end == fsrc {
        return Some(out); // freq tables omitted
    }

    let mut accum = 0u32;
    let mut accum_nbits = 0i32;
    let total = LZFSE_ENCODE_L_SYMBOLS
        + LZFSE_ENCODE_M_SYMBOLS
        + LZFSE_ENCODE_D_SYMBOLS
        + LZFSE_ENCODE_LITERAL_SYMBOLS;
    for i in 0..total {
        // Refill accum, one byte at a time
        while fsrc < fsrc_end && accum_nbits + 8 <= 32 {
            accum |= (src[fsrc] as u32) << accum_nbits;
            accum_nbits += 8;
            fsrc += 1;
        }
        let (value, nbits) = decode_v1_freq_value(accum);
        if nbits > accum_nbits {
            return None;
        }
        match i {
            0..=19 => out.l_freq[i] = value,
            20..=39 => out.m_freq[i - 20] = value,
            40..=103 => out.d_freq[i - 40] = value,
            _ => out.literal_freq[i - 104] = value,
        }
        accum >>= nbits;
        accum_nbits -= nbits;
    }
    // We must end at the header boundary with less than 8 bits left over
    if accum_nbits >= 8 || fsrc != fsrc_end {
        return None;
    }
    Some(out)
}

/// Read a raw (uncompressed-tables) v1 header from the stream.
fn read_v1(src: &[u8], base: usize) -> HeaderV1 {
    let rd16 = |off: usize| u16::from_le_bytes(src[base + off..base + off + 2].try_into().unwrap());
    let mut out = HeaderV1::zeroed(load4(src, base + 4));
    out.n_literals = load4(src, base + 12);
    out.n_matches = load4(src, base + 16);
    out.n_literal_payload_bytes = load4(src, base + 20);
    out.n_lmd_payload_bytes = load4(src, base + 24);
    out.literal_bits = load4(src, base + 28) as i32;
    out.literal_state = [rd16(32), rd16(34), rd16(36), rd16(38)];
    out.lmd_bits = load4(src, base + 40) as i32;
    out.l_state = rd16(44);
    out.m_state = rd16(46);
    out.d_state = rd16(48);
    for i in 0..LZFSE_ENCODE_L_SYMBOLS {
        out.l_freq[i] = rd16(50 + 2 * i);
    }
    for i in 0..LZFSE_ENCODE_M_SYMBOLS {
        out.m_freq[i] = rd16(90 + 2 * i);
    }
    for i in 0..LZFSE_ENCODE_D_SYMBOLS {
        out.d_freq[i] = rd16(130 + 2 * i);
    }
    for i in 0..LZFSE_ENCODE_LITERAL_SYMBOLS {
        out.literal_freq[i] = rd16(258 + 2 * i);
    }
    out
}

// ------------------------------------------------------------------
//  LZFSE compressed block state
// ------------------------------------------------------------------

struct LzfseBlockState {
    n_matches: u32,
    n_lmd_payload_bytes: u32,
    current_literal: usize,
    l_value: i32,
    m_value: i32,
    d_value: i32,
    lmd_in_stream: FseInStream,
    lmd_in_buf: u32,
    l_state: u16,
    m_state: u16,
    d_state: u16,
    l_decoder: Vec<FseValueDecoderEntry>,
    m_decoder: Vec<FseValueDecoderEntry>,
    d_decoder: Vec<FseValueDecoderEntry>,
    literal_decoder: Vec<i32>,
    literals: Vec<u8>,
}

impl LzfseBlockState {
    fn new() -> Self {
        LzfseBlockState {
            n_matches: 0,
            n_lmd_payload_bytes: 0,
            current_literal: 0,
            l_value: 0,
            m_value: 0,
            d_value: 0,
            lmd_in_stream: FseInStream::default(),
            lmd_in_buf: 0,
            l_state: 0,
            m_state: 0,
            d_state: 0,
            l_decoder: vec![FseValueDecoderEntry::default(); LZFSE_ENCODE_L_STATES as usize],
            m_decoder: vec![FseValueDecoderEntry::default(); LZFSE_ENCODE_M_STATES as usize],
            d_decoder: vec![FseValueDecoderEntry::default(); LZFSE_ENCODE_D_STATES as usize],
            literal_decoder: vec![0; LZFSE_ENCODE_LITERAL_STATES as usize],
            literals: vec![0; LZFSE_LITERALS_PER_BLOCK + 64],
        }
    }
}

/// The reference's `copy()` helper: an 8-byte do-while copy that always
/// stores at least 8 bytes and rounds `length` up to the next chunk. The
/// slop is intentional and must be preserved (it can surface in the output
/// buffer when a later block stops with DST-full). Reads that would run past
/// the literals buffer are zero-padded — in C those bytes are indeterminate
/// (out-of-bounds struct memory), so no exact content can be matched there.
fn copy_widely(dst: &mut [u8], dst_pos: usize, src: &[u8], src_pos: usize, length: usize) {
    let mut i = 0usize;
    loop {
        let mut b = [0u8; 8];
        let avail = src.len().saturating_sub(src_pos + i).min(8);
        b[..avail].copy_from_slice(&src[src_pos + i..src_pos + i + avail]);
        dst[dst_pos + i..dst_pos + i + 8].copy_from_slice(&b);
        i += 8;
        if i >= length {
            break;
        }
    }
}

/// lzfse_decode_lmd: execute the L,M,D triplets of the current block.
/// `sp` is the position of the LMD payload in `src` (the reference's s->src).
fn decode_lmd(
    src: &[u8],
    sp: usize,
    dst: &mut [u8],
    dp: &mut usize,
    bs: &mut LzfseBlockState,
) -> Status {
    let mut l_state = bs.l_state;
    let mut m_state = bs.m_state;
    let mut d_state = bs.d_state;
    let mut input = bs.lmd_in_stream;
    let src_start = 0usize; // s->src_begin
    let mut lmd_pos = sp + bs.lmd_in_buf as usize;
    let mut lit = bs.current_literal;
    let mut d = *dp;
    let mut symbols = bs.n_matches;
    let mut l = bs.l_value;
    let mut m = bs.m_value;
    let mut dd = bs.d_value;

    // Signed; may go negative near the end of the buffer.
    let mut remaining_bytes = dst.len() as i64 - d as i64 - 32;

    // A pending partially-executed triplet means we resume mid-symbol.
    let mut execute_pending = l != 0 || m != 0;

    while execute_pending || symbols > 0 {
        if !execute_pending {
            // Decode the next L, M, D symbol from the input stream
            if input.flush(&mut lmd_pos, src_start, src).is_err() {
                return Status::Error;
            }
            l = fse_value_decode(&mut l_state, &bs.l_decoder, &mut input);
            if lit + l as usize >= LZFSE_LITERALS_PER_BLOCK + 64 {
                return Status::Error;
            }
            m = fse_value_decode(&mut m_state, &bs.m_decoder, &mut input);
            let new_d = fse_value_decode(&mut d_state, &bs.d_decoder, &mut input);
            dd = if new_d != 0 { new_d } else { dd };
            symbols -= 1;
        }
        execute_pending = false;

        // ExecuteMatch: error if D is out of range
        if (dd as u32) as i64 > d as i64 + l as i64 {
            return Status::Error;
        }

        if (l + m) as i64 <= remaining_bytes {
            // Fast path: plenty of space remaining. The wide do-while copies
            // (always at least one 8-byte store, rounding the length up)
            // must be replicated exactly: their slop past the logical length
            // is observable if a later block reports DST-full.
            remaining_bytes -= (l + m) as i64;
            copy_widely(dst, d, &bs.literals, lit, l as usize);
            d += l as usize;
            lit += l as usize;
            let ddu = dd as usize;
            if dd >= 8 || dd >= m {
                let mut i = 0usize;
                loop {
                    let v = load8(dst, d + i - ddu);
                    dst[d + i..d + i + 8].copy_from_slice(&v.to_le_bytes());
                    i += 8;
                    if i >= m as usize {
                        break;
                    }
                }
            } else {
                for i in 0..m as usize {
                    dst[d + i] = dst[d + i - ddu];
                }
            }
            d += m as usize;
        } else {
            // Careful path: near the end of the destination buffer
            remaining_bytes += 32;
            if l as i64 <= remaining_bytes {
                for i in 0..l as usize {
                    dst[d + i] = bs.literals[lit + i];
                }
                d += l as usize;
                lit += l as usize;
                remaining_bytes -= l as i64;
                l = 0;
            } else {
                let n = remaining_bytes as usize;
                for i in 0..n {
                    dst[d + i] = bs.literals[lit + i];
                }
                d += n;
                lit += n;
                l -= n as i32;
                // Destination is full
                bs.l_value = l;
                bs.m_value = m;
                bs.d_value = dd;
                bs.l_state = l_state;
                bs.m_state = m_state;
                bs.d_state = d_state;
                bs.lmd_in_stream = input;
                bs.n_matches = symbols;
                bs.lmd_in_buf = lmd_pos.wrapping_sub(sp) as u32;
                bs.current_literal = lit;
                *dp = d;
                return Status::DstFull;
            }
            if m as i64 <= remaining_bytes {
                for i in 0..m as usize {
                    dst[d + i] = dst[d + i - dd as usize];
                }
                d += m as usize;
                remaining_bytes -= m as i64;
                m = 0;
            } else {
                let n = remaining_bytes as usize;
                for i in 0..n {
                    dst[d + i] = dst[d + i - dd as usize];
                }
                d += n;
                m -= n as i32;
                // Destination is full (same save as above)
                bs.l_value = l;
                bs.m_value = m;
                bs.d_value = dd;
                bs.l_state = l_state;
                bs.m_state = m_state;
                bs.d_state = d_state;
                bs.lmd_in_stream = input;
                bs.n_matches = symbols;
                bs.lmd_in_buf = lmd_pos.wrapping_sub(sp) as u32;
                bs.current_literal = lit;
                *dp = d;
                return Status::DstFull;
            }
            remaining_bytes -= 32;
        }
    }
    *dp = d;
    Status::Ok
}

// ------------------------------------------------------------------
//  LZVN block decoder (lzvn_decode_base.c)
// ------------------------------------------------------------------

struct LzvnState {
    sp: usize,     // next byte to read (good position)
    sp_end: usize,
    dp: usize,     // next byte to write (good position)
    dst_begin: usize,
    dp_end: usize,
    l: usize,      // partially expanded match, or 0,0,0
    m: usize,
    d: usize,
    d_prev: usize,
    eos: bool,
}

/// Port of lzvn_decode. Positions in `st` advance only at opcode boundaries
/// ("UPDATE_GOOD"), so on truncation/error the state reflects the last fully
/// executed opcode, exactly like the reference.
fn lzvn_decode_block(st: &mut LzvnState, src: &[u8], dst: &mut [u8]) {
    let mut src_len = st.sp_end - st.sp;
    let mut dst_len = st.dp_end - st.dp;
    if src_len == 0 || dst_len == 0 {
        return; // empty buffer
    }
    let mut sp = st.sp;
    let mut dp = st.dp;
    let mut d = st.d_prev;

    // The reference uses guarded wide copies whose slop past the logical
    // length is observable (a later truncated opcode rolls the good position
    // back but the writes stay); each fast path below replicates the exact
    // write pattern.

    // Literal following a match opcode (L is 0-3): 4-byte wide copy.
    macro_rules! do_copy_literal_and_match {
        ($l:expr, $m:expr) => {{
            let l: usize = $l;
            if dst_len >= 4 && src_len >= 4 {
                let v = load4(src, sp);
                dst[dp..dp + 4].copy_from_slice(&v.to_le_bytes());
                dp += l;
                dst_len -= l;
                sp += l;
                src_len -= l;
                true
            } else if l <= dst_len {
                for i in 0..l {
                    dst[dp + i] = src[sp + i];
                }
                dp += l;
                dst_len -= l;
                sp += l;
                src_len -= l;
                true
            } else {
                // Destination truncated: fill DST, save partial state
                for i in 0..dst_len {
                    dst[dp + i] = src[sp + i];
                }
                st.sp = sp + dst_len;
                st.dp = dp + dst_len;
                st.l = l - dst_len;
                st.m = $m;
                st.d = d;
                false
            }
        }};
    }
    // Plain literal (sml_l / lrg_l): 8-byte chunked copy.
    macro_rules! do_copy_literal {
        ($l:expr) => {{
            let l: usize = $l;
            if dst_len >= l + 7 && src_len >= l + 7 {
                let mut i = 0usize;
                while i < l {
                    let v = load8(src, sp + i);
                    dst[dp + i..dp + i + 8].copy_from_slice(&v.to_le_bytes());
                    i += 8;
                }
                dp += l;
                dst_len -= l;
                sp += l;
                src_len -= l;
                true
            } else if l <= dst_len {
                for i in 0..l {
                    dst[dp + i] = src[sp + i];
                }
                dp += l;
                dst_len -= l;
                sp += l;
                src_len -= l;
                true
            } else {
                for i in 0..dst_len {
                    dst[dp + i] = src[sp + i];
                }
                st.sp = sp + dst_len;
                st.dp = dp + dst_len;
                st.l = l - dst_len;
                st.m = 0;
                st.d = d;
                false
            }
        }};
    }
    macro_rules! do_copy_match {
        ($m:expr) => {{
            let m: usize = $m;
            if dst_len >= m + 7 && d >= 8 {
                let mut i = 0usize;
                while i < m {
                    let v = load8(dst, dp + i - d);
                    dst[dp + i..dp + i + 8].copy_from_slice(&v.to_le_bytes());
                    i += 8;
                }
                dp += m;
                dst_len -= m;
                true
            } else if m <= dst_len {
                for i in 0..m {
                    dst[dp + i] = dst[dp + i - d];
                }
                dp += m;
                dst_len -= m;
                true
            } else {
                for i in 0..dst_len {
                    dst[dp + i] = dst[dp + i - d];
                }
                st.sp = sp;
                st.dp = dp + dst_len;
                st.l = 0;
                st.m = m - dst_len;
                st.d = d;
                false
            }
        }};
    }

    // Partially expanded match saved in state?
    if st.l != 0 || st.m != 0 {
        let l = st.l;
        let m = st.m;
        d = st.d;
        st.l = 0;
        st.m = 0;
        st.d = 0;
        if m == 0 {
            // Resume a plain literal (opcode already consumed)
            if src_len <= l {
                return; // source truncated
            }
            if !do_copy_literal!(l) {
                return;
            }
        } else if l == 0 {
            if !do_copy_match!(m) {
                return;
            }
        } else {
            if !do_copy_literal_and_match!(l, m) {
                return;
            }
            if d > dp - st.dst_begin || d == 0 {
                return; // invalid match distance
            }
            if !do_copy_match!(m) {
                return;
            }
        }
    }

    loop {
        let opc = src[sp];
        // Opcode classes, from the reference jump table
        let (l, m): (usize, usize) = match opc {
            // eos
            0x06 => {
                if src_len < 8 {
                    return; // source truncated
                }
                sp += 8;
                st.eos = true;
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                return;
            }
            // nop
            0x0e | 0x16 => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                if src_len <= 1 {
                    return;
                }
                sp += 1;
                src_len -= 1;
                continue;
            }
            // udef
            0x1e | 0x26 | 0x2e | 0x36 | 0x3e | 0x70..=0x7f | 0xd0..=0xdf => return,
            // lrg_l: 11100000 LLLLLLLL LITERAL
            0xe0 => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                if src_len <= 2 {
                    return;
                }
                let l = src[sp + 1] as usize + 16;
                if src_len <= 2 + l {
                    return;
                }
                sp += 2;
                src_len -= 2;
                if !do_copy_literal!(l) {
                    return;
                }
                continue;
            }
            // sml_l: 1110LLLL LITERAL
            0xe1..=0xef => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                let l = (opc & 15) as usize;
                if src_len <= 1 + l {
                    return;
                }
                sp += 1;
                src_len -= 1;
                if !do_copy_literal!(l) {
                    return;
                }
                continue;
            }
            // lrg_m: 11110000 MMMMMMMM
            0xf0 => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                if src_len <= 2 {
                    return;
                }
                let m = src[sp + 1] as usize + 16;
                sp += 2;
                src_len -= 2;
                if !do_copy_match!(m) {
                    return;
                }
                continue;
            }
            // sml_m: 1111MMMM
            0xf1..=0xff => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                if src_len <= 1 {
                    return;
                }
                let m = (opc & 15) as usize;
                sp += 1;
                src_len -= 1;
                if !do_copy_match!(m) {
                    return;
                }
                continue;
            }
            // med_d: 101LLMMM DDDDDDMM DDDDDDDD LITERAL
            0xa0..=0xbf => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                let l = ((opc >> 3) & 3) as usize;
                if src_len <= 3 + l {
                    return;
                }
                let opc23 = u16::from_le_bytes([src[sp + 1], src[sp + 2]]);
                let m = (((opc & 7) as usize) << 2 | (opc23 & 3) as usize) + 3;
                d = (opc23 >> 2) as usize;
                sp += 3;
                src_len -= 3;
                (l, m)
            }
            // pre_d: LLMMM110
            _ if opc & 7 == 6 => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                let l = (opc >> 6) as usize;
                let m = ((opc >> 3) & 7) as usize + 3;
                if src_len <= 1 + l {
                    return;
                }
                sp += 1;
                src_len -= 1;
                (l, m)
            }
            // lrg_d: LLMMM111 DDDDDDDD DDDDDDDD LITERAL
            _ if opc & 7 == 7 => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                let l = (opc >> 6) as usize;
                let m = ((opc >> 3) & 7) as usize + 3;
                if src_len <= 3 + l {
                    return;
                }
                d = u16::from_le_bytes([src[sp + 1], src[sp + 2]]) as usize;
                sp += 3;
                src_len -= 3;
                (l, m)
            }
            // sml_d: LLMMMDDD DDDDDDDD LITERAL
            _ => {
                st.sp = sp;
                st.dp = dp;
                st.d_prev = d;
                let l = (opc >> 6) as usize;
                let m = ((opc >> 3) & 7) as usize + 3;
                if src_len <= 2 + l {
                    return;
                }
                d = ((opc & 7) as usize) << 8 | src[sp + 1] as usize;
                sp += 2;
                src_len -= 2;
                (l, m)
            }
        };
        // copy_literal_and_match
        if !do_copy_literal_and_match!(l, m) {
            return;
        }
        if d > dp - st.dst_begin || d == 0 {
            return; // invalid match distance
        }
        if !do_copy_match!(m) {
            return;
        }
    }
}

// ------------------------------------------------------------------
//  Top-level block loop (lzfse_decode)
// ------------------------------------------------------------------

fn decode_all(src: &[u8], dst: &mut [u8], dp: &mut usize) -> Status {
    let mut sp = 0usize;
    let mut block_magic = 0u32;
    // Block states persist across blocks in a stream (the reference keeps
    // them in one scratch struct, memset once per call).
    let mut bs = LzfseBlockState::new();
    let mut uncompressed_n_raw = 0u32;
    let mut lzvn_n_raw = 0u32;
    let mut lzvn_n_payload = 0u32;
    let mut lzvn_d_prev = 0u32;

    loop {
        match block_magic {
            0 => {
                if sp + 4 > src.len() {
                    return Status::SrcEmpty;
                }
                let magic = load4(src, sp);

                if magic == LZFSE_ENDOFSTREAM_BLOCK_MAGIC {
                    return Status::Ok;
                }

                if magic == LZFSE_UNCOMPRESSED_BLOCK_MAGIC {
                    if sp + 8 > src.len() {
                        return Status::SrcEmpty;
                    }
                    uncompressed_n_raw = load4(src, sp + 4);
                    sp += 8;
                    block_magic = magic;
                    continue;
                }

                if magic == LZFSE_COMPRESSEDLZVN_BLOCK_MAGIC {
                    if sp + 12 > src.len() {
                        return Status::SrcEmpty;
                    }
                    lzvn_n_raw = load4(src, sp + 4);
                    lzvn_n_payload = load4(src, sp + 8);
                    lzvn_d_prev = 0;
                    sp += 12;
                    block_magic = magic;
                    continue;
                }

                if magic == LZFSE_COMPRESSEDV1_BLOCK_MAGIC
                    || magic == LZFSE_COMPRESSEDV2_BLOCK_MAGIC
                {
                    let header1;
                    let header_size;
                    if magic == LZFSE_COMPRESSEDV2_BLOCK_MAGIC {
                        // Fixed part of the structure present?
                        if sp + V2_HEADER_FIXED_SIZE > src.len() {
                            return Status::SrcEmpty;
                        }
                        header_size = get_field(load8(src, sp + 24), 0, 32) as usize;
                        if sp as u64 + header_size as u64 > src.len() as u64 {
                            return Status::SrcEmpty;
                        }
                        header1 = match decode_v1(src, sp, header_size) {
                            Some(h) => h,
                            None => return Status::Error,
                        };
                    } else {
                        if sp + V1_HEADER_SIZE > src.len() {
                            return Status::SrcEmpty;
                        }
                        header1 = read_v1(src, sp);
                        header_size = V1_HEADER_SIZE;
                    }

                    // Require the entire encoded block in SRC
                    if sp as u64
                        + header_size as u64
                        + header1.n_literal_payload_bytes as u64
                        + header1.n_lmd_payload_bytes as u64
                        > src.len() as u64
                    {
                        return Status::SrcEmpty;
                    }

                    if !header1.check() {
                        return Status::Error;
                    }

                    sp += header_size;

                    // Set up the compressed block state
                    bs.n_lmd_payload_bytes = header1.n_lmd_payload_bytes;
                    bs.n_matches = header1.n_matches;
                    let _ = fse_init_decoder_table(
                        LZFSE_ENCODE_LITERAL_STATES,
                        &header1.literal_freq,
                        &mut bs.literal_decoder,
                    );
                    fse_init_value_decoder_table(
                        LZFSE_ENCODE_L_STATES,
                        &header1.l_freq,
                        &L_EXTRA_BITS,
                        &L_BASE_VALUE,
                        &mut bs.l_decoder,
                    );
                    fse_init_value_decoder_table(
                        LZFSE_ENCODE_M_STATES,
                        &header1.m_freq,
                        &M_EXTRA_BITS,
                        &M_BASE_VALUE,
                        &mut bs.m_decoder,
                    );
                    fse_init_value_decoder_table(
                        LZFSE_ENCODE_D_STATES,
                        &header1.d_freq,
                        &D_EXTRA_BITS,
                        &D_BASE_VALUE,
                        &mut bs.d_decoder,
                    );

                    // Decode literals
                    {
                        let mut input = FseInStream::default();
                        sp += header1.n_literal_payload_bytes as usize; // skip literal payload
                        let mut buf = sp; // read bits backwards from the end
                        if input.init(header1.literal_bits, &mut buf, 0, src).is_err() {
                            return Status::Error;
                        }

                        let mut state = header1.literal_state;
                        let mut i = 0u32;
                        while i < header1.n_literals {
                            if input.flush(&mut buf, 0, src).is_err() {
                                return Status::Error;
                            }
                            let ii = i as usize;
                            bs.literals[ii] =
                                fse_decode(&mut state[0], &bs.literal_decoder, &mut input);
                            bs.literals[ii + 1] =
                                fse_decode(&mut state[1], &bs.literal_decoder, &mut input);
                            bs.literals[ii + 2] =
                                fse_decode(&mut state[2], &bs.literal_decoder, &mut input);
                            bs.literals[ii + 3] =
                                fse_decode(&mut state[3], &bs.literal_decoder, &mut input);
                            i += 4;
                        }
                        bs.current_literal = 0;
                    }

                    // Initialize the L,M,D decode stream (SRC stays at the
                    // start of the LMD payload during block decode)
                    {
                        let mut input = FseInStream::default();
                        let mut buf = sp + header1.n_lmd_payload_bytes as usize;
                        if input.init(header1.lmd_bits, &mut buf, sp, src).is_err() {
                            return Status::Error;
                        }
                        bs.l_state = header1.l_state;
                        bs.m_state = header1.m_state;
                        bs.d_state = header1.d_state;
                        bs.lmd_in_buf = (buf - sp) as u32;
                        bs.l_value = 0;
                        bs.m_value = 0;
                        // Illegal D value so an uninitialized "previous" D errors out
                        bs.d_value = -1;
                        bs.lmd_in_stream = input;
                    }

                    block_magic = magic;
                    continue;
                }

                // Invalid magic number
                return Status::Error;
            }

            LZFSE_UNCOMPRESSED_BLOCK_MAGIC => {
                let mut copy_size = uncompressed_n_raw as usize;
                if copy_size == 0 {
                    block_magic = 0;
                    continue;
                }
                if src.len() <= sp {
                    return Status::SrcEmpty;
                }
                copy_size = copy_size.min(src.len() - sp);
                if dst.len() <= *dp {
                    return Status::DstFull;
                }
                copy_size = copy_size.min(dst.len() - *dp);
                dst[*dp..*dp + copy_size].copy_from_slice(&src[sp..sp + copy_size]);
                sp += copy_size;
                *dp += copy_size;
                uncompressed_n_raw -= copy_size as u32;
            }

            LZFSE_COMPRESSEDV1_BLOCK_MAGIC | LZFSE_COMPRESSEDV2_BLOCK_MAGIC => {
                // Require the entire LMD payload in SRC
                if src.len() <= sp || bs.n_lmd_payload_bytes as usize > src.len() - sp {
                    return Status::SrcEmpty;
                }
                let status = decode_lmd(src, sp, dst, dp, &mut bs);
                if status != Status::Ok {
                    return status;
                }
                block_magic = 0;
                sp += bs.n_lmd_payload_bytes as usize;
            }

            LZFSE_COMPRESSEDLZVN_BLOCK_MAGIC => {
                if lzvn_n_payload > 0 && src.len() <= sp {
                    return Status::SrcEmpty;
                }
                let mut dstate = LzvnState {
                    sp,
                    sp_end: src.len().min(sp + lzvn_n_payload as usize),
                    dp: *dp,
                    dst_begin: 0,
                    dp_end: dst.len().min(*dp + lzvn_n_raw as usize),
                    l: 0,
                    m: 0,
                    d: 0,
                    d_prev: lzvn_d_prev as usize,
                    eos: false,
                };
                lzvn_decode_block(&mut dstate, src, dst);

                let src_used = dstate.sp - sp;
                let dst_used = dstate.dp - *dp;
                if src_used > lzvn_n_payload as usize || dst_used > lzvn_n_raw as usize {
                    return Status::Error; // sanity check
                }
                sp = dstate.sp;
                *dp = dstate.dp;
                lzvn_n_payload -= src_used as u32;
                lzvn_n_raw -= dst_used as u32;
                lzvn_d_prev = dstate.d_prev as u32;

                if lzvn_n_payload == 0 && lzvn_n_raw == 0 && dstate.eos {
                    block_magic = 0; // block done
                    continue;
                }
                if lzvn_n_payload == 0 || lzvn_n_raw == 0 || dstate.eos {
                    return Status::Error;
                }
                // Block is not done and state is valid: need more dst space
                return Status::DstFull;
            }

            _ => return Status::Error, // invalid magic
        }
    }
}

/// Port of `lzfse_decode_buffer`: decode an LZFSE container into a buffer of
/// `dst_size` bytes. Returns the decoded prefix; on DST-full the full-length
/// buffer (as the reference reports `dst_size`); None on error (reference
/// returns 0).
pub fn decode_buffer(src: &[u8], dst_size: usize) -> Option<Vec<u8>> {
    let mut dst = vec![0u8; dst_size];
    let mut dp = 0usize;
    match decode_all(src, &mut dst, &mut dp) {
        Status::Ok => {
            dst.truncate(dp);
            Some(dst)
        }
        Status::DstFull => Some(dst),
        Status::SrcEmpty | Status::Error => None,
    }
}
