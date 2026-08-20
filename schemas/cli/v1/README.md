# Figstash CLI schema v1

Every normal CLI invocation emits exactly one JSON object followed by a newline.
Validate the complete object with `envelope.schema.json`, then validate successful
`data` against the schema named after `meta.command`.

The schema version changes only for breaking machine-contract changes. Additive
fields remain permitted so agents can ignore fields introduced by newer binaries.
