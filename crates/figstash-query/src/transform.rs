//! Deterministic Agent-oriented context transformations.

use figstash_core::{AppError, AppResult, ErrorCode, StoredNode};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Compact representation of one Figma node.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactNode {
    /// Node identifier.
    pub id: String,
    /// Node name.
    pub name: String,
    /// Normalized node type.
    pub node_type: String,
    /// Effective visibility when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Depth in the complete file tree.
    pub depth: u32,
    /// Root-to-node identifier path.
    pub path_ids: Vec<String>,
    /// Absolute bounding box when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<figstash_core::BoundingBox>,
    /// Layout and constraint fields.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub layout: Map<String, Value>,
    /// Paint, stroke, effect, opacity, and radius fields.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub appearance: Map<String, Value>,
    /// Text and typography fields.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub text: Map<String, Value>,
    /// Component and instance fields.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub component: Map<String, Value>,
    /// Style references keyed by Figma style property.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub styles: Map<String, Value>,
    /// Image references discovered in paints.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub image_references: Vec<String>,
    /// Figma export settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export_settings: Option<Value>,
    /// Ordered descendants selected by the requested depth.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CompactNode>,
}

/// Reference to a repeated value in a node field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenReference {
    /// Referencing node identifier.
    pub node_id: String,
    /// Raw Figma field name.
    pub field: String,
}

/// A deterministic value derived from repeated node properties.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedVariable {
    /// Stable content-derived identifier.
    pub id: String,
    /// Broad value category.
    pub category: String,
    /// Original normalized JSON value.
    pub value: Value,
    /// Number of references in the selected snapshot.
    pub occurrences: usize,
    /// Referencing nodes and fields.
    pub references: Vec<TokenReference>,
}

/// Converts a stored node and already transformed children to compact context.
pub fn compact_node(node: &StoredNode, children: Vec<CompactNode>) -> CompactNode {
    let raw = node.index.raw.as_object();
    CompactNode {
        id: node.index.node_id.clone(),
        name: node.index.name.clone(),
        node_type: node.index.node_type.clone(),
        visible: node.index.visible,
        depth: node.index.depth,
        path_ids: node.index.path_ids.clone(),
        bounds: node.index.bounds,
        layout: select_fields(raw, LAYOUT_FIELDS),
        appearance: select_fields(raw, APPEARANCE_FIELDS),
        text: select_fields(raw, TEXT_FIELDS),
        component: select_fields(raw, COMPONENT_FIELDS),
        styles: raw
            .and_then(|object| object.get("styles"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        image_references: find_image_references(raw),
        export_settings: raw.and_then(|object| object.get("exportSettings")).cloned(),
        children,
    }
}

/// Reconstructs raw node JSON from independent node blobs.
pub fn raw_node(node: &StoredNode, children: Vec<Value>) -> Value {
    let mut raw = node.index.raw.clone();
    if !children.is_empty() {
        if let Some(object) = raw.as_object_mut() {
            object.insert("children".to_owned(), Value::Array(children));
        }
    }
    raw
}

/// Derives repeated values suitable for compact Agent context.
pub fn derive_variables(nodes: &[StoredNode]) -> AppResult<Vec<DerivedVariable>> {
    let mut occurrences: BTreeMap<(String, String), (Value, Vec<TokenReference>)> = BTreeMap::new();
    for node in nodes {
        let Some(raw) = node.index.raw.as_object() else {
            continue;
        };
        for field in TOKEN_FIELDS {
            let Some(value) = raw.get(*field) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            let canonical = serde_json::to_string(value).map_err(|error| {
                AppError::new(
                    ErrorCode::Internal,
                    "Failed to canonicalize a derived variable value.",
                )
                .with_details(json!({"reason": error.to_string()}))
            })?;
            let category = token_category(field).to_owned();
            let entry = occurrences
                .entry((category, canonical))
                .or_insert_with(|| (value.clone(), Vec::new()));
            entry.1.push(TokenReference {
                node_id: node.index.node_id.clone(),
                field: (*field).to_owned(),
            });
        }
    }

    Ok(occurrences
        .into_iter()
        .filter(|(_, (_, references))| references.len() > 1)
        .map(|((category, canonical), (value, references))| {
            let hash = blake3::hash(format!("{category}\0{canonical}").as_bytes())
                .to_hex()
                .to_string();
            DerivedVariable {
                id: format!("gv_{}", &hash[..16]),
                category,
                value,
                occurrences: references.len(),
                references,
            }
        })
        .collect())
}

fn select_fields(raw: Option<&Map<String, Value>>, fields: &[&str]) -> Map<String, Value> {
    let mut selected = Map::new();
    if let Some(object) = raw {
        for field in fields {
            if let Some(value) = object.get(*field) {
                selected.insert((*field).to_owned(), value.clone());
            }
        }
    }
    selected
}

fn find_image_references(raw: Option<&Map<String, Value>>) -> Vec<String> {
    let mut references = Vec::new();
    for field in ["fills", "strokes"] {
        let Some(paints) = raw
            .and_then(|object| object.get(field))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for paint in paints {
            if let Some(reference) = paint.get("imageRef").and_then(Value::as_str) {
                if !references.iter().any(|known| known == reference) {
                    references.push(reference.to_owned());
                }
            }
        }
    }
    references
}

fn token_category(field: &str) -> &'static str {
    match field {
        "fills" | "strokes" | "backgroundColor" => "color_or_paint",
        "style" | "fontName" | "fontSize" | "fontWeight" | "lineHeightPx" | "letterSpacing" => {
            "typography"
        }
        "cornerRadius" | "rectangleCornerRadii" => "radius",
        "effects" => "effect",
        _ => "spacing_or_layout",
    }
}

const LAYOUT_FIELDS: &[&str] = &[
    "relativeTransform",
    "size",
    "constraints",
    "layoutAlign",
    "layoutGrow",
    "layoutMode",
    "layoutWrap",
    "primaryAxisSizingMode",
    "counterAxisSizingMode",
    "primaryAxisAlignItems",
    "counterAxisAlignItems",
    "paddingLeft",
    "paddingRight",
    "paddingTop",
    "paddingBottom",
    "itemSpacing",
    "counterAxisSpacing",
    "clipsContent",
];
const APPEARANCE_FIELDS: &[&str] = &[
    "fills",
    "strokes",
    "strokeWeight",
    "strokeAlign",
    "strokeCap",
    "strokeJoin",
    "dashPattern",
    "effects",
    "opacity",
    "blendMode",
    "cornerRadius",
    "rectangleCornerRadii",
    "backgroundColor",
];
const TEXT_FIELDS: &[&str] = &[
    "characters",
    "style",
    "characterStyleOverrides",
    "styleOverrideTable",
    "lineTypes",
    "lineIndentations",
];
const COMPONENT_FIELDS: &[&str] = &[
    "componentId",
    "componentProperties",
    "overrides",
    "isExposedInstance",
    "exposedInstances",
];
const TOKEN_FIELDS: &[&str] = &[
    "fills",
    "strokes",
    "backgroundColor",
    "effects",
    "style",
    "fontName",
    "fontSize",
    "fontWeight",
    "lineHeightPx",
    "letterSpacing",
    "cornerRadius",
    "rectangleCornerRadii",
    "itemSpacing",
    "paddingLeft",
    "paddingRight",
    "paddingTop",
    "paddingBottom",
];
