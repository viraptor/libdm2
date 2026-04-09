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
