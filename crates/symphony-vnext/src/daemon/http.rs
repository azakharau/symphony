use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, error, info, warn};

use crate::{
    api::runtime_api_json_response, config::RootConfig, linear::LinearSdkClient,
    opencode::StdioOpenCodeLauncher, storage::SqliteStore,
};

use super::run_once_with_clients;

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
                        run_once_with_clients(&poll_config, &store, &linear, &StdioOpenCodeLauncher)
                            .await
                    {
                        error!(error = %error, "poll failed");
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

    loop {
        let (stream, peer) = listener.accept().await?;
        debug!(%peer, "dashboard HTTP connection accepted");
        if let Err(error) = handle_http_stream(&config, &database_path, stream).await {
            warn!(error = %error, "dashboard HTTP request failed");
        }
    }
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
        write_http_response(&mut stream, 405, r#"{"error":"method_not_allowed"}"#).await?;
        return Ok(());
    }

    let store = SqliteStore::open(database_path).await?;
    store.migrate().await?;
    let response = runtime_api_json_response(config, &store, path).await?;
    write_http_response(&mut stream, response.status, &response.body).await?;
    Ok(())
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
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
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
}
