use anyhow::{Context, Result, anyhow, bail};
use ntk_core::attachments::{AttachArgs, MAX_BYTES, check_size};
use serde_json::{Value, json};

use super::{Client, urlencode};

impl Client {
    pub async fn attachments(&self, key: &str, workspace: &str, id: &str) -> Result<Value> {
        let response = self
            .http
            .get(format!(
                "{}/v1/tickets/{}/attachments",
                self.base,
                urlencode(&id.to_ascii_lowercase())
            ))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .send()
            .await
            .context("could not list attachments")?;
        response_json(response).await
    }

    pub async fn attach(&self, key: &str, args: AttachArgs) -> Result<Value> {
        args.validate().map_err(|e| anyhow!(e))?;
        let bytes = if let Some(path) = args.path.clone() {
            tokio::task::spawn_blocking(move || read_file(&path)).await??
        } else {
            args.inline_bytes().map_err(|e| anyhow!(e))?
        };
        let id = args.id.to_ascii_lowercase();
        let endpoint = format!("{}/v1/tickets/{}/attachments", self.base, urlencode(&id));
        let mime = args
            .content_type
            .as_deref()
            .unwrap_or("application/octet-stream");
        let content_type =
            reqwest::header::HeaderValue::from_str(mime).context("invalid content_type")?;
        let begin = self
            .http
            .post(&endpoint)
            .bearer_auth(key)
            .json(&json!({
                "workspace": args.workspace, "filename": args.filename,
                "content_type": mime, "size_bytes": bytes.len(),
            }))
            .send()
            .await
            .context("could not begin attachment upload")?;
        let upload = response_json(begin).await?;
        let url = upload["url"]
            .as_str()
            .context("upload response has no URL")?;
        let object_key = upload["object_key"]
            .as_str()
            .context("upload response has no object key")?;
        // Never send the NTK bearer token to storage or include its signed URL in errors.
        self.http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .timeout(std::time::Duration::from_secs(300))
            .body(bytes)
            .send()
            .await
            .map_err(|e| anyhow!("file upload failed: {}", e.without_url()))?
            .error_for_status()
            .map_err(|e| anyhow!("file upload failed: {}", e.without_url()))?;
        let committed = self.http.post(format!("{endpoint}/commit")).bearer_auth(key)
            .json(&json!({
                "workspace": args.workspace, "filename": args.filename,
                "content_type": mime, "object_key": object_key,
            })).send().await.context("upload sent, but attachment confirmation failed; check ntk_attachments before retrying")?;
        response_json(committed).await
    }
}

fn read_file(path: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        bail!("path must be absolute");
    }
    let file = std::fs::File::open(path).context("could not open attachment file")?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("path must name a regular file");
    }
    check_size(usize::try_from(metadata.len())?).map_err(|e| anyhow!(e))?;
    // Bound the read as well: the file may grow after metadata was checked.
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    check_size(bytes.len()).map_err(|e| anyhow!(e))?;
    Ok(bytes)
}

async fn response_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .context("attachment response was not JSON")?;
    if !status.is_success() {
        bail!("{}", body["error"].as_str().unwrap_or(status.as_str()));
    }
    Ok(body)
}

#[cfg(test)]
mod tests;
