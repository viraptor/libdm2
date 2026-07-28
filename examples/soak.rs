//! Long-running randomized soak over every decoder entry point.
//! Not a test; run manually: cargo run --release --example soak -- <seconds>
use libdm2::{dm2_decode, dm2_encode_opts, dm2_read_info, lzfse, lzvn,
             Compression, ImageInfo, PixelFormat};
use std::time::{Duration, Instant};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); self.0 >> 16 }
    fn byte(&mut self) -> u8 { self.next() as u8 }
    fn below(&mut self, n: usize) -> usize { if n == 0 {0} else {(self.next() as usize) % n} }
}

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(30);
    let seed: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut rng = Lcg(seed);
    let fmts = [PixelFormat::Gray8, PixelFormat::GrayA8, PixelFormat::Rgb8, PixelFormat::Rgba8,
                PixelFormat::Gray16, PixelFormat::GrayA16, PixelFormat::Rgb16, PixelFormat::Rgba16];
    let comps = [Compression::None, Compression::Default, Compression::Lossless, Compression::Palette];

    // Build a pool of valid streams to mutate
    let mut pool: Vec<Vec<u8>> = Vec::new();
    for &fmt in &fmts {
        for &(w, h) in &[(3u32, 2u32), (16, 9), (40, 40), (100, 7)] {
            let px: Vec<u8> = (0..(w as usize*h as usize*fmt.pixel_size())).map(|i| (i*7%253) as u8).collect();
            let info = ImageInfo { width: w, height: h, format: fmt };
            for &c in &comps {
                let p = if fmt.is_16bit() {10} else {0};
                if let Ok(e) = dm2_encode_opts(&px, &info, c, 0, p) { pool.push(e); }
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut iters: u64 = 0;
    let mut slowest = Duration::ZERO;
    while Instant::now() < deadline {
        iters += 1;
        let mut data = if pool.is_empty() || rng.below(4) == 0 {
            (0..rng.below(400)).map(|_| rng.byte()).collect::<Vec<u8>>()
        } else {
            pool[rng.below(pool.len())].clone()
        };
        for _ in 0..1 + rng.below(10) {
            if data.is_empty() { break; }
            let p = rng.below(data.len());
            data[p] = rng.byte();
        }
        if rng.below(3) == 0 { let c = rng.below(data.len()+1); data.truncate(c); }

        let t0 = Instant::now();
        if let Ok((info, _)) = dm2_read_info(&data) {
            let want = (info.width as u64)*(info.height as u64)*(info.format.pixel_size() as u64);
            if want <= 32<<20 {
                let mut px = vec![0u8; want as usize];
                let mut oi = info.clone();
                let _ = dm2_decode(&data, &mut px, &mut oi);
            }
        }
        for s in [0usize, 7, 1024] { let mut px = vec![0u8; s];
            let mut i = ImageInfo{width:0,height:0,format:PixelFormat::Gray8};
            let _ = dm2_decode(&data, &mut px, &mut i); }
        let _ = lzfse::decode_buffer(&data, 1<<16);
        let mut d = vec![0u8; 1<<16];
        let _ = lzvn::decode(&data, &mut d);
        let el = t0.elapsed();
        if el > slowest { slowest = el; }
    }
    println!("soak seed={seed}: {iters} iterations in {secs}s, slowest single input {:?}", slowest);
}
