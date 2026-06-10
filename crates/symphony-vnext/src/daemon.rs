use std::{fs, path::PathBuf};

use anyhow::{Context, bail};

use crate::{config::RootConfig, storage::SqliteStore};

#[derive(Debug)]
pub struct DaemonOptions {
    pub config_path: PathBuf,
    pub database_path: PathBuf,
    pub once: bool,
}

pub fn run(options: DaemonOptions) -> anyhow::Result<()> {
    if !options.once {
        bail!(
            "continuous daemon mode is not implemented yet; pass --once for bootstrap validation"
        );
    }

    let input = fs::read_to_string(&options.config_path)
        .with_context(|| format!("read config {}", options.config_path.display()))?;
    let config = RootConfig::from_yaml_str(&input)?;
    let store = SqliteStore::open(&options.database_path)
        .with_context(|| format!("open sqlite database {}", options.database_path.display()))?;
    store.migrate()?;
    store.reconcile_projects(&config)?;

    Ok(())
}
