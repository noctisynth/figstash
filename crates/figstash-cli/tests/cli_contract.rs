//! Offline CLI, JSON Schema, stdout purity, and repeated-query acceptance tests.

use figstash_core::{RequestProfile, SnapshotSelector};
use figstash_query::{NodeGetOptions, QueryService, View, parse_file};
use figstash_store::Store;
use serde_json::Value;
use std::fs;
use std::process::{Command, Output};

const FILE_KEY: &str = "SyntheticFileKey123";

#[test]
fn packaged_cli_schemas_match_the_authoritative_contracts() {
    let package_schemas = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join("cli")
        .join("v1");
    let authoritative_schemas = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
        .join("cli")
        .join("v1");

    for entry in fs::read_dir(&authoritative_schemas)
        .unwrap_or_else(|error| panic!("authoritative schema directory is unreadable: {error}"))
    {
        let entry = entry.unwrap_or_else(|error| panic!("schema entry is unreadable: {error}"));
        let file_type = entry
            .file_type()
            .unwrap_or_else(|error| panic!("schema entry type is unreadable: {error}"));
        if !file_type.is_file() {
            continue;
        }
        let expected = fs::read(entry.path())
            .unwrap_or_else(|error| panic!("authoritative schema is unreadable: {error}"));
        let packaged_path = package_schemas.join(entry.file_name());
        let actual = fs::read(&packaged_path).unwrap_or_else(|error| {
            panic!(
                "packaged schema {} is unreadable: {error}",
                packaged_path.display()
            )
        });
        assert_eq!(
            actual,
            expected,
            "packaged schema {} drifted",
            packaged_path.display()
        );
    }
}

fn prepared_store() -> (tempfile::TempDir, Store, figstash_core::SnapshotSummary) {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let store = Store::open(temporary.path())
        .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
    let bytes = include_bytes!("../../../fixtures/figma/feature-parity.json");
    let indexed = parse_file(bytes).unwrap_or_else(|error| panic!("fixture parse failed: {error}"));
    let stage = store
        .create_staging_file()
        .unwrap_or_else(|error| panic!("staging allocation failed: {error}"));
    fs::write(&stage, bytes).unwrap_or_else(|error| panic!("fixture staging failed: {error}"));
    let summary = store
        .commit_snapshot(FILE_KEY, RequestProfile::default().key(), &stage, &indexed)
        .unwrap_or_else(|error| panic!("snapshot commit failed: {error}"));
    (temporary, store, summary)
}

#[test]
fn every_core_local_command_emits_one_schema_valid_json_object() {
    let (temporary, _store, _summary) = prepared_store();
    let cases: &[(&str, &[&str])] = &[
        (
            "context",
            &["context", FILE_KEY, "--node", "2:1", "--depth", "1"],
        ),
        ("outline", &["outline", FILE_KEY]),
        ("schema", &["schema", "context"]),
        (
            "node.get",
            &["node", "get", FILE_KEY, "--node", "2:1", "--depth", "1"],
        ),
        (
            "node.search",
            &["node", "search", FILE_KEY, "--text", "Hello"],
        ),
        ("tokens.get", &["tokens", "get", FILE_KEY]),
        ("components.list", &["components", "list", FILE_KEY]),
        ("snapshot.status", &["snapshot", "status", FILE_KEY]),
        ("snapshot.list", &["snapshot", "list"]),
        ("snapshot.prune", &["snapshot", "prune"]),
        ("quota.status", &["quota", "status"]),
        ("doctor", &["doctor"]),
    ];
    for (command, arguments) in cases {
        let output = run_cli(temporary.path(), arguments);
        assert!(
            output.status.success(),
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response = parse_single_stdout(&output);
        validate_schema(
            include_str!("../../../schemas/cli/v1/envelope.schema.json"),
            &response,
        );
        assert_eq!(response["meta"]["command"], *command);
        assert_eq!(response["meta"]["network"]["attempts"], 0);
        validate_schema(schema_for(command), &response["data"]);
    }
}

#[test]
fn high_level_agent_commands_are_local_safe_and_unambiguous() {
    let (temporary, _store, _summary) = prepared_store();

    let context = run_cli(
        temporary.path(),
        &[
            "context",
            "https://www.figma.com/design/SyntheticFileKey123/Name?node-id=2-1",
        ],
    );
    assert!(context.status.success());
    let context = parse_single_stdout(&context);
    assert_eq!(context["data"]["node"]["id"], "2:1");
    assert_eq!(context["meta"]["network"]["attempts"], 0);

    let outline = run_cli(temporary.path(), &["outline", FILE_KEY]);
    assert!(outline.status.success());
    let outline = parse_single_stdout(&outline);
    assert_eq!(outline["data"]["root"]["id"], "0:0");
    assert_eq!(
        outline["data"]["root"]["children"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        outline["data"]["root"]["children"][0]["children"]
            .as_array()
            .map(Vec::len),
        Some(4)
    );
    assert_eq!(outline["meta"]["network"]["attempts"], 0);

    let offline_context = run_cli(
        temporary.path(),
        &["--offline", "context", FILE_KEY, "--node", "2:1"],
    );
    assert!(offline_context.status.success());
    assert_eq!(
        parse_single_stdout(&offline_context)["meta"]["network"]["attempts"],
        0
    );

    let offline_outline = run_cli(temporary.path(), &["--offline", "outline", FILE_KEY]);
    assert!(offline_outline.status.success());
    assert_eq!(
        parse_single_stdout(&offline_outline)["meta"]["network"]["attempts"],
        0
    );

    let missing_node = run_cli(temporary.path(), &["context", FILE_KEY]);
    assert_eq!(missing_node.status.code(), Some(2));
    let missing_node = parse_single_stdout(&missing_node);
    assert_eq!(missing_node["error"]["code"], "node_required");
    assert_eq!(missing_node["meta"]["network"]["attempts"], 0);
    assert_eq!(
        missing_node["error"]["details"]["suggestedCommand"],
        format!("figstash outline {FILE_KEY}")
    );

    let conflict = run_cli(
        temporary.path(),
        &[
            "context",
            "https://www.figma.com/design/SyntheticFileKey123/Name?node-id=2-1",
            "--node",
            "2:2",
        ],
    );
    assert_eq!(conflict.status.code(), Some(2));
    let conflict = parse_single_stdout(&conflict);
    assert_eq!(conflict["error"]["code"], "invalid_arguments");
    assert_eq!(conflict["meta"]["network"]["attempts"], 0);
}

#[test]
fn schema_command_supports_catalog_and_detailed_contract_discovery() {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let catalog = run_cli(temporary.path(), &["schema"]);
    assert!(catalog.status.success());
    let catalog = parse_single_stdout(&catalog);
    assert!(
        catalog["data"]["commands"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|command| command["name"] == "context"))
    );
    validate_schema(
        include_str!("../../../schemas/cli/v1/schema.schema.json"),
        &catalog["data"],
    );

    let detail = run_cli(temporary.path(), &["schema", "snapshot.pull"]);
    assert!(detail.status.success());
    let detail = parse_single_stdout(&detail);
    assert_eq!(detail["data"]["network"], "explicit");
    assert_eq!(detail["data"]["endpointClass"], "get_file");
    assert_eq!(detail["data"]["tier"], 1);
    assert_eq!(detail["meta"]["network"]["attempts"], 0);

    let whoami = run_cli(temporary.path(), &["schema", "auth.whoami"]);
    assert!(whoami.status.success());
    let whoami = parse_single_stdout(&whoami);
    assert_eq!(whoami["data"]["network"], "explicit");
    assert_eq!(whoami["data"]["endpointClass"], "get_current_user");
    assert_eq!(whoami["data"]["tier"], 3);
    assert_eq!(whoami["data"]["writesLocalState"], true);
    assert_eq!(whoami["meta"]["network"]["attempts"], 0);

    let unknown = run_cli(temporary.path(), &["schema", "missing.command"]);
    assert_eq!(unknown.status.code(), Some(2));
    let unknown = parse_single_stdout(&unknown);
    assert_eq!(unknown["error"]["code"], "invalid_arguments");
}

#[test]
fn compact_node_is_a_self_contained_agent_d2c_context() {
    let (temporary, _store, _summary) = prepared_store();
    let output = run_cli(
        temporary.path(),
        &["node", "get", FILE_KEY, "--node", "2:1"],
    );
    assert!(output.status.success());
    let response = parse_single_stdout(&output);
    assert_eq!(response["meta"]["source"], "cache");
    assert_eq!(response["meta"]["network"]["attempts"], 0);
    assert_eq!(response["data"]["node"]["id"], "2:1");
    assert_eq!(
        response["data"]["references"]["namedStyles"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    assert!(
        response["data"]["references"]["globalVars"]
            .as_array()
            .is_some_and(|variables| !variables.is_empty())
    );
    validate_schema(
        include_str!("../../../schemas/cli/v1/node.get.schema.json"),
        &response["data"],
    );
}

#[test]
fn argument_and_cache_miss_failures_are_json_with_stable_exit_codes() {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let parse_error = run_cli(temporary.path(), &["node", "get"]);
    assert_eq!(parse_error.status.code(), Some(2));
    let parse_json = parse_single_stdout(&parse_error);
    assert_eq!(parse_json["error"]["code"], "invalid_arguments");

    let cache_miss = run_cli(temporary.path(), &["node", "get", "AnotherValidFileKey123"]);
    assert_eq!(cache_miss.status.code(), Some(4));
    let miss_json = parse_single_stdout(&cache_miss);
    assert_eq!(miss_json["error"]["code"], "snapshot_missing");
    assert_eq!(miss_json["meta"]["network"]["attempts"], 0);
    validate_schema(
        include_str!("../../../schemas/cli/v1/envelope.schema.json"),
        &miss_json,
    );
}

#[test]
fn one_thousand_repeated_local_queries_observe_zero_network_attempts() {
    let (_temporary, store, summary) = prepared_store();
    let service = QueryService::new(store.clone());
    for _ in 0..1_000 {
        let result = service
            .node_get(NodeGetOptions {
                selector: SnapshotSelector {
                    file_key: FILE_KEY,
                    request_profile: RequestProfile::default().key(),
                    snapshot_id: Some(&summary.id),
                },
                node_id: Some("2:2"),
                depth: Some(0),
                view: View::Compact,
            })
            .unwrap_or_else(|error| panic!("offline query failed: {error}"));
        assert_eq!(result.node["id"], "2:2");
    }
    let quota = store
        .quota_status()
        .unwrap_or_else(|error| panic!("quota status failed: {error}"));
    assert_eq!(quota.attempted, 0);
}

#[test]
fn cli_environment_file_and_default_configuration_precedence_is_observable() {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let config_dir = temporary.path().join("config");
    fs::create_dir_all(&config_dir)
        .unwrap_or_else(|error| panic!("config directory failed: {error}"));
    let file_data = temporary.path().join("file-data");
    fs::write(
        config_dir.join("config.toml"),
        format!("data_dir = {file_data:?}\nlog_level = \"off\"\n"),
    )
    .unwrap_or_else(|error| panic!("config fixture failed: {error}"));

    let file_output = Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--config-dir")
        .arg(&config_dir)
        .args(["doctor"])
        .env_remove("FIGSTASH_DATA_DIR")
        .output()
        .unwrap_or_else(|error| panic!("file config process failed: {error}"));
    let file_json = parse_single_stdout(&file_output);
    assert_eq!(
        file_json["data"]["checks"][0]["path"],
        file_data.join("store-v1").to_string_lossy().as_ref()
    );

    let environment_data = temporary.path().join("environment-data");
    let environment_output = Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--config-dir")
        .arg(&config_dir)
        .args(["doctor"])
        .env("FIGSTASH_DATA_DIR", &environment_data)
        .output()
        .unwrap_or_else(|error| panic!("environment config process failed: {error}"));
    let environment_json = parse_single_stdout(&environment_output);
    assert_eq!(
        environment_json["data"]["checks"][0]["path"],
        environment_data.join("store-v1").to_string_lossy().as_ref()
    );

    let cli_data = temporary.path().join("cli-data");
    let cli_output = Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--config-dir")
        .arg(&config_dir)
        .arg("--data-dir")
        .arg(&cli_data)
        .args(["doctor"])
        .env("FIGSTASH_DATA_DIR", &environment_data)
        .output()
        .unwrap_or_else(|error| panic!("CLI config process failed: {error}"));
    let cli_json = parse_single_stdout(&cli_output);
    assert_eq!(
        cli_json["data"]["checks"][0]["path"],
        cli_data.join("store-v1").to_string_lossy().as_ref()
    );
}

#[test]
fn environment_token_is_reported_but_never_echoed() {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let token = "synthetic-sensitive-value-must-never-appear";
    let output = Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--data-dir")
        .arg(temporary.path())
        .arg("--config-dir")
        .arg(temporary.path().join("config"))
        .arg("--log-level")
        .arg("off")
        .args(["auth", "status"])
        .env("FIGMA_TOKEN", token)
        .output()
        .unwrap_or_else(|error| panic!("auth status process failed: {error}"));
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
    let response = parse_single_stdout(&output);
    assert_eq!(response["data"]["credentialKind"], "pat");
    assert_eq!(response["data"]["source"], "environment");
    validate_schema(
        include_str!("../../../schemas/cli/v1/auth.status.schema.json"),
        &response["data"],
    );
}

#[test]
fn offline_whoami_fails_before_credential_or_network_access() {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let output = Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--data-dir")
        .arg(temporary.path())
        .arg("--config-dir")
        .arg(temporary.path().join("config"))
        .arg("--log-level")
        .arg("off")
        .arg("--offline")
        .args(["auth", "whoami"])
        .env_remove("FIGMA_TOKEN")
        .output()
        .unwrap_or_else(|error| panic!("offline whoami process failed: {error}"));
    assert_eq!(output.status.code(), Some(6));
    let response = parse_single_stdout(&output);
    assert_eq!(response["error"]["code"], "offline_mode");
    assert_eq!(response["meta"]["command"], "auth.whoami");
    assert_eq!(response["meta"]["network"]["attempts"], 0);
}

#[test]
fn pull_policy_fails_before_auth_or_network_when_refresh_is_not_explicit() {
    let (temporary, _store, _summary) = prepared_store();
    let existing = run_cli(temporary.path(), &["snapshot", "pull", FILE_KEY]);
    assert_eq!(existing.status.code(), Some(6));
    let existing_json = parse_single_stdout(&existing);
    assert_eq!(existing_json["error"]["code"], "metadata_unavailable");
    assert_eq!(existing_json["meta"]["network"]["attempts"], 0);

    let empty =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let offline = Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--data-dir")
        .arg(empty.path())
        .arg("--config-dir")
        .arg(empty.path().join("config"))
        .arg("--offline")
        .args(["snapshot", "pull", FILE_KEY])
        .env_remove("FIGMA_TOKEN")
        .output()
        .unwrap_or_else(|error| panic!("offline pull process failed: {error}"));
    assert_eq!(offline.status.code(), Some(6));
    let offline_json = parse_single_stdout(&offline);
    assert_eq!(offline_json["error"]["code"], "offline_mode");
    assert_eq!(offline_json["meta"]["network"]["attempts"], 0);
}

fn run_cli(data_dir: &std::path::Path, arguments: &[&str]) -> Output {
    let config_dir = data_dir.join("test-config");
    Command::new(env!("CARGO_BIN_EXE_figstash"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--log-level")
        .arg("off")
        .args(arguments)
        .env_remove("FIGMA_TOKEN")
        .output()
        .unwrap_or_else(|error| panic!("figstash process failed: {error}"))
}

fn parse_single_stdout(output: &Output) -> Value {
    assert!(output.stdout.ends_with(b"\n"));
    assert_eq!(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        1,
        "stdout must contain exactly one JSON line: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn validate_schema(schema: &str, instance: &Value) {
    let schema: Value = serde_json::from_str(schema)
        .unwrap_or_else(|error| panic!("schema is invalid JSON: {error}"));
    let validator = jsonschema::validator_for(&schema)
        .unwrap_or_else(|error| panic!("schema compilation failed: {error}"));
    if let Err(error) = validator.validate(instance) {
        panic!("schema validation failed: {error}; instance={instance}");
    }
}

fn schema_for(command: &str) -> &'static str {
    match command {
        "context" => include_str!("../../../schemas/cli/v1/context.schema.json"),
        "outline" => include_str!("../../../schemas/cli/v1/outline.schema.json"),
        "schema" => include_str!("../../../schemas/cli/v1/schema.schema.json"),
        "node.get" => include_str!("../../../schemas/cli/v1/node.get.schema.json"),
        "node.search" => include_str!("../../../schemas/cli/v1/node.search.schema.json"),
        "tokens.get" => include_str!("../../../schemas/cli/v1/tokens.get.schema.json"),
        "components.list" => {
            include_str!("../../../schemas/cli/v1/components.list.schema.json")
        }
        "snapshot.status" => {
            include_str!("../../../schemas/cli/v1/snapshot.status.schema.json")
        }
        "snapshot.list" => {
            include_str!("../../../schemas/cli/v1/snapshot.list.schema.json")
        }
        "snapshot.prune" => {
            include_str!("../../../schemas/cli/v1/snapshot.prune.schema.json")
        }
        "quota.status" => include_str!("../../../schemas/cli/v1/quota.status.schema.json"),
        "doctor" => include_str!("../../../schemas/cli/v1/doctor.schema.json"),
        _ => panic!("missing command schema for {command}"),
    }
}
