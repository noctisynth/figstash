---
name: figstash-cli
description: Use the Figstash CLI to inspect existing Figma snapshots locally and, only with explicit user authorization, create or refresh a quota-sensitive snapshot. Apply when an agent needs Figma design context through Figstash.
---

# Figstash CLI

Use `figstash` as a machine-oriented JSON CLI. Prefer the built binary; during repository development, replace `figstash` with `cargo run -q -p figstash-cli --`.

## Tier 1 safety invariant

Treat every Tier 1 request as a scarce, user-visible operation. A request to inspect a design, read a node, or implement from a Figma URL is not authorization to pull or refresh it.

Before doing any design read, extract the `FILE_KEY` from the URL and inspect local state without allowing network access:

```bash
figstash --offline snapshot list --file FILE_KEY
figstash --offline snapshot status FILE_KEY
```

If the required file/profile already has a snapshot, do not run `snapshot pull`; continue with `--offline` local queries. Do not refresh merely to check whether Figma is newer.

If no suitable snapshot exists, stop before networking and tell the user:

- the exact file and request profile that are missing;
- why cached data cannot satisfy the request;
- that `GET /v1/files/:key` is Tier 1 and the proposed command can spend one request;
- whether the command includes `--force`, `--version`, or `--geometry paths`.

Run the proposed pull only after unambiguous user authorization for that exact operation. Authorization is single-use: it does not cover a retry after failure, another file, another request profile/version, or a later refresh. Never use `--force` unless the user explicitly requested and authorized a refresh. Never retry a Tier 1 failure automatically.

When a snapshot exists and the user explicitly requests a refresh, disclose that a
normal pull first calls Tier 3 `GET /v1/files/:key/meta` and can then call Tier 1
`GET /v1/files/:key` once only when the version changed. If
`file_metadata:read` is unavailable, the command fails closed without Tier 1 and
reports the explicit `--force` command; do not run that command without separate
authorization.

## Contract discovery

Run `figstash schema` to list stable commands and `figstash schema <command>` for exact inputs, outputs, errors, network behavior, and examples. Treat this runtime schema and `schemas/cli/v1/` as authoritative when this skill and the binary differ.

Every normal invocation emits one JSON envelope and a trailing newline. Read:

- `ok` before consuming `data`;
- `error.code`, `error.message`, and `error.details` on failure;
- `meta.source`, `meta.snapshotId`, and `meta.network.attempts` to verify provenance and network use;
- `warnings` for non-fatal limitations.

Do not parse human-readable stderr as command data.

## Target and node syntax

Use either a Figma file URL or its `FILE_KEY` as `<target>`. Select a node with either a URL query such as `?node-id=2-1` or `--node 2:1`. Do not supply conflicting node IDs in both places.

Global options may precede the command:

```text
--data-dir <path> --config-dir <path> --offline --log-level <level>
```

Use `--offline` when the operation must be structurally prevented from networking.

## Recommended agent workflow

After the offline preflight confirms a snapshot exists, discover and read design context entirely locally:

```bash
figstash --offline outline FILE_KEY
figstash --offline context FILE_KEY --node 2:1
```

`outline` defaults to depth 2 and returns a sparse document/page/top-level-node tree. Increase or reduce it with `--depth`; scope it with `--node` when useful.

`context` is the preferred design-to-code read. It requires a node and returns a compact subtree plus only the styles, repeated values, components, and component sets referenced by that subtree. If it returns `node_required`, run `outline` and retry with a node. Use `--depth`, `--snapshot`, or `--geometry paths` only when the task needs them.

For lower-level inspection:

```bash
figstash --offline node get FILE_KEY --node 2:1 --depth 2
figstash --offline node get FILE_KEY --node 2:1 --view raw
figstash --offline node search FILE_KEY --name Card --type FRAME
figstash --offline node search FILE_KEY --text 'Hello Agent'
figstash --offline tokens get FILE_KEY
figstash --offline components list FILE_KEY
```

Use `--snapshot <id>` for a historical immutable snapshot. `--geometry paths` selects a lineage captured with vector paths; it does not generate geometry that was not pulled.

## Snapshot and request rules

Two commands can call Figma explicitly: `auth whoami` and `snapshot pull`. `auth whoami` is Tier 3. An authorized, uncached `snapshot pull` performs one Tier 1 `GET /v1/files/:key` and stores the result locally. For an existing snapshot, a normal pull spends one Tier 3 metadata request and spends one Tier 1 request only when the returned version changed. The following is an example only, not permission to pull automatically:

```bash
printf '%s' "$FIGMA_TOKEN" | figstash auth set --stdin
figstash auth whoami
figstash snapshot pull FILE_KEY
```

The pull endpoints must serialize exactly as `https://api.figma.com/v1/files/FILE_KEY` and `https://api.figma.com/v1/files/FILE_KEY/meta`, without a duplicated slash or empty trailing query. Treat any different endpoint shape as a Figstash defect; do not retry it against Figma. An unexpected 404 is not authorization to spend another Tier 1 request—validate identity with `auth whoami`, inspect the reported endpoint, and stop for diagnosis.

Generate the PAT with `current_user:read`, `file_content:read`, and `file_metadata:read`. `auth status` checks only local presence with zero network attempts. `auth whoami` explicitly performs `GET /v1/me`, returns the authenticated user, and normally records one Tier 3 attempt; a connection failure or 5xx may cause the single safe Tier 2/3 retry allowed by policy. It does not consume Tier 1 file-content quota. Use `--offline auth whoami` when remote validation must be prohibited.

Never put a token directly in argv, logs, prompts, or committed files. `--force` skips metadata probing and spends one Tier 1 request. A normal pull for an already cached lineage returns `unchanged` with one Tier 3 and zero Tier 1 attempts when the version matches. If metadata access is unavailable, it returns `metadata_unavailable` instead of silently spending Tier 1. This CLI guard is defense in depth; the Agent must still perform the offline preflight and obtain the per-operation authorization described above.

All of these are local after a pull: `context`, `outline`, `node get`, `node search`, `tokens get`, `components list`, `snapshot status`, `snapshot list`, `snapshot diff`, `snapshot prune`, `quota status`, and `schema`. A local cache miss is an error; it must not trigger an implicit pull.

Useful maintenance commands:

```bash
figstash snapshot status FILE_KEY
figstash snapshot list --file FILE_KEY
figstash quota status
figstash doctor
figstash auth status
figstash auth whoami
```

`snapshot prune` is a dry-run plan. `snapshot prune --execute` permanently removes eligible snapshots but preserves the sole snapshot and current HEAD. `snapshot diff` currently returns `not_implemented`.

## Choosing commands

- Unknown node: `outline`, then `context`.
- Known node for implementation: `context`.
- Exact stored Figma payload: `node get --view raw`.
- Find nodes by name, text, type, or ancestor: `node search`.
- Inspect design primitives: `tokens get` or `components list`.
- Check local credential presence: `auth status`; verify its remote identity only when needed with `auth whoami`.
- Check an unfamiliar or newly added command: `schema <command>` before invoking it.
