use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, error, info, warn};

use crate::{
    api::runtime_api_json_response,
    config::{RootConfig, RunnerArchiveConfig},
    linear::LinearSdkClient,
    runner::{
        RunnerSessionArchiveRequest, StdioRunnerLauncher, apply_omp_session_tree_metrics,
        apply_session_tree_metrics, archive_and_delete_session_tree, read_omp_session_tree_metrics,
        read_session_tree_metrics,
    },
    state::{RunnerSessionRecord, RuntimeProviderMode},
    storage::SqliteStore,
};

use super::run_once_with_clients;

const RUNNER_METRICS_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub(super) async fn run_continuous(
    config: RootConfig,
    database_path: PathBuf,
) -> anyhow::Result<()> {
    let server = config
        .server
        .clone()
        .context("continuous daemon mode requires server.host and server.port")?;
    let bind_addr = format!("{}:{}", server.host, server.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("bind dashboard API {bind_addr}"))?;
    info!(%bind_addr, "dashboard API listening");

    let poll_config = config.clone();
    let poll_database_path = database_path.clone();
    let linear = LinearSdkClient::from_env()?;

    tokio::spawn(async move {
        loop {
            debug!("poll tick started");
            match SqliteStore::open(&poll_database_path).await {
                Ok(store) => {
                    if let Err(error) = store.migrate().await {
                        error!(error = %error, "poll storage migration failed");
                    } else if let Err(error) =
                        run_once_with_clients(&poll_config, &store, &linear, &StdioRunnerLauncher)
                            .await
                    {
                        let error_chain = error
                            .chain()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(": ");
                        error!(error = %error, error_chain = %error_chain, "poll failed");
                    } else {
                        debug!("poll tick completed");
                    }
                }
                Err(error) => {
                    error!(error = %error, "poll storage open failed");
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    {
        let metrics_database_path = database_path.clone();
        let runner_archive = config.runner_archive.clone();
        tokio::spawn(async move {
            loop {
                match SqliteStore::open(&metrics_database_path).await {
                    Ok(store) => {
                        if let Err(error) = store.migrate().await {
                            error!(error = %error, "runtime metrics poll storage migration failed");
                        } else if let Err(error) =
                            refresh_runtime_session_metrics(&store, runner_archive.as_ref()).await
                        {
                            warn!(error = %error, "runtime metrics poll failed");
                        }
                    }
                    Err(error) => {
                        error!(error = %error, "runtime metrics poll storage open failed");
                    }
                }
                tokio::time::sleep(RUNNER_METRICS_POLL_INTERVAL).await;
            }
        });
    }

    if config.cleanup.enabled {
        let cleanup_database_path = database_path.clone();
        let cleanup_interval = Duration::from_secs(config.cleanup.interval_secs);
        let cleanup_retention = Duration::from_secs(config.cleanup.retention_secs);
        let runner_archive = config.runner_archive.clone();
        info!(
            interval_secs = cleanup_interval.as_secs(),
            retention_secs = cleanup_retention.as_secs(),
            "runtime cleanup worker scheduled"
        );
        tokio::spawn(async move {
            tokio::time::sleep(cleanup_interval).await;
            loop {
                debug!(
                    interval_secs = cleanup_interval.as_secs(),
                    retention_secs = cleanup_retention.as_secs(),
                    "runtime cleanup tick started"
                );
                match SqliteStore::open(&cleanup_database_path).await {
                    Ok(store) => {
                        if let Err(error) = store.migrate().await {
                            error!(error = %error, "runtime cleanup storage migration failed");
                        } else {
                            if let Some(storage) = runner_archive.as_ref()
                                && let Err(error) =
                                    cleanup_runner_sessions(&store, storage, cleanup_retention)
                                        .await
                            {
                                error!(error = %error, "runner session cleanup failed");
                            }
                            match store.cleanup_runtime_state(cleanup_retention).await {
                                Ok(report) => {
                                    if report.issues_deleted > 0
                                        || report.sessions_deleted > 0
                                        || report.stage_events_deleted > 0
                                        || report.eval_runs_deleted > 0
                                    {
                                        info!(
                                            issues_deleted = report.issues_deleted,
                                            sessions_deleted = report.sessions_deleted,
                                            stage_events_deleted = report.stage_events_deleted,
                                            eval_runs_deleted = report.eval_runs_deleted,
                                            "runtime cleanup removed stale rows"
                                        );
                                    } else {
                                        debug!("runtime cleanup found no stale rows");
                                    }
                                }
                                Err(error) => {
                                    error!(error = %error, "runtime cleanup failed");
                                }
                            }
                        }
                    }
                    Err(error) => {
                        error!(error = %error, "runtime cleanup storage open failed");
                    }
                }
                tokio::time::sleep(cleanup_interval).await;
            }
        });
    } else {
        info!("runtime cleanup worker disabled by config");
    }

    loop {
        let (stream, peer) = listener.accept().await?;
        debug!(%peer, "dashboard HTTP connection accepted");
        if let Err(error) = handle_http_stream(&config, &database_path, stream).await {
            warn!(error = %error, "dashboard HTTP request failed");
        }
    }
}

async fn cleanup_runner_sessions(
    store: &SqliteStore,
    storage: &RunnerArchiveConfig,
    retention: Duration,
) -> anyhow::Result<()> {
    let candidates = store.runner_cleanup_candidates(retention).await?;
    if candidates.is_empty() {
        debug!("runner session cleanup found no archive candidates");
        return Ok(());
    }
    for candidate in candidates {
        let report = archive_and_delete_session_tree(RunnerSessionArchiveRequest {
            runner_archive_database_path: storage.database_path.clone(),
            archive_root: storage.archive_root.clone(),
            project_id: candidate.project_id.clone(),
            issue_id: candidate.issue_id.clone(),
            issue_identifier: candidate.issue_identifier.clone(),
            root_session_id: candidate.session_id.clone(),
        })
        .await?;
        if report.sessions_archived > 0 || report.sessions_deleted > 0 {
            let runtime_records_deleted = store
                .delete_runner_session_record(
                    &candidate.project_id,
                    &candidate.issue_id,
                    &candidate.session_id,
                )
                .await?;
            info!(
                project_id = %candidate.project_id,
                issue = %candidate.issue_identifier,
                session_id = %candidate.session_id,
                artifact_root = %report.artifact_root.display(),
                sessions_archived = report.sessions_archived,
                sessions_deleted = report.sessions_deleted,
                runtime_records_deleted,
                "runner session tree archived and cleaned"
            );
        } else {
            debug!(
                project_id = %candidate.project_id,
                issue = %candidate.issue_identifier,
                session_id = %candidate.session_id,
                "runner session cleanup candidate had no persisted runner rows"
            );
        }
    }
    Ok(())
}

async fn refresh_runtime_session_metrics(
    store: &SqliteStore,
    runner_archive: Option<&RunnerArchiveConfig>,
) -> anyhow::Result<()> {
    for session in store.active_runner_sessions().await? {
        match session.provider_mode {
            RuntimeProviderMode::Acp => {
                let Some(storage) = runner_archive else {
                    continue;
                };
                refresh_single_runner_session_metrics(store, storage, session).await?;
            }
            RuntimeProviderMode::OmpAcp => {
                refresh_single_omp_session_metrics(store, session).await?;
            }
        }
    }
    Ok(())
}

async fn refresh_single_runner_session_metrics(
    store: &SqliteStore,
    storage: &RunnerArchiveConfig,
    mut session: RunnerSessionRecord,
) -> anyhow::Result<()> {
    let Some(metrics) =
        read_session_tree_metrics(&storage.database_path, &session.session_id).await?
    else {
        return Ok(());
    };
    if session_metrics_are_current(&session, metrics.last_updated_ms) {
        return Ok(());
    }
    apply_session_tree_metrics(&mut session, &metrics);
    store.upsert_runner_session(&session).await?;
    debug!(
        project_id = %session.project_id,
        issue_id = %session.issue_id,
        session_id = %session.session_id,
        last_updated_ms = metrics.last_updated_ms,
        "runner session metrics refreshed from lightweight poll"
    );
    Ok(())
}

async fn refresh_single_omp_session_metrics(
    store: &SqliteStore,
    mut session: RunnerSessionRecord,
) -> anyhow::Result<()> {
    let Some(metrics) = read_omp_session_tree_metrics(&session.session_id).await? else {
        return Ok(());
    };
    if omp_session_metrics_are_current(&session, metrics.last_updated_ms) {
        return Ok(());
    }
    apply_omp_session_tree_metrics(&mut session, &metrics);
    store.upsert_runner_session(&session).await?;
    debug!(
        project_id = %session.project_id,
        issue_id = %session.issue_id,
        session_id = %session.session_id,
        subagents = metrics.subagent_count,
        messages = metrics.message_count,
        tokens = metrics.tokens_total,
        last_updated_ms = metrics.last_updated_ms,
        "OMP session metrics refreshed from lightweight poll"
    );
    Ok(())
}

fn session_metrics_are_current(
    session: &RunnerSessionRecord,
    last_updated_ms: Option<u64>,
) -> bool {
    let expected_event = last_updated_ms
        .map(|updated| format!("runner_archive_updated:{updated}"))
        .unwrap_or_else(|| "runner_archive_snapshot".into());
    session.last_event.as_deref() == Some(expected_event.as_str())
}

fn omp_session_metrics_are_current(
    session: &RunnerSessionRecord,
    last_updated_ms: Option<u64>,
) -> bool {
    let expected_event = last_updated_ms
        .map(|updated| format!("omp_jsonl_updated:{updated}"))
        .unwrap_or_else(|| "omp_jsonl_snapshot".into());
    session.last_event.as_deref() == Some(expected_event.as_str())
}

async fn handle_http_stream(
    config: &RootConfig,
    database_path: &PathBuf,
    stream: TcpStream,
) -> anyhow::Result<()> {
    let mut first_line = String::new();
    let mut reader = BufReader::new(stream);
    reader.read_line(&mut first_line).await?;
    let mut stream = reader.into_inner();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/");
    debug!(method, path, "dashboard HTTP request");

    if method != "GET" {
        write_http_response(
            &mut stream,
            405,
            "application/json",
            r#"{"error":"method_not_allowed"}"#,
        )
        .await?;
        return Ok(());
    }

    let store = SqliteStore::open(database_path).await?;
    store.migrate().await?;
    let response = runtime_api_json_response(config, &store, path).await?;
    write_http_response(
        &mut stream,
        response.status,
        response.content_type,
        &response.body,
    )
    .await?;
    Ok(())
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
}
