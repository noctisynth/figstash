# Changelog

<!-- semifold:release version=0.1.0-alpha.1 -->
## v0.1.0-alpha.1

### Bug Fixes

- [`e03c4ab`](https://github.com/noctisynth/figstash/commit/e03c4ab445b29b727640643d5c1d4ab0bf7b49ce): Remove unreliable metadata-driven automatic refresh and restore zero-network fail-closed behavior for existing snapshots.

### New Features

- [`f1b8f31`](https://github.com/noctisynth/figstash/commit/f1b8f31a2c7780459104e3e8f44994af0fefba6e): Compare immutable local snapshots with deterministic node and entity change classifications, filters, and zero network access.

### Refactors

- [`053672f`](https://github.com/noctisynth/figstash/commit/053672fa82630c84adaa1b7eb8fb080931a86c5a): Move the durable SQLite catalog to Toasty models and async repository APIs while preserving store-v1 compatibility and raw SQL for SQLite-specific features.

    Raise the workspace MSRV to Rust 1.95, as required by Toasty.
<!-- semifold:release:end -->

<!-- semifold:release version=0.1.0-alpha.0 -->
## v0.1.0-alpha.0

### Chores

- [`0f90101`](https://github.com/noctisynth/figstash/commit/0f901010af82e78cc072803d266cb78c2f51a7f8): Publish every Figstash package with AGPL-3.0-only licensing, complete crates.io metadata, and versioned internal path dependencies.

### New Features

- [`dccc2d9`](https://github.com/noctisynth/figstash/commit/dccc2d9802577f9c564d97f44a913ee64071d56b): Add high-level context and outline commands plus runtime schema discovery so agents can discover nodes and consume self-contained local design context without implicit Figma requests.
- [`13f7a10`](https://github.com/noctisynth/figstash/commit/13f7a101879ed98c836af7b15f7c9846ba5f7017): Add an explicit Tier 3 auth whoami command that validates the configured PAT and returns the authenticated Figma user without consuming file-content quota.
- [`ecfa2fa`](https://github.com/noctisynth/figstash/commit/ecfa2facf2567879488aab175673298300972ca1): Return referenced styles, derived design values, and component metadata with compact node queries so an Agent can consume a self-contained offline design-to-code context.
- [`64e9232`](https://github.com/noctisynth/figstash/commit/64e923296e4a1686bfb8f1cd149deea9bea0bea0): Implement the complete P0 local-first Agent CLI, including quota-aware Figma pulls, durable snapshots, offline queries, stable JSON contracts, and security/quality gates.

### Dependencies

- Update figstash-core to 0.1.0-alpha.0.
- Update figstash-figma to 0.1.0-alpha.0.
- Update figstash-query to 0.1.0-alpha.0.
- Update figstash-store to 0.1.0-alpha.0.
<!-- semifold:release:end -->
