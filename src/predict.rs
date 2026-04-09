/// Prediction modes for type 2 (default) compression.
/// Each mode predicts each pixel from its neighbors; the encoder stores
/// the signed residual (actual - predicted) which tends to be near zero
/// for smooth images.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PredictMode {
    None = 0,
    UpLeft = 1,
    Left = 2,
    Up = 3,
    Mean = 4,
}

impl PredictMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::UpLeft),
            2 => Some(Self::Left),
            3 => Some(Self::Up),
            4 => Some(Self::Mean),
            _ => None,
        }
    }
}

fn cost(residuals: &[i16]) -> f32 {
    residuals.iter().map(|&r| (r as f32).abs()).sum()
}

/// Select the best prediction mode for a row and write residuals into `out`.
/// `prev_row` is None for the first row of the image.
pub fn predict_row(
    row: &[i16],
    prev_row: Option<&[i16]>,
    out: &mut [i16],
    scratch: &mut [i16],
) -> PredictMode {
    let w = row.len();
    debug_assert_eq!(out.len(), w);
    debug_assert!(scratch.len() >= w);

    // None
    out[..w].copy_from_slice(row);
    let mut best_cost = cost(out);
    let mut best_mode = PredictMode::None;

    // Left
    scratch[0] = row[0];
    for i in 1..w {
        scratch[i] = row[i].wrapping_sub(row[i - 1]);
    }
    let c = cost(scratch);
    if c < best_cost {
        best_cost = c;
        best_mode = PredictMode::Left;
        out[..w].copy_from_slice(&scratch[..w]);
    }

    if let Some(prev) = prev_row {
        // Apple prefers Up over UpLeft in ties: test Up first with strict <,
        // then UpLeft with strict < — Up wins ties because it's set first.

        // Up
        for i in 0..w {
            scratch[i] = row[i].wrapping_sub(prev[i]);
        }
        let c = cost(scratch);
        if c < best_cost {
            best_cost = c;
            best_mode = PredictMode::Up;
            out[..w].copy_from_slice(&scratch[..w]);
        }

        // UpLeft is not used in the mode-selection heuristic for 8-bit gray —
        // Apple's encoder for type 2 appears to only select among None/Left/Up.
        // (UpLeft as mode 1 appears in Apple's output for specific multi-channel
        // cases via _RowEncodeYCC, not through the predict_row cost comparison.)
        let _ = best_cost;
    }

    best_mode
}

/// Reverse a prediction to recover original values.
///
/// Returns `Err(Dm2Error::DecodeFailed)` if the mode requires a previous
/// row but `prev_row` is `None` — this can happen when a corrupt header
/// stores a non-trivial mode for the first row of a tile.
pub fn unpredict_row(
    residuals: &[i16],
    prev_row: Option<&[i16]>,
    mode: PredictMode,
    out: &mut [i16],
) -> crate::error::Result<()> {
    use crate::error::Dm2Error;
    let w = residuals.len();
    let needs_prev = || prev_row.ok_or(Dm2Error::DecodeFailed);
    match mode {
        PredictMode::None => {
            out[..w].copy_from_slice(residuals);
        }
        PredictMode::Left => {
            out[0] = residuals[0];
            for i in 1..w {
                out[i] = residuals[i].wrapping_add(out[i - 1]);
            }
        }
        PredictMode::Up => {
            let prev = needs_prev()?;
            for i in 0..w {
                out[i] = residuals[i].wrapping_add(prev[i]);
            }
        }
        PredictMode::UpLeft => {
            // 2-way Paeth: select between up and left (prefer left on tie)
            let prev = needs_prev()?;
            out[0] = residuals[0].wrapping_add(prev[0]);
            for i in 1..w {
                let p = prev[i] as i32 + out[i - 1] as i32 - prev[i - 1] as i32;
                let pa = (p - out[i - 1] as i32).unsigned_abs();
                let pb = (p - prev[i] as i32).unsigned_abs();
                let pred = if pb < pa { prev[i] } else { out[i - 1] };
                out[i] = residuals[i].wrapping_add(pred);
            }
        }
        PredictMode::Mean => {
            // (left + up + 1) / 2 with truncation-toward-zero for negative sums
            let prev = needs_prev()?;
            out[0] = residuals[0].wrapping_add(prev[0]); // x=0: pred = up
            for i in 1..w {
                let mut sum = out[i - 1] as i32 + prev[i] as i32 + 1;
                if sum < 0 { sum += 1; } // truncation toward zero correction
                let pred = (sum >> 1) as i16;
                out[i] = residuals[i].wrapping_add(pred);
            }
        }
    }
    Ok(())
}

pub fn zigzag_encode(x: i16) -> u16 {
    ((x as i32 * 2) ^ ((x as i32) >> 15)) as u16
}

pub fn zigzag_decode(x: u16) -> i16 {
    ((x >> 1) as i16) ^ (-((x & 1) as i16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_roundtrip() {
        for v in [0i16, 1, -1, 2, -2, 127, -128, i16::MAX, i16::MIN] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v, "failed for {v}");
        }
    }

    #[test]
    fn zigzag_known_values() {
        assert_eq!(zigzag_encode(0), 0);
        assert_eq!(zigzag_encode(1), 2);
        assert_eq!(zigzag_encode(-1), 1);
        assert_eq!(zigzag_encode(2), 4);
        assert_eq!(zigzag_encode(-2), 3);
    }

    #[test]
    fn predict_roundtrip() {
        let row = [10i16, 12, 14, 13, 15];
        let prev = [8i16, 10, 12, 11, 13];
        for mode in [PredictMode::None, PredictMode::Left, PredictMode::Up] {
            let mut residuals = vec![0i16; 5];
            let mut out = vec![0i16; 5];
            match mode {
                PredictMode::None => residuals.copy_from_slice(&row),
                PredictMode::Left => {
                    residuals[0] = row[0];
                    for i in 1..5 { residuals[i] = row[i] - row[i-1]; }
                }
                PredictMode::Up => {
                    for i in 0..5 { residuals[i] = row[i] - prev[i]; }
                }
                _ => continue,
            }
            unpredict_row(&residuals, Some(&prev), mode, &mut out).unwrap();
            assert_eq!(&out, &row, "roundtrip failed for {mode:?}");
        }
    }
}
