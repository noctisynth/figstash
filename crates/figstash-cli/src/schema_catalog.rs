//! Static machine-readable CLI contract catalog.

use figstash_core::{AppError, AppResult, ErrorCode};
use serde_json::{Value, json};

const COMMANDS: &[&str] = &[
    "context",
    "outline",
    "schema",
    "auth.set",
    "auth.status",
    "auth.whoami",
    "auth.clear",
    "doctor",
    "quota.status",
    "snapshot.pull",
    "snapshot.status",
    "snapshot.list",
    "snapshot.diff",
    "snapshot.prune",
    "node.get",
    "node.search",
    "tokens.get",
    "components.list",
];

pub(crate) fn catalog() -> Value {
    json!({
        "commands": COMMANDS.iter().map(|name| summary(name)).collect::<Vec<_>>()
    })
}

pub(crate) fn contract(name: &str) -> AppResult<Value> {
    if !COMMANDS.contains(&name) {
        return Err(AppError::new(
            ErrorCode::InvalidArguments,
            "The requested CLI command contract does not exist.",
        )
        .with_details(json!({
            "command": name,
            "availableCommands": COMMANDS,
        })));
    }
    let (kind, network, endpoint_class, tier, writes_local_state) = effects(name);
    Ok(json!({
        "name": name,
        "kind": kind,
        "network": network,
        "endpointClass": endpoint_class,
        "tier": tier,
        "writesLocalState": writes_local_state,
        "inputSchema": input_schema(name),
        "outputSchema": output_schema(name)?,
        "errors": errors(name),
        "examples": examples(name),
    }))
}

fn summary(name: &str) -> Value {
    let (kind, network, endpoint_class, tier, writes_local_state) = effects(name);
    json!({
        "name": name,
        "kind": kind,
        "network": network,
        "endpointClass": endpoint_class,
        "tier": tier,
        "writesLocalState": writes_local_state,
        "schemaVersion": 1,
    })
}

fn effects(
    name: &str,
) -> (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<u8>,
    bool,
) {
    match name {
        "snapshot.pull" => (
            "online_command",
            "explicit",
            Some("get_file"),
            Some(1),
            true,
        ),
        "auth.whoami" => (
            "online_command",
            "explicit",
            Some("get_current_user"),
            Some(3),
            true,
        ),
        "doctor" => ("diagnostic", "optional_tcp_only", None, None, true),
        "auth.set" | "auth.clear" | "snapshot.prune" => {
            ("local_command", "never", None, None, true)
        }
        _ => ("local_query", "never", None, None, false),
    }
}

fn input_schema(name: &str) -> Value {
    let target = json!({"type": "string", "minLength": 1});
    let node = json!({"type": "string", "minLength": 1});
    let snapshot = json!({"type": "string", "minLength": 1});
    let geometry = json!({"enum": ["none", "paths"], "default": "none"});
    match name {
        "context" => json!({
            "type": "object", "required": ["target"],
            "properties": {"target": target, "node": node, "depth": {"type": "integer", "minimum": 0}, "snapshot": snapshot, "geometry": geometry},
            "additionalProperties": false
        }),
        "outline" => json!({
            "type": "object", "required": ["target"],
            "properties": {"target": target, "node": node, "depth": {"type": "integer", "minimum": 0, "default": 2}, "snapshot": snapshot, "geometry": geometry},
            "additionalProperties": false
        }),
        "schema" => json!({
            "type": "object", "properties": {"command": {"type": "string", "enum": COMMANDS}}, "additionalProperties": false
        }),
        "auth.set" => {
            json!({"type": "object", "required": ["stdin"], "properties": {"stdin": {"const": true}}, "additionalProperties": false})
        }
        "auth.status" | "auth.whoami" | "auth.clear" | "quota.status" => {
            json!({"type": "object"})
        }
        "snapshot.list" => json!({
            "type": "object", "properties": {"file": target}, "additionalProperties": false
        }),
        "snapshot.prune" => json!({
            "type": "object", "properties": {"execute": {"type": "boolean", "default": false}}, "additionalProperties": false
        }),
        "doctor" => {
            json!({"type": "object", "properties": {"network": {"type": "boolean", "default": false}}, "additionalProperties": false})
        }
        "snapshot.pull" => json!({
            "type": "object", "required": ["target"],
            "properties": {"target": target, "force": {"type": "boolean", "default": false}, "version": {"type": "string"}, "geometry": geometry},
            "additionalProperties": false
        }),
        "snapshot.status" | "tokens.get" | "components.list" => json!({
            "type": "object", "required": ["target"],
            "properties": {"target": target, "snapshot": snapshot, "geometry": geometry},
            "additionalProperties": false
        }),
        "snapshot.diff" => json!({
            "type": "object", "required": ["target", "snapshotA", "snapshotB"],
            "properties": {"target": target, "snapshotA": snapshot, "snapshotB": snapshot, "node": node, "type": {"type": "string"}, "path": {"type": "string"}, "geometry": geometry},
            "additionalProperties": false
        }),
        "node.get" => json!({
            "type": "object", "required": ["target"],
            "properties": {"target": target, "node": node, "depth": {"type": "integer", "minimum": 0}, "view": {"enum": ["compact", "raw"], "default": "compact"}, "snapshot": snapshot, "geometry": geometry},
            "additionalProperties": false
        }),
        "node.search" => json!({
            "type": "object", "required": ["target"],
            "properties": {"target": target, "name": {"type": "string"}, "text": {"type": "string"}, "type": {"type": "string"}, "ancestor": node, "limit": {"type": "integer", "minimum": 1, "default": 100}, "cursor": {"type": "string"}, "snapshot": snapshot, "geometry": geometry},
            "additionalProperties": false
        }),
        _ => json!({"type": "object"}),
    }
}

fn output_schema(name: &str) -> AppResult<Value> {
    let source = match name {
        "context" => include_str!("../schemas/cli/v1/context.schema.json"),
        "outline" => include_str!("../schemas/cli/v1/outline.schema.json"),
        "schema" => include_str!("../schemas/cli/v1/schema.schema.json"),
        "auth.set" => include_str!("../schemas/cli/v1/auth.set.schema.json"),
        "auth.status" => include_str!("../schemas/cli/v1/auth.status.schema.json"),
        "auth.whoami" => include_str!("../schemas/cli/v1/auth.whoami.schema.json"),
        "auth.clear" => include_str!("../schemas/cli/v1/auth.clear.schema.json"),
        "doctor" => include_str!("../schemas/cli/v1/doctor.schema.json"),
        "quota.status" => include_str!("../schemas/cli/v1/quota.status.schema.json"),
        "snapshot.pull" => include_str!("../schemas/cli/v1/snapshot.pull.schema.json"),
        "snapshot.status" => include_str!("../schemas/cli/v1/snapshot.status.schema.json"),
        "snapshot.list" => include_str!("../schemas/cli/v1/snapshot.list.schema.json"),
        "snapshot.diff" => include_str!("../schemas/cli/v1/snapshot.diff.schema.json"),
        "snapshot.prune" => include_str!("../schemas/cli/v1/snapshot.prune.schema.json"),
        "node.get" => include_str!("../schemas/cli/v1/node.get.schema.json"),
        "node.search" => include_str!("../schemas/cli/v1/node.search.schema.json"),
        "tokens.get" => include_str!("../schemas/cli/v1/tokens.get.schema.json"),
        "components.list" => include_str!("../schemas/cli/v1/components.list.schema.json"),
        _ => json_string_schema(),
    };
    serde_json::from_str(source).map_err(|error| {
        AppError::new(
            ErrorCode::Internal,
            "A built-in CLI output schema is invalid.",
        )
        .with_details(json!({"command": name, "reason": error.to_string()}))
    })
}

const fn json_string_schema() -> &'static str {
    r#"{"type":"object"}"#
}

fn errors(name: &str) -> Vec<&'static str> {
    match name {
        "context" => vec![
            "node_required",
            "snapshot_missing",
            "snapshot_not_found",
            "node_not_found",
            "store_corrupt",
            "store_failed",
        ],
        "outline" | "node.get" | "node.search" | "tokens.get" | "components.list" => vec![
            "snapshot_missing",
            "snapshot_not_found",
            "node_not_found",
            "invalid_arguments",
            "store_corrupt",
            "store_failed",
        ],
        "snapshot.diff" => vec![
            "snapshot_not_found",
            "invalid_arguments",
            "store_corrupt",
            "store_failed",
        ],
        "snapshot.pull" => vec![
            "offline_mode",
            "auth_missing",
            "auth_failed",
            "scope_missing",
            "metadata_unavailable",
            "rate_limited",
            "network_failed",
            "figma_error",
            "invalid_figma_response",
            "store_failed",
        ],
        "auth.whoami" => vec![
            "offline_mode",
            "auth_missing",
            "auth_failed",
            "scope_missing",
            "rate_limited",
            "network_failed",
            "figma_error",
            "invalid_figma_response",
            "store_failed",
        ],
        "schema" => vec!["invalid_arguments", "internal_error"],
        _ => vec!["invalid_arguments", "store_failed"],
    }
}

fn examples(name: &str) -> Vec<&'static str> {
    match name {
        "context" => vec![
            "figstash context 'https://www.figma.com/design/FILE_KEY/Name?node-id=2-1'",
            "figstash context FILE_KEY --node 2:1 --depth 2",
        ],
        "outline" => vec![
            "figstash outline FILE_KEY",
            "figstash outline FILE_KEY --node 1:1 --depth 1",
        ],
        "schema" => vec!["figstash schema", "figstash schema context"],
        "auth.whoami" => vec!["figstash auth whoami"],
        "snapshot.pull" => vec![
            "figstash snapshot pull FILE_KEY",
            "figstash snapshot pull FILE_KEY --force",
        ],
        "snapshot.diff" => vec![
            "figstash snapshot diff FILE_KEY SNAPSHOT_A SNAPSHOT_B",
            "figstash snapshot diff FILE_KEY SNAPSHOT_A SNAPSHOT_B --node 2:1 --type FRAME",
        ],
        "node.get" => vec!["figstash node get FILE_KEY --node 2:1 --view raw"],
        "node.search" => vec!["figstash node search FILE_KEY --name Card --type FRAME"],
        _ => Vec::new(),
    }
}
