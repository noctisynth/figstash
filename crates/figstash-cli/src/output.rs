//! Single-object stdout emission and stable failure adaptation.

use figstash_core::{AppError, Meta, NetworkUsage, ResponseEnvelope, ResponseSource, Warning};
use serde_json::{Value, json};
use std::io::Write;
use std::time::Instant;
use uuid::Uuid;

pub(crate) struct CommandOutput {
    pub(crate) data: Value,
    pub(crate) source: ResponseSource,
    pub(crate) snapshot_id: Option<String>,
    pub(crate) figma_version: Option<String>,
    pub(crate) network: NetworkUsage,
    pub(crate) warnings: Vec<Warning>,
}

impl CommandOutput {
    pub(crate) fn local(data: impl serde::Serialize) -> Result<Self, serde_json::Error> {
        Ok(Self {
            data: serde_json::to_value(data)?,
            source: ResponseSource::Cache,
            snapshot_id: None,
            figma_version: None,
            network: NetworkUsage::default(),
            warnings: Vec::new(),
        })
    }

    pub(crate) fn neutral(data: impl serde::Serialize) -> Result<Self, serde_json::Error> {
        Ok(Self {
            data: serde_json::to_value(data)?,
            source: ResponseSource::None,
            snapshot_id: None,
            figma_version: None,
            network: NetworkUsage::default(),
            warnings: Vec::new(),
        })
    }
}

pub(crate) fn emit_success(command: &str, output: CommandOutput, started: Instant) {
    let mut meta = Meta::new(command, output.source, trace_id());
    meta.snapshot_id = output.snapshot_id;
    meta.figma_version = output.figma_version;
    meta.network = output.network;
    meta.duration_ms = Some(duration_millis(started));
    emit(&ResponseEnvelope::success(
        output.data,
        meta,
        output.warnings,
    ));
}

pub(crate) fn emit_failure(command: &str, error: &AppError, started: Option<Instant>) {
    let network = error
        .details()
        .get("network")
        .cloned()
        .and_then(|value| serde_json::from_value::<NetworkUsage>(value).ok())
        .unwrap_or_default();
    let source = if network.attempts > 0 {
        ResponseSource::Figma
    } else {
        ResponseSource::None
    };
    let mut meta = Meta::new(command, source, trace_id());
    meta.network = network;
    meta.duration_ms = started.map(duration_millis);
    emit(&ResponseEnvelope::<Value>::failure(error, meta, Vec::new()));
}

pub(crate) fn emit_help(help: &str) {
    let output =
        CommandOutput::neutral(json!({"help": help})).unwrap_or_else(|error| CommandOutput {
            data: json!({"serializationError": error.to_string()}),
            source: ResponseSource::None,
            snapshot_id: None,
            figma_version: None,
            network: NetworkUsage::default(),
            warnings: Vec::new(),
        });
    emit_success("cli.help", output, Instant::now());
}

fn emit<T: serde::Serialize>(response: &ResponseEnvelope<T>) {
    let serialized = serde_json::to_string(response).unwrap_or_else(|_| {
        "{\"schemaVersion\":1,\"ok\":false,\"error\":{\"code\":\"internal_error\",\"message\":\"Failed to serialize CLI output.\",\"details\":{},\"retryable\":false},\"meta\":{\"command\":\"internal\",\"source\":\"none\",\"network\":{\"attempts\":0,\"tier1\":0,\"tier2\":0,\"tier3\":0},\"traceId\":\"serialization-fallback\"},\"warnings\":[]}".to_owned()
    });
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(serialized.as_bytes());
    let _ = stdout.write_all(b"\n");
}

fn trace_id() -> String {
    Uuid::new_v4().to_string()
}

fn duration_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
