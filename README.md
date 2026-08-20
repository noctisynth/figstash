# Figstash

Figstash is a local-first, quota-aware Figma data CLI designed for software
agents. The repository is currently at the workspace-bootstrap stage; product
behavior has not been implemented yet.

The project is a new Rust implementation. It does not inject code into Figma
Desktop, load a Figma plugin, or provide a remote service.

## Repository layout

- `crates/figstash-core`: domain and application contracts
- `crates/figstash-figma`: Figma REST boundary
- `crates/figstash-store`: durable local snapshot storage
- `crates/figstash-query`: offline transforms and queries
- `crates/figstash-cli`: the `figstash` executable
- `schemas/cli/v1`: versioned Agent-facing JSON schemas
- `fixtures/figma`: sanitized or synthetic Figma input payloads
- `crates/*/tests/snapshots`: reviewed `insta` golden outputs

P2/P3 crates for SVG, visual regression, and MCP will only be added when those
phases begin.

## Development

The workspace uses Rust edition 2024 and has an MSRV of Rust 1.85.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Golden outputs use [`insta`](https://insta.rs/). Run `cargo insta test` and
`cargo insta review` when intentionally updating them. Raw Figma inputs stay in
`fixtures/figma`; generated `.snap` files live beside the tests that own them.

## Versioning

The repository uses [Semifold](https://github.com/noctisynth/semifold) with the
Rust resolver. Package versions are written explicitly in every member
`Cargo.toml` so Semifold can update each package deterministically.
`main` is the base branch; Semifold owns the separate `release` branch used by
its version/publish workflow. The release branch must never be set to `main`.

```bash
smif commit
smif status
smif version
smif config sync --check
```

See [DESIGN.md](./DESIGN.md) for the technical design and
[TODO.md](./TODO.md) for the prioritized implementation plan.
