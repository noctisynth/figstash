//! Clap command surface. Parsing errors are adapted by the entry point.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "figstash",
    version,
    about = "Durable local Figma snapshots for agents"
)]
pub(crate) struct Cli {
    #[arg(long, global = true)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub(crate) config_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub(crate) offline: bool,
    #[arg(long, global = true)]
    pub(crate) log_level: Option<LogLevel>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

impl Cli {
    pub(crate) const fn command_name(&self) -> &'static str {
        match &self.command {
            Command::Context(_) => "context",
            Command::Outline(_) => "outline",
            Command::Schema(_) => "schema",
            Command::Auth(AuthCommand { command }) => match command {
                AuthSubcommand::Set(_) => "auth.set",
                AuthSubcommand::Status => "auth.status",
                AuthSubcommand::Whoami => "auth.whoami",
                AuthSubcommand::Clear => "auth.clear",
            },
            Command::Doctor(_) => "doctor",
            Command::Quota(QuotaCommand { command }) => match command {
                QuotaSubcommand::Status => "quota.status",
            },
            Command::Snapshot(SnapshotCommand { command }) => match command {
                SnapshotSubcommand::Pull(_) => "snapshot.pull",
                SnapshotSubcommand::Status(_) => "snapshot.status",
                SnapshotSubcommand::List(_) => "snapshot.list",
                SnapshotSubcommand::Diff(_) => "snapshot.diff",
                SnapshotSubcommand::Prune(_) => "snapshot.prune",
            },
            Command::Node(NodeCommand { command }) => match command {
                NodeSubcommand::Get(_) => "node.get",
                NodeSubcommand::Search(_) => "node.search",
            },
            Command::Tokens(TokensCommand { command }) => match command {
                TokensSubcommand::Get(_) => "tokens.get",
            },
            Command::Components(ComponentsCommand { command }) => match command {
                ComponentsSubcommand::List(_) => "components.list",
            },
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Return self-contained compact design context for one node.
    Context(ContextArgs),
    /// Return a sparse local node tree for target discovery.
    Outline(OutlineArgs),
    /// Inspect machine-readable command contracts.
    Schema(SchemaArgs),
    Auth(AuthCommand),
    Doctor(DoctorArgs),
    Quota(QuotaCommand),
    Snapshot(SnapshotCommand),
    Node(NodeCommand),
    Tokens(TokensCommand),
    Components(ComponentsCommand),
}

#[derive(Debug, Args)]
pub(crate) struct ContextArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) node: Option<String>,
    #[arg(long)]
    pub(crate) depth: Option<u32>,
    #[arg(long)]
    pub(crate) snapshot: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Args)]
pub(crate) struct OutlineArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) node: Option<String>,
    #[arg(long, default_value_t = 2)]
    pub(crate) depth: u32,
    #[arg(long)]
    pub(crate) snapshot: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Args)]
pub(crate) struct SchemaArgs {
    /// Stable command name such as `context` or `snapshot.pull`.
    pub(crate) command: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct AuthCommand {
    #[command(subcommand)]
    pub(crate) command: AuthSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AuthSubcommand {
    Set(AuthSetArgs),
    Status,
    /// Validate the credential and return the current Figma user.
    Whoami,
    Clear,
}

#[derive(Debug, Args)]
pub(crate) struct AuthSetArgs {
    #[arg(long, required = true)]
    pub(crate) stdin: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    #[arg(long)]
    pub(crate) network: bool,
}

#[derive(Debug, Args)]
pub(crate) struct QuotaCommand {
    #[command(subcommand)]
    pub(crate) command: QuotaSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum QuotaSubcommand {
    Status,
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotCommand {
    #[command(subcommand)]
    pub(crate) command: SnapshotSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SnapshotSubcommand {
    Pull(SnapshotPullArgs),
    Status(SnapshotStatusArgs),
    List(SnapshotListArgs),
    Diff(SnapshotDiffArgs),
    Prune(SnapshotPruneArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotPullArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) force: bool,
    #[arg(long)]
    pub(crate) version: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotStatusArgs {
    pub(crate) target: String,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
    #[arg(long)]
    pub(crate) snapshot: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotListArgs {
    #[arg(long)]
    pub(crate) file: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotDiffArgs {
    pub(crate) target: String,
    pub(crate) snapshot_a: String,
    pub(crate) snapshot_b: String,
    #[arg(long)]
    pub(crate) node: Option<String>,
    #[arg(long = "type")]
    pub(crate) node_type: Option<String>,
    #[arg(long)]
    pub(crate) path: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Args)]
pub(crate) struct SnapshotPruneArgs {
    #[arg(long)]
    pub(crate) execute: bool,
}

#[derive(Debug, Args)]
pub(crate) struct NodeCommand {
    #[command(subcommand)]
    pub(crate) command: NodeSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum NodeSubcommand {
    Get(NodeGetArgs),
    Search(NodeSearchArgs),
}

#[derive(Debug, Args)]
pub(crate) struct NodeGetArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) node: Option<String>,
    #[arg(long)]
    pub(crate) depth: Option<u32>,
    #[arg(long, value_enum, default_value_t = ViewArg::Compact)]
    pub(crate) view: ViewArg,
    #[arg(long)]
    pub(crate) snapshot: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Args)]
pub(crate) struct NodeSearchArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) name: Option<String>,
    #[arg(long)]
    pub(crate) text: Option<String>,
    #[arg(long = "type")]
    pub(crate) node_type: Option<String>,
    #[arg(long)]
    pub(crate) ancestor: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: u32,
    #[arg(long)]
    pub(crate) cursor: Option<String>,
    #[arg(long)]
    pub(crate) snapshot: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Args)]
pub(crate) struct TokensCommand {
    #[command(subcommand)]
    pub(crate) command: TokensSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TokensSubcommand {
    Get(LocalSnapshotArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ComponentsCommand {
    #[command(subcommand)]
    pub(crate) command: ComponentsSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ComponentsSubcommand {
    List(LocalSnapshotArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LocalSnapshotArgs {
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) snapshot: Option<String>,
    #[arg(long, value_enum, default_value_t = GeometryArg::None)]
    pub(crate) geometry: GeometryArg,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum GeometryArg {
    None,
    Paths,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ViewArg {
    Compact,
    Raw,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}
