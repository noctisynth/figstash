# Figstash CLI schema v1

Every normal CLI invocation emits exactly one JSON object followed by a newline.
Validate the complete object with `envelope.schema.json`, then validate successful
`data` against the schema named after `meta.command`.

The schema version changes only for breaking machine-contract changes. Additive
fields remain permitted so agents can ignore fields introduced by newer binaries.

Agent-facing high-level command schemas are:

- `context.schema.json`: one compact node subtree plus its referenced design context;
- `outline.schema.json`: sparse node discovery tree;
- `schema.schema.json`: command catalog or one detailed command contract.

Authentication schemas distinguish local presence from remote identity:

- `auth.status.schema.json`: zero-network local credential presence;
- `auth.whoami.schema.json`: explicitly validated Tier 3 current-user identity.

`figstash schema` exposes the same catalog at runtime. `figstash schema <command>`
returns its input schema, successful data schema, documented errors, network
classification, local-state effect, and examples without accessing Figma.

The minimal PAT scopes for the implemented command set are `file_content:read`
for snapshot pulls and `current_user:read` for `auth whoami`.
