//! Allocation-amplification bound: a hostile header must not be able to
//! make the decoder allocate wildly more than the caller's own buffer.
//!
//! A tracking global allocator records peak live bytes, so this measures
//! real behavior rather than reasoning about it. The deepmap2 decoder
//! derives image height from the caller's pixel buffer (not the header),
//! so amplification should stay a small constant factor.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Tracking;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            let live = if new_size >= layout.size() {
                LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + (new_size - layout.size())
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed) - (layout.size() - new_size)
            };
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static A: Tracking = Tracking;

fn measure<F: FnOnce()>(f: F) -> usize {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    f();
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

use libdm2::{dm2_decode, ImageInfo, PixelFormat};

/// Build a deepmap2 header claiming the given tile geometry, followed by
/// tile records whose declared sizes are honest but whose payloads are junk.
fn hostile_stream(compression: u8, format: u8, param: u8, tile_w: u16, tile_h: u16) -> Vec<u8> {
    let mut d = b"dmp2".to_vec();
    d.push(compression);
    d.push(0); // quality
    d.push(param);
    d.push(format);
    d.extend_from_slice(&tile_w.to_le_bytes());
    d.extend_from_slice(&tile_h.to_le_bytes());
    // A few small tiles of junk; the decoder sizes its scratch from the
    // header geometry, not from these.
    for _ in 0..4 {
        let payload: Vec<u8> = (0..48u8).collect();
        d.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        d.extend_from_slice(&payload);
    }
    d
}

#[test]
fn hostile_header_cannot_blow_up_allocation() {
    // Caller buffers of a few sizes; the decoder must stay within a small
    // multiple of each regardless of what the header claims.
    const LIMIT_FACTOR: usize = 6;
    const FLOOR: usize = 1 << 20; // fixed-size scratch (FSE tables, literals)

    let formats: &[(u8, PixelFormat)] = &[
        (1, PixelFormat::Gray8),
        (2, PixelFormat::GrayA8),
        (3, PixelFormat::Rgb8),
        (4, PixelFormat::Rgba8),
        (0x11, PixelFormat::Gray16),
        (0x12, PixelFormat::GrayA16),
        (0x13, PixelFormat::Rgb16),
        (0x14, PixelFormat::Rgba16),
    ];

    let mut worst = (0usize, String::new());

    for &(fmt_code, fmt) in formats {
        let ps = fmt.pixel_size();
        for &compression in &[1u8, 2, 3, 4] {
            for &tile_w in &[1u16, 2, 256, 4096, 16384, 32768, 65535] {
                for &tile_h in &[1u16, 256, 4096, 65535] {
                    let param = if fmt.is_16bit() { 10 } else { 0 };
                    let data = hostile_stream(compression, fmt_code, param, tile_w, tile_h);

                    // Caller buffer must be a whole number of rows or the
                    // decoder rejects before doing any work.
                    let row_bytes = (tile_w as usize) * ps;
                    for rows in [1usize, 4, 64] {
                        let buf_len = row_bytes * rows;
                        if buf_len == 0 || buf_len > 8 << 20 {
                            continue;
                        }
                        let mut pixels = vec![0u8; buf_len];
                        let mut info =
                            ImageInfo { width: 0, height: 0, format: PixelFormat::Gray8 };

                        let peak = measure(|| {
                            let _ = dm2_decode(&data, &mut pixels, &mut info);
                        });

                        let allowed = FLOOR + buf_len * LIMIT_FACTOR;
                        let label = format!(
                            "{fmt:?} comp={compression} tile={tile_w}x{tile_h} buf={buf_len}"
                        );
                        if peak > worst.0 {
                            worst = (peak, format!("{label} peak={peak} allowed={allowed}"));
                        }
                        assert!(
                            peak <= allowed,
                            "[{label}] decoder allocated {peak} bytes for a {buf_len}-byte \
                             caller buffer (limit {allowed})"
                        );
                    }
                }
            }
        }
    }
    eprintln!("worst amplification case: {}", worst.1);
}
