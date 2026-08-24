# Changelog

<!-- semifold:release version=0.1.0-alpha.0 -->
## v0.1.0-alpha.0

### Chores

- [`4d5e5b4`](https://github.com/noctisynth/figstash/commit/4d5e5b4c3934562ffa3c1b5759fc7fb37fbab186): Keep the supported Rust toolchain lint-clean while preserving full-history secret scanning in organization repositories.
- [`0f90101`](https://github.com/noctisynth/figstash/commit/0f901010af82e78cc072803d266cb78c2f51a7f8): Publish every Figstash package with AGPL-3.0-only licensing, complete crates.io metadata, and versioned internal path dependencies.

### New Features

- [`13f7a10`](https://github.com/noctisynth/figstash/commit/13f7a101879ed98c836af7b15f7c9846ba5f7017): Add an explicit Tier 3 auth whoami command that validates the configured PAT and returns the authenticated Figma user without consuming file-content quota.
- [`64e9232`](https://github.com/noctisynth/figstash/commit/64e923296e4a1686bfb8f1cd149deea9bea0bea0): Implement the complete P0 local-first Agent CLI, including quota-aware Figma pulls, durable snapshots, offline queries, stable JSON contracts, and security/quality gates.

### Dependencies

- Update figstash-core to 0.1.0-alpha.0.
<!-- semifold:release:end -->
