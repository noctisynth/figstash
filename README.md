# Figstash

Figstash is a local-first, quota-aware Figma data CLI designed for software
agents. One explicit Figma file pull becomes an immutable, compressed local
snapshot that can be searched and transformed repeatedly without networking.

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

## Agent quick start

Build the binary, store a personal access token without putting it in argv, and
pull one file:

```bash
cargo build --release -p figstash-cli
printf '%s' "$FIGMA_TOKEN" | target/release/figstash auth set --stdin
target/release/figstash auth whoami
target/release/figstash snapshot pull 'https://www.figma.com/design/FILE_KEY/Name'
```

Generate the PAT with the minimal scopes `current_user:read` and
`file_content:read`. `auth whoami` explicitly performs `GET /v1/me` to verify
the credential and report its Figma user. It is a Tier 3 request, not a Tier 1
file-content request; `auth status` remains the zero-network presence check.

The first pull performs exactly one Tier 1 `GET /v1/files/:key`. `--force` also
performs exactly one Tier 1 request and is never retried automatically. Until the
P1 metadata probe exists, an ordinary pull of an already cached lineage returns
`metadata_unavailable`; it never silently spends another Tier 1 request.

All of these commands are pure local reads after a snapshot exists:

```bash
figstash outline FILE_KEY
figstash context 'https://www.figma.com/design/FILE_KEY/Name?node-id=2-1'
figstash schema context
figstash node get FILE_KEY --node 2:1 --depth 2
figstash node get 'https://www.figma.com/design/FILE_KEY/Name?node-id=2-1' --view raw
figstash node search FILE_KEY --name Card --type FRAME
figstash node search FILE_KEY --text 'Hello Agent'
figstash tokens get FILE_KEY
figstash components list FILE_KEY
figstash snapshot status FILE_KEY
figstash snapshot list
figstash snapshot prune
figstash quota status
```

For the one-way design-to-code handoff, `context` is the preferred Agent-facing
shortcut:

```bash
figstash context 'https://www.figma.com/design/FILE_KEY/Name?node-id=2-1'
```

`data.node` contains the normalized node subtree. `data.references` contains
the named styles, repeated local design values, components, and component sets
actually referenced by that subtree. This handoff is fully local after the
snapshot pull; code generation remains the responsibility of the consuming
Agent and does not trigger a Figma request.

`context` requires a node in the URL or through `--node`; it never dumps a whole
file implicitly. Use `outline` first when the node is unknown. Its default depth
of two returns the document, pages, and their top-level nodes without loading
styling payloads. `schema` lists or describes the stable machine-readable CLI
contracts, for example `figstash schema snapshot.pull`.

`snapshot prune` only plans deletion. `snapshot prune --execute` performs the
reported permanent deletion and never removes the sole snapshot or a current
HEAD. `snapshot diff` is present as a stable command but returns
`not_implemented` until P1.

Every normal invocation writes exactly one JSON object plus a newline to stdout.
Errors use the same envelope and stable exit codes; diagnostics only use stderr.
Schemas live in [`schemas/cli/v1`](./schemas/cli/v1). Inspect
`meta.source`, `meta.snapshotId`, and `meta.network` on every result.

## Configuration and secrets

Precedence is CLI flag, environment, config file, then safe default. The primary
overrides are `--data-dir`, `FIGSTASH_DATA_DIR`, `--config-dir`, and
`FIGSTASH_CONFIG_DIR`. The config file is `config.toml` in the resolved OS config
directory and supports:

```toml
connect_timeout_seconds = 10
total_timeout_seconds = 120
inherit_proxy = true
maximum_download_bytes = 536870912
log_level = "warn"
# stale_after_seconds = 86400
```

`FIGMA_TOKEN` is always interpreted explicitly as a PAT and takes precedence
over the system keyring. PAT contents are never written to config, SQLite, logs,
or JSON output. The data directory is created with user-only permissions on Unix.
When P1 metadata probing is implemented, PATs using that feature will also need
`file_metadata:read`.

## Development

The workspace uses Rust edition 2024 and has an MSRV of Rust 1.85.

```bash
cargo fmt --all --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features -- -D warnings
INSTA_UPDATE=no cargo test --workspace --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

Golden outputs use [`insta`](https://insta.rs/). Run `cargo insta test` and
`cargo insta review` when intentionally updating them. Raw Figma inputs stay in
`fixtures/figma`; generated `.snap` files live beside the tests that own them.

## Versioning

The repository uses [Semifold](https://github.com/noctisynth/semifold) with the
Rust resolver. Package versions are written explicitly in every member
`Cargo.toml` so Semifold can update each package deterministically.
`main` is the base branch; Semifold owns the separate `release` branch used by
its version/publish workflow. All workspace packages carry crates.io-compatible
metadata and versioned internal path dependencies. The release branch must
never be set to `main`.

```bash
smif commit
smif status
smif config sync --check
```

`smif version`, release-branch writes, and publishing run only in GitHub Actions.
The root `schemas/cli/v1` directory is authoritative; the CLI crate contains a
byte-identical package mirror so published source archives build independently.
Repository tests reject drift between the two copies.

## License

Figstash is licensed under the
[GNU Affero General Public License v3.0 only](https://github.com/noctisynth/figstash/blob/main/LICENSE)
(`AGPL-3.0-only`).

See [DESIGN.md](./DESIGN.md) for the technical design and
[TODO.md](./TODO.md) for the prioritized implementation plan.
