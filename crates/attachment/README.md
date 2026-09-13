# seekdeep-attachment

English | [中文](README.zh.md)

The durable attachment seam for SeekDeep Harness. `AttachmentStore` validates
and durably commits immutable image bytes, then returns a serializable
`ImageAttachmentRef`. Consumers never persist browser paths, object URLs,
provider URLs, or base64 payloads in session events.

`AttachmentId` is an opaque Rust newtype for the content-addressed storage
identity. A reference records that ID, the verified media type, exact encoded
byte length, intrinsic dimensions, and an optional display name stripped of
local path semantics. The version-one closed media enum accepts PNG, JPEG,
WebP, and GIF.

Unsent composer images remain browser-owned temporary drafts.
`validate_image` applies the same admission policy without persisting; batch
writers validate every member before saving any member so one malformed image
cannot strand earlier unreferenced objects. `save_image` validates and commits
each accepted image before its owning model-visible session event is appended.
`read_image` verifies that stored bytes still match the logged reference.

Callers may pass an `AbortSignal` to `read_image`. Implementations observe it
around backend and verification work and preserve its original reason rather
than translating cancellation into a storage error. `AttachmentError` carries a
stable routing code, human-safe message, and optional source without depending
on the LLM crate, which avoids a dependency cycle.

The store is registered on the typed `attachments` Cordis service slot. The
returned effect owns that registration and removes it on disposal. Concrete
backends own byte validation and immutable-object integrity; the seam's no-op
invariant companion reserves the renamed package identity without inventing a
mutable relationship.

## Model experience

The seam affects models indirectly through the role-neutral core image block
and provider adapters that resolve its durable reference. Adding an image
changes the provider request and invalidates the affected KV-cache suffix.

## Known limitations

- Version one accepts PNG, JPEG, WebP, and GIF only.
- Retention and garbage collection are deferred because resumed and forked
  sessions may share immutable objects.
- Generic files, audio, video, and persistent unsent drafts require separate
  lifecycle and provider contracts.
