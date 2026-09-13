//! Safe `f64` limit to integer count conversions for the fetch provider.

/// Floors a positive-finite `f64` byte/char/length limit into a `usize` count.
///
/// The source compares integer counts (byte length, character length, URL length) against these
/// limits and slices with `subarray(0, n)` / `slice(0, n)`, both of which truncate a fractional
/// bound. Floor the limit once so every downstream comparison and slice uses exact integer
/// arithmetic, which is parity-identical for integer counts.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn floor_to_usize(value: f64) -> usize {
    value.floor() as usize
}

/// Truncates a validated non-negative-integer `f64` into a `u64`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn trunc_to_u64(value: f64) -> u64 {
    value.trunc() as u64
}

/// An integer `count` exceeds a positive-finite `f64` limit exactly when it exceeds the limit's
/// floor.
#[must_use]
pub(crate) fn exceeds(count: usize, limit: f64) -> bool {
    count > floor_to_usize(limit)
}
