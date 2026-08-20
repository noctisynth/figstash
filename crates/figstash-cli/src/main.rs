//! Figstash command-line entry point.

mod app;
mod args;
mod config;
mod output;
mod schema_catalog;

use clap::{Parser, error::ErrorKind};
use figstash_core::{AppError, ErrorCode, ExitCode};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn main() {
    let status = catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| {
        let error = AppError::new(
            ErrorCode::Internal,
            "Figstash stopped because of an unexpected internal error.",
        );
        output::emit_failure("internal", &error, None);
        ExitCode::Internal.as_i32()
    });
    if status != ExitCode::Success.as_i32() {
        std::process::exit(status);
    }
}

fn run() -> i32 {
    let cli = match args::Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            output::emit_help(&error.to_string());
            return ExitCode::Success.as_i32();
        }
        Err(error) => {
            let app_error = AppError::new(ErrorCode::InvalidArguments, error.to_string());
            output::emit_failure("cli.parse", &app_error, None);
            return app_error.exit_code().as_i32();
        }
    };
    let command = cli.command_name();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let app_error = AppError::new(
                ErrorCode::Internal,
                "Failed to initialize the asynchronous runtime.",
            )
            .with_detail("reason", serde_json::json!(error.to_string()));
            output::emit_failure(command, &app_error, None);
            return app_error.exit_code().as_i32();
        }
    };
    runtime.block_on(app::run(cli))
}
