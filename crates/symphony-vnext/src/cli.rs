use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use crate::{config::RootConfig, daemon, storage::SqliteStore};

#[derive(Debug, Parser)]
#[command(name = "symphony-vnext", about = "Rust-first Symphony vNext runtime")]
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
}

pub async fn run() -> anyhow::Result<()> {
    run_with_args(std::env::args_os()).await
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
            RootConfig::from_yaml_str(&input)?;
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
    }
}
