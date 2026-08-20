//! Tolerant parser for the Figma `GET file` response.

use figstash_core::{
    AppError, AppResult, BoundingBox, ErrorCode, IndexedEntity, IndexedNode, IndexedSnapshot,
    Warning,
};
use serde_json::{Map, Value, json};

const MAX_NODE_COUNT: usize = 1_000_000;
const MAX_NODE_DEPTH: u32 = 512;

/// Parses a complete Figma file response into deterministic local indexes.
///
/// Every node's original non-children fields are retained. Children are stored
/// as independent rows and reconstructed by the query layer, avoiding quadratic
/// blob growth while keeping the raw view lossless.
///
/// # Errors
///
/// Returns `invalid_figma_response` for malformed, truncated, or resource-limit
/// violating payloads, and `internal_error` if deterministic hashing fails.
pub fn parse_file(bytes: &[u8]) -> AppResult<IndexedSnapshot> {
    let envelope: Value = serde_json::from_slice(bytes).map_err(|error| {
        AppError::new(
            ErrorCode::InvalidFigmaResponse,
            "Figma returned invalid or truncated JSON.",
        )
        .with_details(json!({"reason": error.to_string()}))
    })?;
    let object = envelope.as_object().ok_or_else(|| {
        AppError::new(
            ErrorCode::InvalidFigmaResponse,
            "The Figma file response must be a JSON object.",
        )
    })?;

    let file_name = required_string(object, "name")?;
    let figma_version = required_string(object, "version")?;
    let last_modified = optional_string(object, "lastModified");
    let document = object.get("document").ok_or_else(|| {
        AppError::new(
            ErrorCode::InvalidFigmaResponse,
            "The Figma file response is missing its document root.",
        )
    })?;

    let mut nodes = Vec::new();
    let mut warnings = Vec::new();
    let mut path = Vec::new();
    walk_node(document, None, 0, 0, &mut path, &mut nodes, &mut warnings)?;

    Ok(IndexedSnapshot {
        file_name,
        figma_version,
        last_modified,
        nodes,
        styles: parse_entity_map(object.get("styles"), "style"),
        components: parse_entity_map(object.get("components"), "component"),
        component_sets: parse_entity_map(object.get("componentSets"), "component_set"),
        warnings,
    })
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn walk_node(
    value: &Value,
    parent_id: Option<&str>,
    depth: u32,
    sibling_order: u32,
    path: &mut Vec<String>,
    output: &mut Vec<IndexedNode>,
    warnings: &mut Vec<Warning>,
) -> AppResult<String> {
    if depth > MAX_NODE_DEPTH {
        return Err(AppError::new(
            ErrorCode::InvalidFigmaResponse,
            "The Figma document exceeds the supported node nesting depth.",
        )
        .with_details(json!({"maximumDepth": MAX_NODE_DEPTH})));
    }
    if output.len() >= MAX_NODE_COUNT {
        return Err(AppError::new(
            ErrorCode::InvalidFigmaResponse,
            "The Figma document exceeds the supported node count.",
        )
        .with_details(json!({"maximumNodes": MAX_NODE_COUNT})));
    }

    let object = value.as_object().ok_or_else(|| {
        AppError::new(
            ErrorCode::InvalidFigmaResponse,
            "A Figma document node must be a JSON object.",
        )
    })?;
    let node_id = required_string(object, "id")?;
    let original_type = required_string(object, "type")?;
    let node_type = if is_known_node_type(&original_type) {
        original_type.clone()
    } else {
        warnings.push(
            Warning::new(
                "unknown_node_type",
                "An unknown Figma node type was retained in the raw view.",
            )
            .with_details(json!({"nodeId": node_id, "nodeType": original_type})),
        );
        "UNKNOWN".to_owned()
    };
    let name = optional_string(object, "name").unwrap_or_default();
    path.push(node_id.clone());

    let mut raw_object = object.clone();
    let children = raw_object.remove("children");
    let output_index = output.len();
    output.push(IndexedNode {
        node_id: node_id.clone(),
        parent_id: parent_id.map(ToOwned::to_owned),
        node_type,
        name,
        depth,
        sibling_order,
        path_ids: path.clone(),
        visible: object.get("visible").and_then(Value::as_bool),
        bounds: parse_bounds(object.get("absoluteBoundingBox")),
        component_id: optional_string(object, "componentId"),
        text_content: optional_string(object, "characters"),
        raw: Value::Object(raw_object.clone()),
        subtree_hash: String::new(),
    });

    let mut child_hashes = Vec::new();
    if let Some(children_value) = children {
        let array = children_value.as_array().ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidFigmaResponse,
                "A Figma node children field must be an array.",
            )
            .with_details(json!({"nodeId": node_id}))
        })?;
        for (index, child) in array.iter().enumerate() {
            let order = u32::try_from(index).map_err(|_| {
                AppError::new(
                    ErrorCode::InvalidFigmaResponse,
                    "A node has too many direct children.",
                )
                .with_details(json!({"nodeId": node_id}))
            })?;
            child_hashes.push(walk_node(
                child,
                Some(&node_id),
                depth + 1,
                order,
                path,
                output,
                warnings,
            )?);
        }
    }

    let own_bytes = serde_json::to_vec(&raw_object).map_err(|error| {
        AppError::new(
            ErrorCode::Internal,
            "Failed to serialize a normalized node for hashing.",
        )
        .with_details(json!({"reason": error.to_string()}))
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&own_bytes);
    for child_hash in child_hashes {
        hasher.update(child_hash.as_bytes());
    }
    let subtree_hash = hasher.finalize().to_hex().to_string();
    if let Some(indexed) = output.get_mut(output_index) {
        indexed.subtree_hash.clone_from(&subtree_hash);
    } else {
        return Err(AppError::new(
            ErrorCode::Internal,
            "Parser lost the node slot reserved during traversal.",
        ));
    }
    path.pop();
    Ok(subtree_hash)
}

fn required_string(object: &Map<String, Value>, key: &str) -> AppResult<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidFigmaResponse,
                format!("The Figma response is missing required string field `{key}`."),
            )
        })
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn parse_bounds(value: Option<&Value>) -> Option<BoundingBox> {
    let object = value?.as_object()?;
    Some(BoundingBox {
        x: object.get("x")?.as_f64()?,
        y: object.get("y")?.as_f64()?,
        width: object.get("width")?.as_f64()?,
        height: object.get("height")?.as_f64()?,
    })
}

fn parse_entity_map(value: Option<&Value>, entity_type: &str) -> Vec<IndexedEntity> {
    let Some(map) = value.and_then(Value::as_object) else {
        return Vec::new();
    };
    map.iter()
        .map(|(id, raw)| IndexedEntity {
            id: id.clone(),
            node_id: raw
                .get("node_id")
                .or_else(|| raw.get("nodeId"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            name: raw
                .get("name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            entity_type: entity_type.to_owned(),
            raw: raw.clone(),
        })
        .collect()
}

fn is_known_node_type(node_type: &str) -> bool {
    matches!(
        node_type,
        "DOCUMENT"
            | "CANVAS"
            | "FRAME"
            | "GROUP"
            | "SECTION"
            | "VECTOR"
            | "BOOLEAN_OPERATION"
            | "STAR"
            | "LINE"
            | "ELLIPSE"
            | "REGULAR_POLYGON"
            | "RECTANGLE"
            | "TEXT"
            | "SLICE"
            | "COMPONENT"
            | "COMPONENT_SET"
            | "INSTANCE"
            | "STAMP"
            | "WIDGET"
            | "EMBED"
            | "LINK_UNFURL"
            | "MEDIA"
            | "SHAPE_WITH_TEXT"
            | "CODE_BLOCK"
            | "CONNECTOR"
            | "TABLE"
            | "TABLE_CELL"
            | "HIGHLIGHT"
            | "WASHI_TAPE"
    )
}

#[cfg(test)]
mod tests {
    use super::parse_file;
    use serde_json::json;

    #[test]
    fn parser_is_deterministic_and_retains_unknown_nodes() {
        let fixture = br#"{
          "name":"Example","version":"42","lastModified":"now",
          "document":{"id":"0:0","name":"Doc","type":"DOCUMENT","children":[
            {"id":"1:1","name":"Future","type":"FUTURE_NODE","futureField":true}
          ]},
          "styles":{},"components":{},"componentSets":{}
        }"#;
        let first = parse_file(fixture).unwrap_or_else(|error| panic!("parse failed: {error}"));
        let second = parse_file(fixture).unwrap_or_else(|error| panic!("parse failed: {error}"));
        assert_eq!(first.nodes, second.nodes);
        assert_eq!(first.nodes[1].node_type, "UNKNOWN");
        assert_eq!(first.nodes[1].raw["futureField"], true);
        assert_eq!(first.warnings.len(), 1);
    }

    #[test]
    fn indexes_1k_10k_and_100k_synthetic_node_fixtures() {
        for node_count in [1_000_usize, 10_000, 100_000] {
            let children = (1..node_count)
                .map(|index| {
                    json!({
                        "id": format!("1:{index}"),
                        "name": format!("Node {index}"),
                        "type": "RECTANGLE"
                    })
                })
                .collect::<Vec<_>>();
            let fixture = serde_json::to_vec(&json!({
                "name": "Synthetic Scale Fixture",
                "version": node_count.to_string(),
                "document": {
                    "id": "0:0",
                    "name": "Document",
                    "type": "DOCUMENT",
                    "children": children
                }
            }))
            .unwrap_or_else(|error| panic!("fixture serialization failed: {error}"));
            let first = parse_file(&fixture)
                .unwrap_or_else(|error| panic!("{node_count} node parse failed: {error}"));
            let second = parse_file(&fixture)
                .unwrap_or_else(|error| panic!("{node_count} node reparse failed: {error}"));
            assert_eq!(first.nodes.len(), node_count);
            assert_eq!(first.nodes[0].subtree_hash, second.nodes[0].subtree_hash);
        }
    }
}
