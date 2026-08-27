# Agent Note: verify-cordis-config gates source ownership of configured plugins

Status: implemented

English | [中文](2026-07-30-cordis-config-source-plane-resolution-gate.zh.md)

## Problem

A Loader configuration names plugins by external package specifier, while the Rust launcher resolves first from its compiled `PluginCatalog` and permits model-authored JavaScript only through the compatibility path. Compatibility manifests, built JavaScript, and Cargo packages can drift independently: a local product specifier may remain in cordis.yml after its compiled Rust owner disappears, or a developer tree may mask the missing owner with stale `lib/` output. The resulting failure appears only when that composition boots from a clean checkout. A boot smoke proves one composition and platform; the repository contains shipped, example, fixture, and overlay configurations whose plugin sets differ.

## Decision

The Rust [`verify-cordis-config`](../../../../crates/repository-tools/src/cordis_config_verifier.rs) implementation requires every configured local package specifier to have source-tree Rust ownership. A Cargo package named `seekdeep-foo` owns `@seekdeep-ai/seekdeep-foo`; committed Rust source may declare additional exact package identities; and a small alias table covers compiled Loader built-ins and intentional NPM/Cargo naming differences. An alias becomes valid only when its named Cargo package exists. The check uses the package portion of a subpath specifier, reports every configuration that names an unowned package, and leaves relative, URL, and external model-authored JavaScript specifiers outside the local-package rule.

The same command preserves the surrounding source invariants: recursive group, insert, and include-patch metadata validation; owner-manifest dependencies; adaptive directory-picker packages mounted by runtime string; host-versus-session preset plane separation; and agreement between a client package's `./client` export and `seekdeep.client` declaration. `!!js` is parsed but never executed. The YAML boundary preserves actual tags while excluding quoted text, comments, and block-scalar bodies from tag normalization.

## Alternatives considered

**Rely on keyless boot smokes.** A smoke detects a missing owner only for the selected profile, environment, and platform. The static verifier covers every discovered configuration and reports all missing owners in one run.

**Treat compatibility `package.json` exports as source ownership.** Those exports name built JavaScript and declarations. Accepting them would recreate the stale-artifact masking problem and would make a foreign-language compatibility surface authoritative over Rust production ownership.

**Require identical NPM and Cargo suffixes.** Most packages follow that convention, but framework built-ins and a few established product identities intentionally differ. Exact conditional aliases keep those exceptions visible without weakening missing-owner detection.

## Consequences

- A configured local package with no compiled Rust owner makes `verify-cordis-config` fail before any profile boot.
- Adding or renaming a configured package requires a Cargo owner, an exact Rust-declared identity, or a reviewed conditional alias in the same change.
- External model-authored JavaScript remains supported; the ownership check applies only to repository-local SeekDeep and Cordis package identities.
