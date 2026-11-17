// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成 MCP 的 spec（上游 `core/tool_services/image_mcp.rs` 的 AIGX 版）。
//!
//! 工具集与上游逐字段对齐：
//!   · `generate_images`：文生图（`request_id` 幂等，`n/size/quality/output_format`）；
//!   · `edit_images`：参考图编辑（管理员启用 `editing` 后才出现）；
//!   · `get_image_result`：按 `job_id` 或原 `request_id` 查询，不重新生图。
//!
//! 数量上限 / 保留期来自 `image_config`；`server_info` 与包名走
//! `ToolServices` 的固定元数据（与 `/api/tools/install-info` 同一口径）。

use serde_json::{json, Value};

use super::{image_config, McpSpec, ToolServices};

pub fn spec(services: &ToolServices) -> McpSpec {
    let config = image_config::load(services).unwrap_or_default();
    let retention = config.retention_hours.clamp(1, 168);
    let count = config.max_images.clamp(1, 10);
    let mut tools = vec![generation_tool(false, count, retention)];
    if config.editing.is_some() {
        tools.push(generation_tool(true, count, retention));
    }
    tools.push(json!({
        "name": "get_image_result", "title": "Get generated images",
        "description": "Query an existing image job without generating again. Use job_id or the original request_id. Results expire after the retention period returned by the job. Never submit another generation automatically when the status is uncertain, failed or expired.",
        "inputSchema": { "type": "object", "properties": {
            "job_id": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Job ID returned by generation." },
            "request_id": { "type": "string", "minLength": 8, "maxLength": 128, "description": "Original submission identifier; use this when the submission response was lost." },
            "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 20, "description": "Wait up to this many seconds for completion, default 0." },
            "preview": { "type": "boolean", "description": "Include a small image preview, default false." }
        }, "anyOf": [{ "required": ["job_id"] }, { "required": ["request_id"] }], "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
    }));
    McpSpec {
        server_info: json!({
            "name": super::package_name(super::IMAGE_GENERATION_SERVICE_ID),
            "title": "Image Generation",
            "version": super::IMAGE_GENERATION_VERSION,
        }),
        instructions: "Generate images using accounts configured in AIGX. Generation consumes upstream credits. Use a unique request_id for each user-approved generation and reuse it for transport retries; poll get_image_result for running jobs. Never invent local file paths: HTTP results are authenticated download URLs, not files on the client machine. Model and account selection are controlled by the administrator.",
        tools,
    }
}

fn generation_tool(editing: bool, count: u32, retention: u32) -> Value {
    let mut properties = json!({
        "request_id": { "type": "string", "minLength": 8, "maxLength": 128, "pattern": "^[A-Za-z0-9_-]+$",
            "description": "Unique identifier (8-128 ASCII letters, digits, hyphens or underscores). Reuse it after a timeout; use a new identifier only for a new user-approved generation." },
        "prompt": { "type": "string", "minLength": 1, "maxLength": 32000, "description": "Complete image requirements: subject, composition, style, text and changes to references." },
        "n": { "type": "integer", "minimum": 1, "maximum": count, "description": "Image count, default 1. Some providers only accept 1; the service never makes extra calls to fill a batch." },
        "size": { "type": "string", "description": "Optional auto or WIDTHxHEIGHT, subject to provider limits." },
        "quality": { "type": "string", "description": "Optional provider-supported quality. Omit unless known to be supported." },
        "output_format": { "type": "string", "enum": ["png", "jpeg", "webp"], "description": "File encoding; default png." },
        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 20, "description": "Wait up to this many seconds, default 10; running jobs can be queried later." },
        "preview": { "type": "boolean", "description": "Include a small image preview on completion, default false." }
    });
    let mut required = vec!["request_id", "prompt"];
    if editing {
        properties["images"] = json!({ "type": "array", "minItems": 1, "maxItems": 16,
            "items": { "type": "string", "minLength": 1 },
            "description": "Ordered references: image:<id> from a previous result, public HTTP(S) URLs, or image data URIs. HTTP mode cannot read local file paths. PNG/JPEG/WebP only." });
        required.push("images");
    }
    json!({
        "name": if editing { "edit_images" } else { "generate_images" },
        "title": if editing { "Edit images" } else { "Generate images" },
        "description": format!("{} Results are retained for {retention} hours; submission identifiers are deduplicated for that retention period plus seven days. Never reuse an old request_id for a new generation.", if editing {
            "Edit reference images with the configured model. Consumes credits. Returns the image files or a running job; poll get_image_result without submitting again. No masks, exact pixel preservation or streaming output."
        } else {
            "Generate images from text with the configured model and accounts. Consumes credits. Returns image files or a running job; poll get_image_result without submitting again. Does not change the agent's chat model."
        }),
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileStore;
    use crate::tool_services::{ImageConfig, ImageTarget};
    use serde_json::json;
    use std::sync::Arc;

    fn services(with_editing: bool) -> (ToolServices, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let services = ToolServices::new(store);
        let config = ImageConfig {
            generation: ImageTarget {
                provider: "workbuddy".into(),
                model: "glm-image".into(),
                account_id: String::new(),
            },
            editing: with_editing.then(|| ImageTarget {
                provider: "workbuddy".into(),
                model: "glm-image".into(),
                account_id: String::new(),
            }),
            max_images: 3,
            max_concurrency: 2,
            retention_hours: 24,
            storage_mb: 512,
        };
        services
            .update(|value| value.image_generation = Some(config))
            .unwrap();
        (services, dir)
    }

    /// spec 形状：工具名、数量上限、编辑开关、查询工具的 anyOf。
    #[test]
    fn spec_shape_matches_upstream() {
        let (services, _dir) = services(false);
        let spec = spec(&services);
        let names: Vec<&str> = spec
            .tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["generate_images", "get_image_result"]);
        let generate = &spec.tools[0];
        assert_eq!(generate["inputSchema"]["properties"]["n"]["maximum"], 3);
        assert_eq!(
            generate["inputSchema"]["required"],
            json!(["request_id", "prompt"])
        );
        assert_eq!(generate["inputSchema"]["additionalProperties"], false);
        let query = &spec.tools[1];
        assert_eq!(
            query["inputSchema"]["anyOf"],
            json!([{ "required": ["job_id"] }, { "required": ["request_id"] }])
        );
        assert_eq!(query["annotations"]["readOnlyHint"], true);
        assert_eq!(spec.server_info["name"], "agent2api-image-generation");

        let (services, _dir) = self::services(true);
        let spec = super::spec(&services);
        let names: Vec<&str> = spec
            .tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["generate_images", "edit_images", "get_image_result"]
        );
        let edit = &spec.tools[1];
        assert_eq!(
            edit["inputSchema"]["required"],
            json!(["request_id", "prompt", "images"])
        );
        assert_eq!(edit["inputSchema"]["properties"]["images"]["maxItems"], 16);
    }
}
