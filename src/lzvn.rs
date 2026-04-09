/// LZVN encoder/decoder — a lightweight LZ compression format used internally by Apple.
///
/// Ported from the open-source lzfse reference implementation
/// (lzvn_decode_base.c, lzvn_encode_base.c).
///
/// The format uses variable-length opcodes to encode literal runs and
/// back-references (matches). Each opcode's type is determined by bit
/// patterns in the first byte, dispatched through a 256-entry table.

// ------------------------------------------------------------------
//  Decoder
// ------------------------------------------------------------------

fn extract(val: u8, lsb: u8, width: u8) -> u8 {
    (val >> lsb) & ((1 << width) - 1)
}

fn load2(src: &[u8]) -> u16 {
    u16::from_le_bytes([src[0], src[1]])
}

pub fn decode(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut sp = 0usize; // source position
    let mut dp = 0usize; // destination position
    let mut d_prev: usize = 0; // previous match distance

    loop {
        if sp >= src.len() {
            return None;
        }
        // Output buffer full — stop decoding (Apple's decoder does the same)
        if dp >= dst.len() {
            return Some(dp);
        }
        let opc = src[sp];
        let src_rem = src.len() - sp;

        match opcode_type(opc) {
            Op::Eos => return Some(dp),

            Op::Nop => { sp += 1; }

            Op::Undef => return None,

            Op::SmlD => {
                let opc_len = 2;
                let l = extract(opc, 6, 2) as usize;
                let m = extract(opc, 3, 3) as usize + 3;
                if src_rem <= opc_len + l { return None; }
                let d = ((extract(opc, 0, 3) as usize) << 8) | src[sp + 1] as usize;
                sp += opc_len;
                copy_literal(src, &mut sp, dst, &mut dp, l)?;
                copy_match(dst, &mut dp, d, m)?;
                d_prev = d;
            }

            Op::MedD => {
                let opc_len = 3;
                let l = extract(opc, 3, 2) as usize;
                if src_rem <= opc_len + l { return None; }
                let opc23 = load2(&src[sp + 1..]);
                let m = ((extract(opc, 0, 3) as usize) << 2 | (opc23 & 3) as usize) + 3;
                let d = (opc23 >> 2) as usize;
                sp += opc_len;
                copy_literal(src, &mut sp, dst, &mut dp, l)?;
                copy_match(dst, &mut dp, d, m)?;
                d_prev = d;
            }

            Op::LrgD => {
                let opc_len = 3;
                let l = extract(opc, 6, 2) as usize;
                let m = extract(opc, 3, 3) as usize + 3;
                if src_rem <= opc_len + l { return None; }
                let d = load2(&src[sp + 1..]) as usize;
                sp += opc_len;
                copy_literal(src, &mut sp, dst, &mut dp, l)?;
                copy_match(dst, &mut dp, d, m)?;
                d_prev = d;
            }

            Op::PreD => {
                let opc_len = 1;
                let l = extract(opc, 6, 2) as usize;
                let m = extract(opc, 3, 3) as usize + 3;
                if src_rem <= opc_len + l { return None; }
                sp += opc_len;
                copy_literal(src, &mut sp, dst, &mut dp, l)?;
                copy_match(dst, &mut dp, d_prev, m)?;
            }

            Op::SmlM => {
                let m = extract(opc, 0, 4) as usize;
                if src_rem <= 1 { return None; }
                sp += 1;
                copy_match(dst, &mut dp, d_prev, m)?;
            }

            Op::LrgM => {
                if src_rem <= 2 { return None; }
                let m = src[sp + 1] as usize + 16;
                sp += 2;
                copy_match(dst, &mut dp, d_prev, m)?;
            }

            Op::SmlL => {
                let opc_len = 1;
                let l = extract(opc, 0, 4) as usize;
                if src_rem <= opc_len + l { return None; }
                sp += opc_len;
                copy_literal(src, &mut sp, dst, &mut dp, l)?;
            }

            Op::LrgL => {
                if src_rem <= 2 { return None; }
                let l = src[sp + 1] as usize + 16;
                let opc_len = 2;
                if src_rem <= opc_len + l { return None; }
                sp += opc_len;
                copy_literal(src, &mut sp, dst, &mut dp, l)?;
            }
        }
    }
}

fn copy_literal(src: &[u8], sp: &mut usize, dst: &mut [u8], dp: &mut usize, l: usize) -> Option<()> {
    if *sp + l > src.len() { return None; }
    // Truncate to fit output buffer (Apple's decoder stops when output is full)
    let l = l.min(dst.len() - *dp);
    dst[*dp..*dp + l].copy_from_slice(&src[*sp..*sp + l]);
    *sp += l;
    *dp += l;
    Some(())
}

fn copy_match(dst: &mut [u8], dp: &mut usize, d: usize, m: usize) -> Option<()> {
    if d == 0 || d > *dp { return None; }
    // Truncate to fit output buffer
    let m = m.min(dst.len() - *dp);
    // Byte-by-byte to handle overlapping matches (e.g. d=1 → memset)
    for i in 0..m {
        dst[*dp + i] = dst[*dp + i - d];
    }
    *dp += m;
    Some(())
}

#[derive(Debug, Clone, Copy)]
enum Op { SmlD, MedD, LrgD, PreD, SmlM, LrgM, SmlL, LrgL, Eos, Nop, Undef }

/// The 256-entry opcode classification table, matching Apple's reference.
fn opcode_type(opc: u8) -> Op {
    #[rustfmt::skip]
    const TABLE: [u8; 256] = [
        0,0,0,0,0,0,8,3, 0,0,0,0,0,0,9,3, // 0x00
        0,0,0,0,0,0,9,3, 0,0,0,0,0,0,10,3, // 0x10
        0,0,0,0,0,0,10,3, 0,0,0,0,0,0,10,3, // 0x20
        0,0,0,0,0,0,10,3, 0,0,0,0,0,0,10,3, // 0x30
        0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x40
        0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x50
        0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x60
        10,10,10,10,10,10,10,10, 10,10,10,10,10,10,10,10, // 0x70
        0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x80
        0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0x90
        1,1,1,1,1,1,1,1, 1,1,1,1,1,1,1,1, // 0xA0
        1,1,1,1,1,1,1,1, 1,1,1,1,1,1,1,1, // 0xB0
        0,0,0,0,0,0,4,3, 0,0,0,0,0,0,4,3, // 0xC0
        10,10,10,10,10,10,10,10, 10,10,10,10,10,10,10,10, // 0xD0
        7,6,6,6,6,6,6,6, 6,6,6,6,6,6,6,6, // 0xE0
        5,2,2,2,2,2,2,2, 2,2,2,2,2,2,2,2, // 0xF0
    ];
    // 0=SmlD, 1=MedD, 2=SmlM, 3=LrgD, 4=PreD, 5=LrgM, 6=SmlL, 7=LrgL, 8=Eos, 9=Nop, 10=Undef
    match TABLE[opc as usize] {
        0 => Op::SmlD, 1 => Op::MedD, 2 => Op::SmlM, 3 => Op::LrgD,
        4 => Op::PreD, 5 => Op::LrgM, 6 => Op::SmlL, 7 => Op::LrgL,
        8 => Op::Eos,  9 => Op::Nop,  _ => Op::Undef,
    }
}

// ------------------------------------------------------------------
//  Encoder
// ------------------------------------------------------------------

const HASH_BITS: usize = 14;
const HASH_VALUES: usize = 1 << HASH_BITS;
const OFFSETS_PER_HASH: usize = 4;
const MAX_DISTANCE: usize = 0xFFFF;
const MAX_LITERAL_BACKLOG: usize = 400;

struct MatchInfo {
    pos: usize,
    len: usize,
    dist: usize,
    gain: isize, // len - cost_of_encoding_distance
}

pub fn encode(src: &[u8]) -> Vec<u8> {
    if src.len() < 8 {
        return encode_tiny(src);
    }

    // Generous output buffer; LZVN can expand data
    let mut dst = Vec::with_capacity(src.len() + src.len() / 8 + 64);
    let mut table = vec![[0i32; OFFSETS_PER_HASH]; HASH_VALUES];

    let mut i = 0usize; // current source position
    let end = if src.len() >= 8 { src.len() - 8 } else { 0 };
    let mut literal_start = 0usize;
    let mut d_prev: usize = 0;
    let mut pending: Option<MatchInfo> = None;

    while i < end {
        // Invariant: `end <= src.len() - 8`, so `i + 4 <= src.len()`.
        debug_assert!(i + 4 <= src.len());
        let val = u32::from_le_bytes(src[i..i + 4].try_into().expect("4-byte slice"));
        let h = hash3i(val);
        let entry = &mut table[h];

        // Find best match from hash table
        let mut best: Option<MatchInfo> = None;
        for slot in 0..OFFSETS_PER_HASH {
            let candidate_pos = entry[slot] as usize;
            if let Some(m) = try_match(src, i, candidate_pos, literal_start) {
                if best.as_ref().map_or(true, |b| m.gain > b.gain || (m.gain == b.gain && m.len > b.len)) {
                    best = Some(m);
                }
            }
        }

        // Try previous distance
        if d_prev > 0 && i >= d_prev {
            if let Some(m) = try_match(src, i, i - d_prev, literal_start) {
                // Bonus for reusing previous distance (cheaper encoding)
                let adjusted = MatchInfo { gain: m.gain + 1, ..m };
                if best.as_ref().map_or(true, |b| adjusted.gain > b.gain) {
                    best = Some(m);
                }
            }
        }

        // Update hash table (rotate: newest goes to slot 0)
        entry.rotate_right(1);
        entry[0] = i as i32;

        let literal_backlog = i - literal_start;

        match (&mut pending, best) {
            (None, None) => {
                // No match; force-emit literals if backlog too large
                if literal_backlog >= MAX_LITERAL_BACKLOG {
                    emit_literal(src, &mut dst, literal_start, literal_backlog);
                    literal_start = i;
                }
                i += 1;
            }
            (None, Some(m)) => {
                pending = Some(m);
                i += 1;
            }
            (Some(p), None) => {
                let lit_len = p.pos - literal_start;
                emit_match(src, &mut dst, literal_start, lit_len, p.len, p.dist, d_prev);
                d_prev = p.dist;
                literal_start = p.pos + p.len;
                i = literal_start;
                pending = None;
            }
            (Some(p), Some(m)) => {
                if m.pos >= p.pos + p.len {
                    // Non-overlapping: emit pending, keep new
                    let lit_len = p.pos - literal_start;
                    emit_match(src, &mut dst, literal_start, lit_len, p.len, p.dist, d_prev);
                    d_prev = p.dist;
                    literal_start = p.pos + p.len;
                    pending = Some(m);
                    i += 1;
                } else if m.gain > p.gain {
                    // Overlapping, new is better: discard pending, keep new
                    pending = Some(m);
                    i += 1;
                } else {
                    // Overlapping, pending is better: emit it, discard new
                    let lit_len = p.pos - literal_start;
                    emit_match(src, &mut dst, literal_start, lit_len, p.len, p.dist, d_prev);
                    d_prev = p.dist;
                    literal_start = p.pos + p.len;
                    i = literal_start;
                    pending = None;
                }
            }
        }
    }

    // Emit any pending match
    if let Some(p) = pending {
        let lit_len = p.pos - literal_start;
        emit_match(src, &mut dst, literal_start, lit_len, p.len, p.dist, d_prev);
        literal_start = p.pos + p.len;
    }

    // Emit remaining literals
    let remaining = src.len() - literal_start;
    if remaining > 0 {
        emit_literal(src, &mut dst, literal_start, remaining);
    }

    // End-of-stream: 0x06 followed by 7 zero bytes (Apple requires 8-byte EOS block)
    dst.extend_from_slice(&[0x06, 0, 0, 0, 0, 0, 0, 0]);

    dst
}

fn encode_tiny(src: &[u8]) -> Vec<u8> {
    let mut dst = Vec::with_capacity(src.len() + 12);
    if !src.is_empty() {
        emit_literal(src, &mut dst, 0, src.len());
    }
    dst.extend_from_slice(&[0x06, 0, 0, 0, 0, 0, 0, 0]);
    dst
}

fn hash3i(val: u32) -> usize {
    let i = val & 0xFFFFFF;
    let h = i.wrapping_mul(1 + (1 << 6) + (1 << 12)) >> 12;
    (h as usize) & (HASH_VALUES - 1)
}

fn try_match(src: &[u8], pos: usize, candidate: usize, literal_start: usize) -> Option<MatchInfo> {
    if candidate >= pos || pos - candidate > MAX_DISTANCE { return None; }
    let dist = pos - candidate;

    // Quick 4-byte check
    if pos + 4 > src.len() || candidate + 4 > src.len() { return None; }
    if src[pos..pos + 4] != src[candidate..candidate + 4] { return None; }

    // Expand forward
    let max_len = src.len().min(pos + 271) - pos; // LZVN max match ≈271
    let max_back = (candidate - candidate.min(literal_start)).min(pos - literal_start);
    let mut len = 4;
    while len < max_len && src[pos + len] == src[candidate + len] {
        len += 1;
    }

    // Expand backward into literal
    let mut back = 0;
    while back < max_back && src[pos - back - 1] == src[candidate - back - 1] {
        back += 1;
    }

    let total_len = len + back;
    let match_pos = pos - back;
    let cost = if dist < 2048 { 2isize } else { 3 };
    let gain = total_len as isize - cost;

    if gain < 1 || total_len < 3 { return None; }

    Some(MatchInfo { pos: match_pos, len: total_len, dist, gain })
}

fn emit_literal(src: &[u8], dst: &mut Vec<u8>, start: usize, mut len: usize) {
    let mut p = start;
    while len > 15 {
        let x = len.min(271);
        dst.push(0xE0);
        dst.push((x - 16) as u8);
        dst.extend_from_slice(&src[p..p + x]);
        p += x;
        len -= x;
    }
    if len > 0 {
        dst.push(0xE0 + len as u8);
        dst.extend_from_slice(&src[p..p + len]);
    }
}

fn emit_match(src: &[u8], dst: &mut Vec<u8>, lit_start: usize, mut lit_len: usize, mut match_len: usize, dist: usize, d_prev: usize) {
    let mut p = lit_start;

    // Emit large literal prefix (>3 bytes handled separately)
    while lit_len > 15 {
        let x = lit_len.min(271);
        dst.push(0xE0);
        dst.push((x - 16) as u8);
        dst.extend_from_slice(&src[p..p + x]);
        p += x;
        lit_len -= x;
    }
    if lit_len > 3 {
        dst.push(0xE0 + lit_len as u8);
        dst.extend_from_slice(&src[p..p + lit_len]);
        p += lit_len;
        lit_len = 0;
    }

    // First opcode encodes up to 3 literal bytes and up to 10 match bytes
    let l = lit_len;
    let first_m = match_len.min(10 - 2 * l);
    match_len -= first_m;
    let x = first_m - 3; // 0..7

    // Read up to 4 literal bytes (we'll store only L of them)
    let mut lit_buf = [0u8; 4];
    let avail = (src.len() - p).min(4);
    lit_buf[..avail].copy_from_slice(&src[p..p + avail]);

    if dist == d_prev {
        if l == 0 {
            dst.push(0xF0 + (x as u8 + 3)); // sml_m: 1111MMMM
        } else {
            dst.push((l << 6) as u8 + (x << 3) as u8 + 6); // pre_d: LLMMM110
        }
        dst.extend_from_slice(&lit_buf[..l]);
    } else if dist < 2048 - 2 * 256 {
        // Short distance
        dst.push((dist >> 8) as u8 + (l << 6) as u8 + (x << 3) as u8); // sml_d: LLMMMDDD
        dst.push(dist as u8);
        dst.extend_from_slice(&lit_buf[..l]);
    } else if dist >= (1 << 14) || match_len == 0 || first_m + match_len > 34 {
        // Long distance
        dst.push((l << 6) as u8 + (x << 3) as u8 + 7); // lrg_d: LLMMM111
        dst.extend_from_slice(&(dist as u16).to_le_bytes());
        dst.extend_from_slice(&lit_buf[..l]);
    } else {
        // Medium distance — fold remaining match into opcode
        let total_x = x + match_len;
        match_len = 0;
        dst.push(0xA0 + (total_x >> 2) as u8 + (l << 3) as u8); // med_d
        let d_enc = (dist << 2 | (total_x & 3)) as u16;
        dst.extend_from_slice(&d_enc.to_le_bytes());
        dst.extend_from_slice(&lit_buf[..l]);
    }

    // Emit remaining match length
    while match_len > 15 {
        let x = match_len.min(271);
        dst.push(0xF0);
        dst.push((x - 16) as u8);
        match_len -= x;
    }
    if match_len > 0 {
        dst.push(0xF0 + match_len as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_zeros() {
        let data = vec![0u8; 256];
        let compressed = encode(&data);
        let mut decompressed = vec![0u8; data.len()];
        let n = decode(&compressed, &mut decompressed).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&decompressed, &data);
    }

    #[test]
    fn roundtrip_sequential() {
        let data: Vec<u8> = (0..=255).cycle().take(1024).collect();
        let compressed = encode(&data);
        let mut decompressed = vec![0u8; data.len()];
        let n = decode(&compressed, &mut decompressed).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&decompressed, &data);
    }

    #[test]
    fn roundtrip_random() {
        let mut data = vec![0u8; 4096];
        let mut rng = 42u32;
        for b in data.iter_mut() {
            rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
            *b = (rng >> 16) as u8;
        }
        let compressed = encode(&data);
        let mut decompressed = vec![0u8; data.len()];
        let n = decode(&compressed, &mut decompressed).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&decompressed, &data);
    }

    #[test]
    fn roundtrip_tiny() {
        for len in 0..=16 {
            let data: Vec<u8> = (0..len).collect();
            let compressed = encode(&data);
            let mut decompressed = vec![0u8; data.len().max(1)];
            let n = decode(&compressed, &mut decompressed).unwrap();
            assert_eq!(n, data.len(), "failed for len={len}");
            assert_eq!(&decompressed[..n], &data[..]);
        }
    }

    #[test]
    fn roundtrip_repetitive() {
        // Highly compressible: repeated pattern
        let pattern = b"abcdefgh";
        let data: Vec<u8> = pattern.iter().copied().cycle().take(8192).collect();
        let compressed = encode(&data);
        assert!(compressed.len() < data.len() / 2, "should compress well");
        let mut decompressed = vec![0u8; data.len()];
        let n = decode(&compressed, &mut decompressed).unwrap();
        assert_eq!(n, data.len());
        assert_eq!(&decompressed, &data);
    }

    #[test]
    fn decode_known_lzvn() {
        // Known LZVN from Apple's encoder: 32x1 sequential 0..31
        // e0 10 00 01 02 ... 1f 06
        let mut compressed = vec![0xE0u8, 0x10];
        compressed.extend(0u8..32);
        compressed.push(0x06);
        let mut decompressed = vec![0u8; 32];
        let n = decode(&compressed, &mut decompressed).unwrap();
        assert_eq!(n, 32);
        let expected: Vec<u8> = (0..32).collect();
        assert_eq!(&decompressed, &expected);
    }
}
