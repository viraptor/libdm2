/// Color space transforms for type 2 encoding.
///
/// Apple's deepmap2 uses a modified YCoCg transform that differs from
/// the published YCoCg-R in two ways:
/// 1. Halving uses truncation toward zero (/2) not arithmetic shift (>>1)
/// 2. Negative Co and Cg values are decremented by 1 after computation
///
/// These modifications make the transform slightly asymmetric but improve
/// compression efficiency for LZFSE.

/// Truncation-toward-zero halving (differs from >>1 for negative odd values)
fn half(x: i16) -> i16 { x / 2 }

pub fn rgba_to_ycocg(pixels: &[u8], w: usize, y: &mut [i16], co: &mut [i16], cg: &mut [i16], a: &mut [i16]) {
    for i in 0..w {
        let r = pixels[i * 4] as i16;
        let g = pixels[i * 4 + 1] as i16;
        let b = pixels[i * 4 + 2] as i16;
        let alpha = pixels[i * 4 + 3] as i16;

        let mut co_val = r - b;
        let t = b + half(co_val);
        let mut cg_val = g - t;
        let y_val = t + half(cg_val);

        if co_val < 0 { co_val -= 1; }
        if cg_val < 0 { cg_val -= 1; }

        y[i] = y_val;
        co[i] = co_val;
        cg[i] = cg_val;
        a[i] = alpha;
    }
}

pub fn ycocg_to_rgba(y: &[i16], co: &[i16], cg: &[i16], a: &[i16], w: usize, pixels: &mut [u8]) {
    for i in 0..w {
        let mut co_val = co[i];
        let mut cg_val = cg[i];
        if co_val < 0 { co_val += 1; }
        if cg_val < 0 { cg_val += 1; }

        let t = y[i] - half(cg_val);
        let g = cg_val + t;
        let b = t - half(co_val);
        let r = co_val + b;

        pixels[i * 4] = r.clamp(0, 255) as u8;
        pixels[i * 4 + 1] = g.clamp(0, 255) as u8;
        pixels[i * 4 + 2] = b.clamp(0, 255) as u8;
        pixels[i * 4 + 3] = a[i].clamp(0, 255) as u8;
    }
}

pub fn rgb_to_ycocg(pixels: &[u8], w: usize, y: &mut [i16], co: &mut [i16], cg: &mut [i16]) {
    for i in 0..w {
        let r = pixels[i * 3] as i16;
        let g = pixels[i * 3 + 1] as i16;
        let b = pixels[i * 3 + 2] as i16;

        let mut co_val = r - b;
        let t = b + half(co_val);
        let mut cg_val = g - t;
        let y_val = t + half(cg_val);

        if co_val < 0 { co_val -= 1; }
        if cg_val < 0 { cg_val -= 1; }

        y[i] = y_val;
        co[i] = co_val;
        cg[i] = cg_val;
    }
}

pub fn ycocg_to_rgb(y: &[i16], co: &[i16], cg: &[i16], w: usize, pixels: &mut [u8]) {
    for i in 0..w {
        let mut co_val = co[i];
        let mut cg_val = cg[i];
        if co_val < 0 { co_val += 1; }
        if cg_val < 0 { cg_val += 1; }

        let t = y[i] - half(cg_val);
        let g = cg_val + t;
        let b = t - half(co_val);
        let r = co_val + b;

        pixels[i * 3] = r.clamp(0, 255) as u8;
        pixels[i * 3 + 1] = g.clamp(0, 255) as u8;
        pixels[i * 3 + 2] = b.clamp(0, 255) as u8;
    }
}


/// Convert an `f32` to IEEE 754 binary16 (half-float) bits, rounding to
/// nearest-even — the same conversion the hardware `fcvt` performs.
///
/// Used by the 16-bit type-2 decode path: RGBA16 deepmap2 streams store
/// channel values as fixed-point integer codes that expand to half-float
/// bit patterns (`value = code / 2^(param-1)`, alpha = `a8 / 255`), and
/// Apple's decoder emits the round-to-nearest-even half of that quotient.
pub fn f32_to_f16_bits(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x007f_ffff;
    if exp == 0xff {
        // Inf stays Inf; NaN keeps a quiet-bit payload so it stays NaN.
        return sign | 0x7c00 | if mant != 0 { 0x0200 } else { 0 };
    }
    let unbiased = exp - 127;
    if unbiased >= 16 {
        return sign | 0x7c00; // overflows half range -> ±Inf
    }
    if unbiased >= -14 {
        // Normal half. Rounding may carry from mantissa into exponent —
        // adding 1 to the packed value handles that correctly (IEEE trick).
        let mut h = (sign as u32) | ((((unbiased + 15) as u32) << 10) | (mant >> 13));
        let rest = mant & 0x1fff;
        if rest > 0x1000 || (rest == 0x1000 && (h & 1) != 0) {
            h += 1;
        }
        return h as u16;
    }
    // Subnormal half (or underflow to zero).
    if unbiased < -25 {
        return sign; // below half of the smallest subnormal -> ±0
    }
    let mant = mant | 0x0080_0000; // make the implicit leading 1 explicit
    let shift = (13 + (-14 - unbiased)) as u32; // 14..=24
    let mut h = (sign as u32) | (mant >> shift);
    let rest = mant & ((1u32 << shift) - 1);
    let halfway = 1u32 << (shift - 1);
    if rest > halfway || (rest == halfway && (h & 1) != 0) {
        h += 1;
    }
    h as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ycocg_rgba_roundtrip() {
        let pixels = [100u8, 150, 200, 255, 0, 128, 64, 200, 255, 255, 255, 128];
        let w = 3;
        let mut y = vec![0i16; w];
        let mut co = vec![0i16; w];
        let mut cg = vec![0i16; w];
        let mut a = vec![0i16; w];
        rgba_to_ycocg(&pixels, w, &mut y, &mut co, &mut cg, &mut a);
        let mut out = vec![0u8; 12];
        ycocg_to_rgba(&y, &co, &cg, &a, w, &mut out);
        assert_eq!(&out, &pixels);
    }

    /// Expected bit patterns cross-checked against Apple's decoder output on
    /// real .car RGBA16 renditions — see decode16 tests.
    #[test]
    fn f16_conversion_oracle_values() {
        // Alpha expansion a8/255 observed in vImageDeepmap2Decode output.
        assert_eq!(f32_to_f16_bits(23.0 / 255.0), 0x2dc6);
        assert_eq!(f32_to_f16_bits(129.0 / 255.0), 0x380c);
        assert_eq!(f32_to_f16_bits(55.0 / 255.0), 0x32e7);
        assert_eq!(f32_to_f16_bits(255.0 / 255.0), 0x3c00); // 1.0
        // Fixed-point colour codes / 512 (param=10) observed likewise.
        assert_eq!(f32_to_f16_bits(2.0 / 512.0), 0x1c00);
        assert_eq!(f32_to_f16_bits(98.0 / 512.0), 0x3220);
        assert_eq!(f32_to_f16_bits(-10.0 / 512.0), 0xa500);
        assert_eq!(f32_to_f16_bits(-5.0 / 512.0), 0xa100);
        // Specials and boundaries.
        assert_eq!(f32_to_f16_bits(0.0), 0x0000);
        assert_eq!(f32_to_f16_bits(-0.0), 0x8000);
        assert_eq!(f32_to_f16_bits(65504.0), 0x7bff); // half max
        assert_eq!(f32_to_f16_bits(65520.0), 0x7c00); // rounds up to +Inf
        assert_eq!(f32_to_f16_bits(65519.0), 0x7bff); // rounds down to max
        assert_eq!(f32_to_f16_bits(f32::INFINITY), 0x7c00);
        assert_eq!(f32_to_f16_bits(f32::NEG_INFINITY), 0xfc00);
        assert_eq!(f32_to_f16_bits(f32::NAN) & 0x7c00, 0x7c00);
        assert_ne!(f32_to_f16_bits(f32::NAN) & 0x03ff, 0);
        // Subnormal range: 2^-24 is the smallest half subnormal.
        assert_eq!(f32_to_f16_bits(5.9604645e-8), 0x0001); // 2^-24
        assert_eq!(f32_to_f16_bits(2.9802322e-8), 0x0000); // 2^-25 ties to even 0
        assert_eq!(f32_to_f16_bits(4.4703484e-8), 0x0001); // 1.5*2^-25 rounds up
        // Round-to-nearest-even on a normal boundary: 2049/2048 is exactly
        // between 1.0 and the next half (1+2^-10) -> ties to even (1.0).
        assert_eq!(f32_to_f16_bits(2049.0 / 2048.0), 0x3c00);
        assert_eq!(f32_to_f16_bits(2050.0 / 2048.0), 0x3c01); // exactly 1+2^-10
        assert_eq!(f32_to_f16_bits(2051.0 / 2048.0), 0x3c02); // tie -> even (up)
    }

    #[test]
    fn ycocg_rgb_roundtrip() {
        let pixels = [100u8, 150, 200, 0, 128, 64, 255, 255, 255];
        let w = 3;
        let mut y = vec![0i16; w];
        let mut co = vec![0i16; w];
        let mut cg = vec![0i16; w];
        rgb_to_ycocg(&pixels, w, &mut y, &mut co, &mut cg);
        let mut out = vec![0u8; 9];
        ycocg_to_rgb(&y, &co, &cg, w, &mut out);
        assert_eq!(&out, &pixels);
    }
}
