# seekdeep-util output retention

English | [中文](README.zh.md)

The dependency-light bounded-retention library in
`seekdeep_util::output_retention`. It answers only the mechanical question
“what was retained, and what was omitted?” Tool-specific code continues to own
exit status, file grouping, line numbering, provider failures, spill files, and
model-facing recovery prose.

This is a library, not a service or plugin. It registers nothing, emits no
events, and keeps state only for one retainer accumulation.

## API

`ItemRetainer<T>` keeps the first bounded number of ordered logical items while
continuing to count every observed item. `push` returns a `PushDecision`, and
`finish` returns the retained items, exact seen and kept counts, and an
`Omitted` value.

`TextRetainer` bounds a byte stream using one of three
`TextRetentionStrategy` variants:

- `Head` keeps a stable prefix.
- `Tail` keeps the final byte window.
- `HeadTail` keeps a stable prefix and suffix while omitting the middle.

The `try_*` constructors retain the source API's JavaScript-shaped numeric
boundary and reject negative, fractional, and non-finite budgets with the
field-specific diagnostic. Rust-native constructors accept already-validated
`usize` budgets.

`describe_omitted` renders the standardized omission clause, while
`format_retention_notice` combines that clause with recovery guidance supplied
by the consumer.

## Resource semantics

Item and text retainers are separate because they have different resource
models. An item consumer can preserve a complete side channel while retaining
only an inline preview, and therefore knows an exact omitted item count. Text
is a byte stream: head, tail, and head-tail retention provide bounded memory
and report exact omitted bytes.

`truncated` is strictly a budget fact. It means otherwise-available input was
omitted by the retainer; it never means an upstream operation was incomplete.
Permission failures, skipped binary files, partial provider failures, and
unreadable candidates remain in the owning tool's domain result.

## UTF-8 boundaries

Text caps and omission counts are bytes, not characters. At `finish`, a cut
through a UTF-8 code point is trimmed so the retention boundary itself cannot
introduce a replacement character. Prefix and suffix are decoded separately,
so a code point is never reconstructed across an omitted middle. Malformed
bytes that were already wholly inside a retained region remain subject to
lossy decoding, matching the source behavior.

## Consumer mapping

- `glob`, `grep`, and web-search source previews use item-head retention.
- process and shell output use text-tail or text-head-tail retention.
- bounded response bodies use text-head or text-head-tail retention.

Consumers still own spill persistence, pagination, model-visible notices, and
cache consequences. The line-window contract used by file reads remains a
separate abstraction because one omitted count cannot describe both sides of a
paginated window.

## Model and cache effect

None directly. A consumer decides how retained content and omission metadata
become model-visible and owns any request-prefix change.

## Limitations

Item retention supports head retention only. Text retention is byte-oriented;
line and character windows require a domain-specific renderer, and a cut may
discard partial UTF-8 boundary bytes to keep the returned text valid.
