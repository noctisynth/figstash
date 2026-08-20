//! Shared Figma URL, file key, branch key, and node identifier parsing.

use figstash_core::{AppError, AppResult, ErrorCode};
use percent_encoding::percent_decode_str;
use serde::Serialize;
use serde_json::json;
use url::Url;

/// Canonical identifiers parsed from a Figma URL or direct file key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FigmaTarget {
    /// Parent file key from the URL.
    pub file_key: String,
    /// Branch key when the URL names a branch.
    pub branch_key: Option<String>,
    /// Canonical REST node ID when supplied in the URL.
    pub node_id: Option<String>,
}

impl FigmaTarget {
    /// Returns the key used for Figma REST requests and local snapshot lineages.
    #[must_use]
    pub fn effective_file_key(&self) -> &str {
        self.branch_key.as_deref().unwrap_or(&self.file_key)
    }
}

/// Parses a supported Figma URL or a direct file/branch key.
///
/// # Errors
///
/// Returns `invalid_input` for an unsupported host, path, key, encoding, or node ID.
pub fn parse_figma_target(input: &str) -> AppResult<FigmaTarget> {
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid_target(input, "The Figma target cannot be empty."));
    }
    if !input.contains("://") {
        validate_file_key(input)?;
        return Ok(FigmaTarget {
            file_key: input.to_owned(),
            branch_key: None,
            node_id: None,
        });
    }

    let url = Url::parse(input).map_err(|error| {
        invalid_target(input, "The Figma URL is invalid.")
            .with_detail("reason", json!(error.to_string()))
    })?;
    if url.scheme() != "https" {
        return Err(invalid_target(input, "Figma URLs must use HTTPS."));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "figma.com" && host != "www.figma.com" {
        return Err(invalid_target(input, "The URL host is not figma.com."));
    }

    let encoded_segments = url
        .path_segments()
        .map(Iterator::collect::<Vec<_>>)
        .unwrap_or_default();
    let mut segments = Vec::with_capacity(encoded_segments.len());
    for segment in encoded_segments {
        let decoded = percent_decode_str(segment).decode_utf8().map_err(|error| {
            invalid_target(input, "A Figma URL path segment is not valid UTF-8.")
                .with_detail("reason", json!(error.to_string()))
        })?;
        segments.push(decoded.into_owned());
    }
    let (file_key, branch_key) = parse_path(&segments, input)?;
    validate_file_key(&file_key)?;
    if let Some(branch) = &branch_key {
        validate_file_key(branch)?;
    }
    let node_id = url
        .query_pairs()
        .find(|(key, _)| key == "node-id")
        .map(|(_, value)| normalize_node_id(&value))
        .transpose()?;
    Ok(FigmaTarget {
        file_key,
        branch_key,
        node_id,
    })
}

/// Converts URL-style hyphen separators to the canonical REST colon form.
///
/// # Errors
///
/// Returns `invalid_input` when the node identifier is not numeric Figma syntax.
pub fn normalize_node_id(input: &str) -> AppResult<String> {
    let trimmed = input.trim();
    let mut normalized_parts = Vec::new();
    for part in trimmed.split(';') {
        let part = part.strip_prefix('I').unwrap_or(part);
        let separator = if part.contains(':') { ':' } else { '-' };
        let Some((left, right)) = part.split_once(separator) else {
            return Err(invalid_node(input));
        };
        if left.is_empty()
            || right.is_empty()
            || !left.bytes().all(|byte| byte.is_ascii_digit())
            || !right.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid_node(input));
        }
        normalized_parts.push(format!("{left}:{right}"));
    }
    if normalized_parts.is_empty() {
        return Err(invalid_node(input));
    }
    Ok(normalized_parts.join(";"))
}

fn parse_path(segments: &[String], input: &str) -> AppResult<(String, Option<String>)> {
    let Some(kind) = segments.first().map(String::as_str) else {
        return Err(invalid_target(input, "The Figma URL has no file path."));
    };
    match kind {
        "design" | "file" | "board" | "figjam" | "proto" => {
            let file_key = segments
                .get(1)
                .ok_or_else(|| invalid_target(input, "The Figma URL is missing its file key."))?;
            let branch_key = segments
                .iter()
                .position(|segment| segment == "branch")
                .and_then(|index| segments.get(index + 1))
                .cloned();
            Ok((file_key.clone(), branch_key))
        }
        "branch" => {
            let branch_key = segments.get(1).ok_or_else(|| {
                invalid_target(input, "The Figma branch URL is missing its branch key.")
            })?;
            Ok((branch_key.clone(), Some(branch_key.clone())))
        }
        _ => Err(invalid_target(
            input,
            "The Figma URL path is not a supported design, file, FigJam, proto, or branch URL.",
        )),
    }
}

fn validate_file_key(key: &str) -> AppResult<()> {
    if (6..=128).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::InvalidInput,
            "The Figma file or branch key has invalid syntax.",
        )
        .with_details(json!({"fileKey": key})))
    }
}

fn invalid_target(input: &str, message: &'static str) -> AppError {
    AppError::new(ErrorCode::InvalidInput, message).with_details(json!({"input": input}))
}

fn invalid_node(input: &str) -> AppError {
    AppError::new(
        ErrorCode::InvalidInput,
        "The Figma node ID must use numeric `1234:5678` or `1234-5678` syntax.",
    )
    .with_details(json!({"nodeId": input}))
}

#[cfg(test)]
mod tests {
    use super::{normalize_node_id, parse_figma_target};

    #[test]
    fn parses_supported_urls_and_normalizes_node_ids() {
        let design =
            parse_figma_target("https://www.figma.com/design/Abcdef123/File?node-id=1234-5678")
                .unwrap_or_else(|error| panic!("design URL failed: {error}"));
        assert_eq!(design.file_key, "Abcdef123");
        assert_eq!(design.node_id.as_deref(), Some("1234:5678"));

        let branch = parse_figma_target(
            "https://figma.com/file/Abcdef123/name/branch/Branch987?node-id=I1%3A2%3B3-4",
        )
        .unwrap_or_else(|error| panic!("branch URL failed: {error}"));
        assert_eq!(branch.effective_file_key(), "Branch987");
        assert_eq!(branch.node_id.as_deref(), Some("1:2;3:4"));
        assert_eq!(
            normalize_node_id("1-2")
                .unwrap_or_else(|error| panic!("node normalization failed: {error}")),
            "1:2"
        );
    }

    #[test]
    fn rejects_foreign_hosts_and_invalid_keys() {
        assert!(parse_figma_target("https://example.com/design/Abcdef123/x").is_err());
        assert!(parse_figma_target("short").is_err());
    }
}
