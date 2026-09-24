# Agent Note: Read-path numbers are JavaScript numbers

Status: implemented

English | [中文](2026-09-19-read-path-numbers-are-javascript-numbers.zh.md)

## Problem

The source's read tool, its renderer, the presentation model, and the client read card carry offsets, limits, configuration caps, line numbers, and totals as JavaScript numbers: `parsePositiveInteger` admits any finite integer of at least one, `readMetaFromMeta` compares replayed values with binary64 arithmetic, and messages and JSON output format them as `Number.prototype.toString` and `JSON.stringify` do. The port narrowed every one of those fields to `u64` or `usize`. An offset of 2^64 saturated to `18446744073709551615` in the out-of-range message where the source prints `18446744073709552000`, a configured cap of `1e300` failed to deserialize where the source accepts it, and a replayed read card with a line number past `u64` fell back to the generic card where the source renders it. Five parity rows stayed pending on exactly that gap.

## Decision

`seekdeep_lossless_json::JsonNumber` is one binary64 value with the source's semantics: `Display` follows `Number.prototype.toString` through `ryu-js`, serialization writes the digits of an exact integer below 2^53 and otherwise the shortest round-trip text as a raw JSON number, a non-finite value serializes as `null`, deserialization reads any JSON number literal the way `JSON.parse` does, including magnitudes past binary64 as infinities, and equality treats every NaN as one value. The read tool's arguments, input, caps, and outcome, the renderer's window, lines, totals, and replay metadata, `FileLocation.line`, `ReadFileLine.number`, `ReadResultView.offset` and `totalLines`, and the client read card carry it. Validation uses the source predicates (`Number.isInteger` and at least one for offsets, limits, caps, and line numbers; non-negative for totals), comparisons and the `offset - 1` and `endLine + 1` arithmetic run in binary64, and a cap bounds a Rust slice only through a saturating conversion at the point of use. The tools crate re-exports the type so tool crates that build a `FileLocation` need no further dependency.

## Alternatives considered

**Widen the fields to `u128` or `i128`.** Rejected: no integer type reproduces binary64 rounding, so `2^53 + 1` would compare unequal to `2^53` where the source's `offset - 1` makes them equal, and the formatting of 2^60 would still differ from the source's shortest digits.

**Keep `u64` and special-case the messages.** Rejected: the value itself is the observable surface; the tool result's `offset`, the persisted read metadata, and the replayed card all carry it, not only the diagnostics.

**Store `serde_json::Number` directly.** Rejected: the search card already does, but its text form gives no arithmetic or comparison, and the read path needs both; `JsonNumber` converts to it only when serializing a value whose JavaScript text is not its integer digits.

## Consequences

- The seven pending read-path and session-reentrancy rows are verified; the tool-fs differential test compares 4144 source observations, including offsets, limits, replayed line numbers, and totals at 2^53, 2^53+1, 2^60, 2^64, 1e300, and 1e400.
- Counts the runtime measures (a file's line total, a window's line index) enter the type through `From<u64>` and `From<usize>`, which round past 2^53 exactly as the source's arithmetic would; no file the harness can read reaches that range.
- Search result line numbers and totals still use `u64` and `serde_json::Number`; their rows are verified on other evidence and can move to `JsonNumber` when a gap appears.
