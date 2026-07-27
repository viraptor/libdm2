/// Prediction for type 2 (default) compression. The `PredictMode` enum
/// and all row-reconstruction code live in [`crate::verified`] with
/// Verus-checked specifications; this module re-exports the type and
/// keeps the (float-based, correctness-irrelevant) encoder heuristic.
pub use crate::verified::PredictMode;

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
///
/// All five mode inversions are implemented in [`crate::verified`] with
/// Verus-checked functional postconditions; this function only handles
/// the mode dispatch and the missing-previous-row error.
pub fn unpredict_row(
    residuals: &[i16],
    prev_row: Option<&[i16]>,
    mode: PredictMode,
    out: &mut [i16],
) -> crate::error::Result<()> {
    use crate::error::Dm2Error;
    use crate::verified;
    let w = residuals.len();
    let out = &mut out[..w];
    let needs_prev = || prev_row.ok_or(Dm2Error::DecodeFailed);
    match mode {
        PredictMode::None => verified::unpredict_none(residuals, out),
        PredictMode::Left => verified::unpredict_left(residuals, out),
        PredictMode::Up => verified::unpredict_up(residuals, &needs_prev()?[..w], out),
        PredictMode::UpLeft => verified::unpredict_upleft(residuals, &needs_prev()?[..w], out),
        PredictMode::Mean => verified::unpredict_mean(residuals, &needs_prev()?[..w], out),
    }
    Ok(())
}

/// Zigzag coding, delegated to the Verus-verified implementation in
/// [`crate::verified`] (proved to be a bijection with `zigzag_decode` as
/// its inverse). `tests/verified_props.rs` checks equivalence with the
/// historical formula `((x as i32 * 2) ^ ((x as i32) >> 15)) as u16`
/// exhaustively.
pub fn zigzag_encode(x: i16) -> u16 {
    crate::verified::zigzag_encode(x)
}

pub fn zigzag_decode(x: u16) -> i16 {
    crate::verified::zigzag_decode(x)
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
