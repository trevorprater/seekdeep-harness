# seekdeep-settings

English | [中文](README.zh.md)

The user-settings capability seam for SeekDeep Harness. One provider owns a
raw document of per-namespace sections. Plugins register a namespace schema and
read a resolved value layered as schema defaults, the registrant's composition
`base`, then the user section. Without a provider, optional consumers continue
using composition configuration alone.

## Service API

- `document_path()` returns an absolute user-editable file path when the
  provider has one. Browser protocols expose only derived availability and
  never the host path.
- `prepare_document()` makes a local document ready for an editor and returns
  its path. Non-file providers return `None`.
- `register(owner, ns, schema, options)` returns an owned `SettingsScope`.
  Disposal of the owner removes the namespace and observers. Duplicate
  namespaces and invalid stored sections fail at registration, the earliest
  point their schema and owner validator can judge them.
- `describe(redact)` returns registration-order descriptors with canonical
  schema JSON, resolved value, raw-section revision, detached `base` and `user`
  layers, apply timing, and—when redaction is requested—secret slots. Every
  wire surface must request redaction.
- `get(ns)` returns the current resolved value or `None` while unregistered.
- `update(ns, patch, expected_revision)` deep-merges an object patch into only
  the user layer, validates the complete resolved candidate, persists it, then
  commits. Arrays and scalars replace lower values wholesale.
- `replace(ns, section, expected_revision)` replaces the user layer; an empty
  object resets all keys to the base and schema defaults.
- `mutate(ns, ops, expected_revision)` applies ordered `set` and `unset` path
  operations to the section as it stands at the front of its write queue. This
  is the safe editing path for a redacted, incomplete view because it never
  restates secrets the caller did not receive.

Rust callers supply owned `serde_json::Value` inputs, which are detached and
JSON-shaped by construction. Compatibility bindings must reject functions,
dates, maps, big integers, non-finite numbers, undefined array members, class
instances, and cycles with a path-bearing error before constructing these
values, matching the source boundary.

Each descriptor's `revision` is a monotonic counter over the raw user section.
A stale expectation fails with `SettingsConflictError`, stable code
`SETTINGS_CONFLICT`, and both revisions. The check runs at the front of the
per-namespace queue, so queued predecessors cannot silently supersede an editor.
An identical stored section does not move the revision; an override equal to
the composition base does move it even though the resolved value is unchanged.

Scopes return detached immutable-by-ownership snapshots. Watcher calls are
asynchronous and serialized independently per callback in commit order.
Synchronous panics and asynchronous failures are contained. A disposed watcher
starts no queued or future invocation; already-started work settles. Service
teardown refuses new work and drains queued writes and started watchers. A
write whose registrant disappears during persistence still reaches storage but
does not commit or notify that old owner.

## Provider contract

Providers implement writable state, `load`, and `persist`, and may expose and
prepare one local document. `SettingsService::install` loads before publishing
the service and owns the complete service in a rollback-safe child lifecycle.
Provider watchers publish complete external documents through a weak
`SettingsPublisher`.

Each registered namespace re-resolves independently on publish. An invalid
section keeps that namespace's last good value and warns while other namespaces
continue. Once storage becomes valid again the namespace recovers. Boot-time
load failures and registration-time validation failures remain loud.

`install_settings_section` is the canonical optional-provider wiring. It uses
the composition entry as `base`, selects the registered resolved scope while a
provider exists, observes live commits, and restores the entry when the
provider disappears. Consumer teardown is deliberately silent, including a
stored change that lands while that consumer is unloading.

## Events

`settings/updated (ns, next, previous, source)` fires after a resolved-value
change. Source is the closed enum `update` or `provider`. Deep-equal values are
silent.

`settings/document-updated (ns, revision)` fires whenever the raw section
changes, even if resolution does not. Configuration surfaces use it to refresh
override state and conflict revisions.

Both events fan out to every synchronous listener. Ordinary listener failures
are contained and logged; invariant failures propagate only after the rest of
the listeners have run. Browser-facing bindings expose the same client-safe
namespace, source, and descriptor wire vocabulary.

## Model experience

Settings affect a model only through consumers that resolve model-affecting
values, such as a default route. This service directly changes no request
prefix or KV cache; each consumer owns that effect.

## Known limitations

- Resolution has schema defaults, one composition base, and one user layer; it
  does not report per-field provenance.
- `redact_secrets` is not a proven wire boundary. It follows object, dictionary,
  and array nodes only; secrets reachable solely through unions, intersections,
  or transforms can pass through, and serialized schema defaults can disclose a
  secret. Wire-exposed namespaces must use schemas the walker can prove until a
  fail-closed descriptor API exists.
- Cross-process concurrency is provider-defined. The seam serializes only
  in-process writes per namespace.
