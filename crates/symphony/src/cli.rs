use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::{config::RootConfig, daemon, storage::SqliteStore};

#[derive(Debug, Parser)]
#[command(name = "symphony", about = "Rust-first Symphony runtime")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    ValidateConfig {
        #[arg(long)]
        config: PathBuf,
    },
    InitStore {
        #[arg(long)]
        database: PathBuf,
    },
    Daemon {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        once: bool,
    },
    RecordAcceptanceSelfDefect {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        database: PathBuf,
        #[arg(long, default_value = "symphony")]
        source_project: String,
        #[arg(long)]
        source_issue: String,
        #[arg(long)]
        session_id: String,
        #[arg(long, default_value = "live_acceptance_related_only")]
        fingerprint: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        process_id: Option<u32>,
    },
}

pub async fn run() -> anyhow::Result<()> {
    let args = std::env::args_os().collect::<Vec<_>>();
    run_with_args(args).await
}

pub async fn run_with_args<I, T>(args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    match cli.command {
        Command::ValidateConfig { config } => {
            let input = tokio::fs::read_to_string(&config)
                .await
                .with_context(|| format!("read config {}", config.display()))?;
            RootConfig::from_toml_str(&input)?;
            Ok(())
        }
        Command::InitStore { database } => {
            let store = SqliteStore::open(&database)
                .await
                .with_context(|| format!("open sqlite database {}", database.display()))?;
            store.migrate().await?;
            Ok(())
        }
        Command::Daemon {
            config,
            database,
            once,
        } => {
            daemon::run(daemon::DaemonOptions {
                config_path: config,
                database_path: database,
                once,
            })
            .await
        }
        Command::RecordAcceptanceSelfDefect {
            config,
            database,
            source_project,
            source_issue,
            session_id,
            fingerprint,
            message,
            process_id,
        } => {
            daemon::record_acceptance_self_defect(daemon::AcceptanceSelfDefectOptions {
                config_path: config,
                database_path: database,
                source_project_id: source_project,
                source_issue_identifier: source_issue,
                session_id,
                fingerprint,
                message,
                process_id,
            })
            .await?;
            Ok(())
        }
    }
}
