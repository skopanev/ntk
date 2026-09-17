//! MCP adapter: reuse REST authorization, ticket checks and storage verification.
use super::*;
use ntk_core::attachments::AttachArgs;

pub(super) async fn call(app: &Arc<App>, token: &str, name: &str, args: &Value) -> (bool, String) {
    match execute(app, token, name, args).await {
        Ok(result) => result,
        Err(error) => (false, error.to_string()),
    }
}

async fn execute(
    app: &Arc<App>,
    token: &str,
    name: &str,
    args: &Value,
) -> anyhow::Result<(bool, String)> {
    let workspace = args["workspace"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("workspace is required"))?;
    let id = args["id"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("id is required"))?
        .to_ascii_lowercase();
    if name == "ntk_attachments" {
        let query = serde_json::from_value(json!({"workspace": workspace}))?;
        return Ok(body_text(
            crate::attach::list(State(app.clone()), hdrs(token), Path(id), Query(query)).await,
        )
        .await);
    }
    let input: AttachArgs = serde_json::from_value(args.clone())?;
    let bytes = input.inline_bytes().map_err(|e| anyhow::anyhow!(e))?;
    let mime = input
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    let content_type = reqwest::header::HeaderValue::from_str(mime)?;
    let begin = serde_json::from_value(json!({
        "workspace": workspace, "filename": input.filename,
        "content_type": mime, "size_bytes": bytes.len(),
    }))?;
    let (ok, text) = body_text(
        crate::attach::begin(
            State(app.clone()),
            hdrs(token),
            Path(id.clone()),
            Json(begin),
        )
        .await,
    )
    .await;
    if !ok {
        return Ok((false, text));
    }
    let upload: Value = serde_json::from_str(&text)?;
    let url = upload["url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("upload response has no URL"))?;
    let object_key = upload["object_key"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("upload response has no object key"))?;
    reqwest::Client::new()
        .put(url)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .timeout(std::time::Duration::from_secs(300))
        .body(bytes)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("file upload failed: {}", e.without_url()))?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("file upload failed: {}", e.without_url()))?;
    let commit = serde_json::from_value(json!({
        "workspace": workspace, "filename": input.filename,
        "content_type": mime, "object_key": object_key,
    }))?;
    Ok(body_text(
        crate::attach::commit(State(app.clone()), hdrs(token), Path(id), Json(commit)).await,
    )
    .await)
}
