//! The attachment input contract shared by both MCP transports.
use base64::{Engine, engine::general_purpose::STANDARD};
use schemars::JsonSchema;
use serde::Deserialize;

pub const MAX_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_BASE64: usize = MAX_BYTES.div_ceil(3) * 4;
// Space for base64 and the JSON envelope. Applied only to the MCP route.
pub const MAX_REQUEST: usize = MAX_BASE64 + 1024 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachArgs {
    pub workspace: String,
    pub id: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub content: Option<String>,
    pub content_base64: Option<String>,
    pub path: Option<String>,
}

impl AttachArgs {
    pub fn validate(&self) -> Result<(), String> {
        if self.workspace.trim().is_empty() || self.id.trim().is_empty() {
            return Err("workspace and id must not be empty".into());
        }
        if self.filename.trim().is_empty() || self.filename.chars().count() > 256 {
            return Err("filename must contain 1 to 256 characters".into());
        }
        let sources = [
            self.content.is_some(),
            self.content_base64.is_some(),
            self.path.is_some(),
        ];
        if sources.into_iter().filter(|v| *v).count() != 1 {
            return Err("provide exactly one of content, content_base64 or path".into());
        }
        Ok(())
    }

    /// Remote MCP never interprets paths on the server's filesystem.
    pub fn inline_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = if let Some(content) = &self.content {
            check_size(content.len())?;
            content.as_bytes().to_vec()
        } else if let Some(encoded) = &self.content_base64 {
            if encoded.len() > MAX_BASE64 {
                return Err("file exceeds 50 MiB".into());
            }
            STANDARD
                .decode(encoded)
                .map_err(|_| "content_base64 must be standard padded base64")?
        } else {
            return Err("path is available only in local stdio MCP; send content or content_base64 to remote MCP".into());
        };
        check_size(bytes.len())?;
        Ok(bytes)
    }
}

pub fn check_size(size: usize) -> Result<(), String> {
    if size == 0 || size > MAX_BYTES {
        Err("file size must be from 1 byte to 50 MiB".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "attachments_tests.rs"]
mod tests;
