use crate::{
    api::{RuntimeDashboardApi, runtime_api_json_response},
    config::RootConfig,
    storage::{SqliteStore, StorageError},
};

mod html;
mod quota;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardHtmlResponse {
    pub status: u16,
    pub body: String,
}

pub async fn runtime_dashboard_html_response(
    config: &RootConfig,
    store: &SqliteStore,
    path: &str,
) -> Result<Option<DashboardHtmlResponse>, StorageError> {
    let normalized = path.trim_end_matches('/');
    let normalized = if normalized.is_empty() {
        "/"
    } else {
        normalized
    };
    let api = RuntimeDashboardApi::from_store(config, store).await?;

    let quota = quota::OpenCodeQuotaSnapshot::load_localhost().await.ok();

    let response = match normalized {
        "/" => html_response(200, html::render_aggregate(api.aggregate(), quota.as_ref())),
        "/quota" => html_response(200, html::render_quota(quota.as_ref())),
        path if path.starts_with("/projects/") => project_or_issue_response(&api, path)?,
        path if path.starts_with("/api/") => return Ok(None),
        _ => html_response(404, html::render_not_found(normalized)),
    };

    Ok(Some(response))
}

pub async fn runtime_dashboard_response(
    config: &RootConfig,
    store: &SqliteStore,
    path: &str,
) -> Result<(u16, &'static str, String), StorageError> {
    if let Some(response) = runtime_dashboard_html_response(config, store, path).await? {
        return Ok((response.status, "text/html; charset=utf-8", response.body));
    }

    let response = runtime_api_json_response(config, store, path).await?;
    Ok((response.status, "application/json", response.body))
}

fn project_or_issue_response(
    api: &RuntimeDashboardApi,
    path: &str,
) -> Result<DashboardHtmlResponse, StorageError> {
    let parts = path
        .strip_prefix("/projects/")
        .unwrap_or_default()
        .split('/')
        .collect::<Vec<_>>();

    match parts.as_slice() {
        [project_id] => api.project_drilldown(project_id)?.map_or_else(
            || Ok(html_response(404, html::render_not_found(path))),
            |project| Ok(html_response(200, html::render_project(project))),
        ),
        [project_id, "issues", issue_id] => api.issue_detail(project_id, issue_id)?.map_or_else(
            || Ok(html_response(404, html::render_not_found(path))),
            |issue| Ok(html_response(200, html::render_issue(issue))),
        ),
        _ => Ok(html_response(404, html::render_not_found(path))),
    }
}

const fn html_response(status: u16, body: String) -> DashboardHtmlResponse {
    DashboardHtmlResponse { status, body }
}

#[cfg(test)]
mod tests {
    use super::html;

    #[test]
    fn html_escape_escapes_markup_quotes_and_apostrophes() {
        assert_eq!(
            html::escape("<script>alert('x') & \"y\"</script>"),
            "&lt;script&gt;alert(&#39;x&#39;) &amp; &quot;y&quot;&lt;/script&gt;"
        );
    }

    #[test]
    fn trim_middle_preserves_short_values_and_bounds_long_values() {
        assert_eq!(html::trim_middle("short", 12), "short");
        assert_eq!(
            html::trim_middle("abcdefghijklmnopqrstuvwxyz", 12),
            "abcde…uvwxyz"
        );
    }
}
