use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use super::{OpenCodeError, OpenCodeLaunchSpec, PermissionPolicy};

pub(super) async fn set_session_config_option<R, W>(
    stdin: &mut W,
    stdout: &mut R,
    permission_policy: &PermissionPolicy,
    id: u64,
    session_id: &str,
    config_id: &str,
    value: Option<&str>,
) -> Result<(), OpenCodeError>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    acp_request(
        stdin,
        stdout,
        permission_policy,
        id,
        "session/set_config_option",
        json!({
            "sessionId": session_id,
            "configId": config_id,
            "value": value,
        }),
    )
    .await?;
    Ok(())
}

pub(super) fn session_new_params(spec: &OpenCodeLaunchSpec) -> Value {
    json!({
        "cwd": spec.cwd,
        "title": spec
            .prompt
            .lines()
            .next()
            .unwrap_or("Symphony OpenCode issue"),
        "agent": spec.agent,
        "mcpServers": [],
    })
}

pub(super) async fn acp_request<R, W>(
    stdin: &mut W,
    stdout: &mut R,
    permission_policy: &PermissionPolicy,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, OpenCodeError>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    write_acp_request(stdin, id, method, params).await?;
    read_acp_response(stdout, stdin, permission_policy, id, method).await
}

pub(super) async fn read_acp_response<R, W>(
    stdout: &mut R,
    stdin: &mut W,
    permission_policy: &PermissionPolicy,
    id: u64,
    method: &str,
) -> Result<Value, OpenCodeError>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    loop {
        let mut line = String::new();
        let bytes = stdout.read_line(&mut line).await?;
        if bytes == 0 {
            return Err(OpenCodeError::AcpProtocol(format!(
                "ACP stdout closed before `{method}` response"
            )));
        }
        let message: Value = serde_json::from_str(&line).map_err(|error| {
            OpenCodeError::AcpProtocol(format!("invalid ACP JSON `{line}`: {error}"))
        })?;

        if message.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = message.get("error") {
                return Err(OpenCodeError::AcpProtocol(format!(
                    "ACP `{method}` failed: {error}"
                )));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }

        if message.get("id").is_some() && message.get("method").is_some() {
            respond_to_acp_request(stdin, permission_policy, &message).await?;
        }
    }
}

pub(super) async fn write_acp_request<W>(
    stdin: &mut W,
    id: u64,
    method: &str,
    params: Value,
) -> Result<(), OpenCodeError>
where
    W: AsyncWrite + Unpin + Send,
{
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    stdin.write_all(format!("{request}\n").as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

async fn respond_to_acp_request<W>(
    stdin: &mut W,
    permission_policy: &PermissionPolicy,
    request: &Value,
) -> Result<(), OpenCodeError>
where
    W: AsyncWrite + Unpin + Send,
{
    let Some(id) = request.get("id").cloned() else {
        return Ok(());
    };
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("request");
    let result = match permission_policy {
        PermissionPolicy::Reject => json!({
            "outcome": "rejected",
            "message": format!("Symphony rejects ACP `{method}` requests in unattended mode"),
        }),
        PermissionPolicy::Cancel => json!({
            "outcome": "cancelled",
            "message": format!("Symphony cancels ACP `{method}` requests in unattended mode"),
        }),
    };
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    stdin.write_all(format!("{response}\n").as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

pub(super) fn extract_session_id(result: &Value) -> Result<String, OpenCodeError> {
    result
        .get("sessionId")
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            OpenCodeError::AcpProtocol(format!(
                "ACP session/new response did not include session id: {result}"
            ))
        })
}
