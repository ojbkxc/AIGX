// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成运行时（上游 `api/tool_images.rs` + `tool_image_results.rs` 的 AIGX 版）。
//!
//! ── 执行层怎么复用 ──────────────────────────────────────────
//! AIGX 的 `/v1/images/generations|edits` handler 把鉴权、计费、日志与执行
//! 交织在一起，本模块不调用 HTTP handler，而是复用同一条**内部生成管线**：
//! `api::openai::resolve_bridges_needs`（渠道/账号候选 + 能力过滤）→
//! `resolve_upstream_model`（渠道级模型映射）→ `Bridge::generate_image` /
//! `Bridge::edit_image`。failover 的安全判定与 `images.rs` 一致：
//! 在途丢失类错误（超时/传输中断/解码失败/账号池聚合失败）**不换渠道重发**
//! （上游可能已出图已计费）。`images_failover_safe` 是 `images.rs` 里的私有
//! 函数，这里按同语义复制（未改热路径文件，见报告「偏离」）。
//!
//! ── 与上游的差异 ────────────────────────────────────────────
//! · 无 `RequestTelemetry` / `KeyScope`：owner 用网关 Key id（上游用 Key 哈希），
//!   作用域用 `ApiKey.allowed_providers/allowed_models` 直接判定；
//! · 不写请求日志、不扣费（上游经内部 images 管线计入用量；AIGX 工具服务是
//!   管理员配置的账号池，MCP 调用不产生 Key 级计费上下文）；
//! · `account_id` 只做存在性/提供商匹配校验，`AccountBridge` 仍按优先级选号
//!   （AIGX 无 `KeyScope::restrict_image_target` / `routing_snapshot` 钉号能力）；
//! · 编辑参考图在**任务创建前**解析（`image:<id>` / data URI / 公网 URL），
//!   上游在任务内带超时解析；失败时不留占位任务（重试幂等语义不变）。
//!
//! ── 结果形状 ────────────────────────────────────────────────
//! `job_result` / `mcp_result` / `file_response` 与上游 `tool_image_results.rs`
//! 逐字段对齐：状态、guidance、`images[].url/ref/requires_auth`、过期时间、
//! 去重保留期；MCP 侧附 `image` 预览（`preview=true`）。

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::api::openai::AppState;
use crate::bridge::{BridgeContext, ImageEditFormPart};

use super::image_config::{self, ImageOperation, ImageTarget};
use super::image_jobs::{self, ImageFile, ImageJob, ImageJobs, ImageToolError, NewJob};
use super::mcp::McpRequestContext;

/// MCP 结果里的下载地址前缀（上游同路径）。
pub const MCP_FILE_PREFIX: &str = "/mcp/image-generation/files";
/// 管理面结果里的下载地址前缀。
pub const MANAGEMENT_FILE_PREFIX: &str = "/api/tools/image-generation/files";

const MAX_REFERENCE_BYTES: usize = 10 * 1024 * 1024;
const MAX_INLINE_BYTES: usize = 14 * 1024 * 1024;
const MAX_REFERENCES: usize = 16;

/// 运行时任务存储（每次调用构造；KV 与文件目录都来自 ToolServices / 数据目录）。
pub fn jobs_for(state: &AppState) -> ImageJobs {
    ImageJobs::from_services(state.tool_services.as_ref())
}

/// 网关 Key → 任务 owner。上游用 Key 哈希；AIGX 用 Key id（稳定、不落明文）。
pub fn owner_of(key: &crate::api::auth::ApiKey) -> String {
    key.id.clone()
}

/* ─── 请求形状 ─────────────────────────────────────────────── */

fn text<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn submission_id(args: &Value) -> Result<String, ImageToolError> {
    let id = text(args, "request_id");
    if !(8..=128).contains(&id.len())
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ImageToolError::bad_request(
            "request_id 必须是 8–128 位英文字母、数字、连字符或下划线；重试应使用原编号",
        ));
    }
    Ok(id.to_string())
}

fn options(args: &Value, query: bool) -> Result<(u64, bool), ImageToolError> {
    let Some(object) = args.as_object() else {
        return Err(ImageToolError::bad_request("工具参数必须是对象"));
    };
    let allowed: &[&str] = if query {
        &["job_id", "request_id", "wait_seconds", "preview"]
    } else {
        &[
            "request_id",
            "prompt",
            "n",
            "size",
            "quality",
            "output_format",
            "images",
            "wait_seconds",
            "preview",
        ]
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ImageToolError::bad_request(format!(
            "图片工具不支持参数 {key}"
        )));
    }
    let wait = match args.get("wait_seconds") {
        None => {
            if query {
                0
            } else {
                10
            }
        }
        Some(value) => value
            .as_u64()
            .filter(|value| *value <= 20)
            .ok_or_else(|| ImageToolError::bad_request("wait_seconds 必须为 0–20 的整数"))?,
    };
    let preview = match args.get("preview") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ImageToolError::bad_request("preview 必须为布尔值"))?,
    };
    Ok((wait, preview))
}

/// 参考图 / 结果下载的网页来源校验（与上游 `validate_origin` 同口径）。
pub fn validate_origin(headers: &HeaderMap) -> Result<(), ImageToolError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let parsed = origin
        .to_str()
        .ok()
        .and_then(|value| url::Url::parse(value).ok());
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let valid = parsed.as_ref().is_some_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && host.is_some_and(|host| {
                url::Url::parse(&format!("{}://{host}", url.scheme()))
                    .is_ok_and(|expected| expected.origin() == url.origin())
            })
    });
    if valid {
        Ok(())
    } else {
        Err(ImageToolError::new(
            403,
            "image_origin_rejected",
            "图片 MCP 拒绝不受信任的网页来源",
        ))
    }
}

/// 任务归属 / 作用域鉴权（上游 `authorize` + `KeyScope` 的扁平化版本）。
pub fn authorize_job(
    job: &ImageJob,
    owner: &str,
    allowed_providers: Option<&[String]>,
    allowed_models: Option<&[String]>,
) -> Result<(), ImageToolError> {
    let denied = || ImageToolError::not_found("图片任务不存在或无权访问");
    if job.owner != owner {
        return Err(denied());
    }
    if let Some(allowed) = allowed_providers {
        if !allowed.is_empty() && !allowed.iter().any(|provider| provider == &job.provider) {
            return Err(denied());
        }
    }
    if let Some(allowed) = allowed_models {
        if !allowed.is_empty() && !allowed.iter().any(|model| model == &job.model) {
            return Err(denied());
        }
    }
    Ok(())
}

fn authorize(job: &ImageJob, ctx: &McpRequestContext<'_>) -> Result<(), ImageToolError> {
    authorize_job(
        job,
        &ctx.owner,
        ctx.allowed_providers.as_deref(),
        ctx.allowed_models.as_deref(),
    )
}

/* ─── MCP 入口 ─────────────────────────────────────────────── */

/// `generate_images` / `edit_images` / `get_image_result` 的统一入口。
pub async fn call(
    name: &str,
    args: &Value,
    ctx: &McpRequestContext<'_>,
) -> Result<Value, ImageToolError> {
    let query = name == "get_image_result";
    let (wait, preview) = options(args, query)?;
    if query {
        for key in ["job_id", "request_id"] {
            if args.get(key).is_some_and(|value| {
                value
                    .as_str()
                    .is_none_or(|text| text.is_empty() || text.len() > 128)
            }) {
                return Err(ImageToolError::bad_request(
                    "任务编号必须是 1–128 个字符的字符串",
                ));
            }
        }
    }
    // 任务存储只依赖 ToolServices 的 FileStore（与 `jobs_for(state)` 同源，
    // 但不必经由 AppState；查询/参数错误路径因此无须完整应用状态）。
    let jobs = ImageJobs::from_services(ctx.services);
    let mut job = if query {
        let id = text(args, "job_id").to_string();
        let request_id = text(args, "request_id").to_string();
        if id.is_empty() && request_id.is_empty() {
            return Err(ImageToolError::bad_request("请提供 job_id 或原 request_id"));
        }
        if !id.is_empty() {
            jobs.get(&id)?
        } else {
            jobs.find_request(&ctx.owner, &request_id)?
                .ok_or_else(|| ImageToolError::not_found("未找到该提交编号，未重新生图"))?
        }
    } else {
        submit(name, args, ctx, &jobs).await?
    };
    authorize(&job, ctx)?;
    if !text(args, "request_id").is_empty() && text(args, "request_id") != job.request_id {
        return Err(ImageToolError::bad_request(
            "job_id 与 request_id 不属于同一任务",
        ));
    }
    let until = Instant::now() + Duration::from_secs(wait);
    while job.state == "running" && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(500)).await;
        job = jobs.get(&job.id)?;
    }
    mcp_result(&jobs, &job, &ctx.base_url, preview)
}

fn same_request(job: ImageJob, hash: &str) -> Result<ImageJob, ImageToolError> {
    if job.request_hash != hash {
        return Err(ImageToolError::new(
            409,
            "image_request_conflict",
            "同一 request_id 已用于不同的图片请求；未再次生图",
        ));
    }
    Ok(job)
}

/// 提交前的执行输入：生成走 JSON body，编辑转成上游 multipart 表单项。
enum PreparedInput {
    Generate(Value),
    Edit(Vec<ImageEditFormPart>),
}

async fn submit(
    name: &str,
    args: &Value,
    ctx: &McpRequestContext<'_>,
    jobs: &ImageJobs,
) -> Result<ImageJob, ImageToolError> {
    let operation = match name {
        "generate_images" => ImageOperation::Generate,
        "edit_images" => ImageOperation::Edit,
        _ => return Err(ImageToolError::bad_request("未知的图片工具")),
    };
    let request_id = submission_id(args)?;
    let mut normalized_args = args.clone();
    if normalized_args.get("n").is_none() {
        normalized_args["n"] = json!(1);
    }
    if normalized_args.get("output_format").is_none() {
        normalized_args["output_format"] = json!("png");
    }
    let args = &normalized_args;
    let mut hash_args = args.clone();
    if let Some(object) = hash_args.as_object_mut() {
        object.remove("wait_seconds");
        object.remove("preview");
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(format!("{name}\n{hash_args}").as_bytes())
    );
    if let Some(job) = jobs.find_request(&ctx.owner, &request_id)? {
        authorize(&job, ctx)?;
        return same_request(job, &hash);
    }
    let config = image_config::load(ctx.services)
        .ok_or_else(|| ImageToolError::new(503, "image_configuration", "尚未配置图片生成服务"))?;
    let target = image_config::target(&config, operation)?.clone();
    // 目录能力校验（模型 image 标签 + 账号存在性）；目录为空时只做形状校验。
    // 走到这里才需要完整应用状态；编辑未启用/配置缺失等错误在上方已返回。
    let state = ctx.state.ok_or_else(|| {
        ImageToolError::new(
            503,
            "image_runtime_unavailable",
            "图片生成运行时不可用（缺少应用状态）",
        )
    })?;
    let catalog = image_config::catalog(state);
    image_config::validate_target_or_error(&catalog, &target, operation)?;
    // 网关 Key 的作用域（上游 `restrict_scope`）。
    if let Some(allowed) = &ctx.allowed_providers {
        if !allowed.is_empty() && !allowed.iter().any(|provider| provider == &target.provider) {
            return Err(ImageToolError::new(
                403,
                "permission_denied",
                "当前网关 Key 无权使用所选生图提供商",
            ));
        }
    }
    if let Some(allowed) = &ctx.allowed_models {
        if !allowed.is_empty() && !allowed.iter().any(|model| model == &target.model) {
            return Err(ImageToolError::new(
                403,
                "permission_denied",
                "当前网关 Key 无权使用所选图片模型",
            ));
        }
    }
    let input = prepare_input(operation, args, &target, ctx, jobs).await?;
    let count = args
        .get("n")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 10) as u32;
    if count > config.max_images.clamp(1, 10) {
        return Err(ImageToolError::bad_request(
            "生成数量超过管理员设置的单次上限",
        ));
    }
    let timeout = super::timeout_seconds(ctx.services, super::IMAGE_GENERATION_SERVICE_ID);
    let (job, created) = jobs.create(
        &config,
        NewJob {
            owner: ctx.owner.as_str(),
            request_id: request_id.as_str(),
            request_hash: hash.as_str(),
            operation: operation.as_str(),
            target: &target,
            count,
            timeout,
        },
    )?;
    let job = same_request(job, &hash)?;
    if created {
        // 受理与执行独立于请求生命周期：HTTP 断线不会只留下占位记录。
        let state = (*state).clone();
        let jobs_copy = jobs.clone();
        let id = job.id.clone();
        tokio::spawn(async move {
            run_job(state, jobs_copy, id, operation, target, input, timeout).await;
        });
    }
    Ok(job)
}

/// 生成：JSON body（`response_format=b64_json`，输出编码默认 png）。
/// 编辑：把 1–16 张参考图解析成字节并组装上游 multipart 表单项。
async fn prepare_input(
    operation: ImageOperation,
    args: &Value,
    target: &ImageTarget,
    ctx: &McpRequestContext<'_>,
    jobs: &ImageJobs,
) -> Result<PreparedInput, ImageToolError> {
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| ImageToolError::bad_request("prompt 必须是非空字符串"))?;
    if prompt.chars().count() > 32_000 {
        return Err(ImageToolError::bad_request("prompt 最多 32000 个字符"));
    }
    let n = args
        .get("n")
        .and_then(Value::as_u64)
        .filter(|n| (1..=10).contains(n))
        .ok_or_else(|| ImageToolError::bad_request("n 必须是 1 到 10 之间的整数"))?;
    if let Some(format) = args.get("output_format") {
        format
            .as_str()
            .filter(|value| matches!(*value, "png" | "jpeg" | "webp"))
            .ok_or_else(|| ImageToolError::bad_request("output_format 必须是 png/jpeg/webp"))?;
    }
    for key in ["size", "quality"] {
        if let Some(value) = args.get(key) {
            value
                .as_str()
                .ok_or_else(|| ImageToolError::bad_request(format!("{key} 必须是字符串")))?;
        }
    }
    match operation {
        ImageOperation::Generate => {
            if args.get("images").is_some() {
                return Err(ImageToolError::bad_request("参考图只能用于编辑工具"));
            }
            let mut payload = json!({
                "model": target.model,
                "response_format": "b64_json",
                "output_format": "png",
                "n": n,
                "prompt": prompt,
            });
            for key in ["size", "quality", "output_format"] {
                if let Some(value) = args.get(key) {
                    payload[key] = value.clone();
                }
            }
            Ok(PreparedInput::Generate(payload))
        }
        ImageOperation::Edit => {
            let references = args
                .get("images")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty() && items.len() <= MAX_REFERENCES)
                .ok_or_else(|| ImageToolError::bad_request("编辑需要 1–16 张参考图"))?;
            let mut parts = vec![
                ImageEditFormPart::Text {
                    name: "model".into(),
                    value: target.model.clone(),
                },
                ImageEditFormPart::Text {
                    name: "prompt".into(),
                    value: prompt.to_string(),
                },
                ImageEditFormPart::Text {
                    name: "n".into(),
                    value: n.to_string(),
                },
                ImageEditFormPart::Text {
                    name: "response_format".into(),
                    value: "b64_json".into(),
                },
            ];
            for key in ["size", "quality", "output_format"] {
                if let Some(value) = args.get(key).and_then(Value::as_str) {
                    parts.push(ImageEditFormPart::Text {
                        name: key.into(),
                        value: value.to_string(),
                    });
                }
            }
            let mut inline_bytes = 0usize;
            for (index, reference) in references.iter().enumerate() {
                let reference = reference.as_str().ok_or_else(|| {
                    ImageToolError::bad_request("参考图必须是图片 ID、URL 或 data URI")
                })?;
                let (mime, bytes) = resolve_reference(reference, ctx, jobs).await?;
                inline_bytes = inline_bytes.saturating_add(bytes.len());
                if inline_bytes > MAX_INLINE_BYTES {
                    return Err(ImageToolError::new(
                        413,
                        "invalid_image_input",
                        "参考图内联数据总大小不得超过 14 MiB",
                    ));
                }
                let extension = match mime.as_str() {
                    "image/jpeg" => "jpg",
                    "image/webp" => "webp",
                    _ => "png",
                };
                parts.push(ImageEditFormPart::File {
                    name: "image[]".into(),
                    filename: format!("reference-{index}.{extension}"),
                    content_type: mime,
                    data: bytes::Bytes::from(bytes),
                });
            }
            Ok(PreparedInput::Edit(parts))
        }
    }
}

/// 参考图 → (mime, bytes)。支持 `image:<id>`（本服务历史结果）、
/// `data:image/...;base64,...`、公网 http(s) URL（DNS 层 SSRF 校验 +
/// 不跟随重定向 + 10 MiB 上限）。
async fn resolve_reference(
    reference: &str,
    ctx: &McpRequestContext<'_>,
    jobs: &ImageJobs,
) -> Result<(String, Vec<u8>), ImageToolError> {
    let reference = reference.trim();
    if let Some(id) = reference.strip_prefix("image:") {
        let (job, file) = jobs.read_file(id.trim())?;
        authorize(&job, ctx)?;
        if file.bytes.len() > MAX_REFERENCE_BYTES {
            return Err(ImageToolError::new(
                413,
                "invalid_image_input",
                "参考图片超过 10 MiB，请先缩小图片",
            ));
        }
        return Ok((file.mime, file.bytes));
    }
    if reference.starts_with("data:image/") {
        return decode_data_uri(reference);
    }
    if reference.starts_with("https://") || reference.starts_with("http://") {
        if let Err(rejection) = crate::net_guard::validate_outbound_url(reference, false) {
            return Err(ImageToolError::bad_request(format!(
                "参考图 URL 不被允许：{}",
                rejection.message()
            )));
        }
        let mut response = ctx
            .client
            .get(reference)
            .send()
            .await
            .map_err(|error| ImageToolError::bad_request(format!("参考图下载失败：{error}")))?;
        if !response.status().is_success() {
            return Err(ImageToolError::bad_request(format!(
                "参考图下载失败：HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length as usize > MAX_REFERENCE_BYTES)
        {
            return Err(ImageToolError::new(
                413,
                "invalid_image_input",
                "单张参考图不得超过 10 MiB",
            ));
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| ImageToolError::bad_request(format!("参考图下载失败：{error}")))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_REFERENCE_BYTES {
                return Err(ImageToolError::new(
                    413,
                    "invalid_image_input",
                    "单张参考图不得超过 10 MiB",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let mime = image_mime(&bytes, "参考图")?;
        return Ok((mime.to_string(), bytes));
    }
    Err(ImageToolError::bad_request(
        "参考图必须是 image:<id>、公网 http(s) URL 或 data:image/... 数据 URI；HTTP MCP 不能读取本机文件路径（请使用本地桥接）",
    ))
}

fn decode_data_uri(value: &str) -> Result<(String, Vec<u8>), ImageToolError> {
    let rest = value
        .strip_prefix("data:")
        .ok_or_else(|| ImageToolError::bad_request("参考图 data URL 无效"))?;
    let (meta, encoded) = rest
        .split_once(',')
        .ok_or_else(|| ImageToolError::bad_request("参考图 data URL 无效"))?;
    if !meta.starts_with("image/") || !meta.ends_with(";base64") {
        return Err(ImageToolError::bad_request(
            "参考图必须使用 data:image/...;base64 格式",
        ));
    }
    if encoded.len() > MAX_REFERENCE_BYTES.div_ceil(3) * 4 {
        return Err(ImageToolError::new(
            413,
            "invalid_image_input",
            "单张参考图不得超过 10 MiB",
        ));
    }
    let mime = meta.trim_end_matches(";base64").to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| ImageToolError::bad_request("参考图 base64 无效"))?;
    if bytes.len() > MAX_REFERENCE_BYTES {
        return Err(ImageToolError::new(
            413,
            "invalid_image_input",
            "单张参考图不得超过 10 MiB",
        ));
    }
    image_mime(&bytes, "参考图")?;
    Ok((mime, bytes))
}

fn image_mime(bytes: &[u8], label: &str) -> Result<&'static str, ImageToolError> {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => Ok("image/png"),
        Ok(image::ImageFormat::Jpeg) => Ok("image/jpeg"),
        Ok(image::ImageFormat::WebP) => Ok("image/webp"),
        _ => Err(ImageToolError::bad_request(format!(
            "{label}仅支持 PNG、JPEG、WebP"
        ))),
    }
}

/* ─── 执行 ─────────────────────────────────────────────────── */

struct ExecutionFailure {
    uncertain: bool,
    message: String,
}

/// 生成型请求的换渠道安全判定（与 `images.rs::images_failover_safe` 同语义）：
/// 在途丢失类错误意味着上游可能已出图已计费，不换渠道重发。
fn images_failover_safe(error: &crate::bridge::BridgeError) -> bool {
    use crate::bridge::BridgeError;
    if matches!(
        error,
        BridgeError::Timeout { .. }
            | BridgeError::Transport(_)
            | BridgeError::StreamAborted
            | BridgeError::UpstreamDecode(_)
            | BridgeError::AllAccountsFailed(_)
    ) {
        return false;
    }
    crate::api::openai::is_retryable_bridge_error(error)
}

async fn execute(
    state: &AppState,
    operation: ImageOperation,
    target: &ImageTarget,
    input: &PreparedInput,
    timeout: u64,
) -> Result<Value, ExecutionFailure> {
    // 供应商限制：只允许配置里选中的 provider（账号型渠道由 AccountBridge
    // 的 allowed_providers 在账号级过滤）。
    let allowed = vec![target.provider.clone()];
    let needs = match operation {
        ImageOperation::Generate => crate::api::openai::image_generations(),
        ImageOperation::Edit => crate::api::openai::image_edits(),
    };
    let candidates = crate::api::openai::resolve_bridges_needs(
        state,
        &target.model,
        Some(allowed.as_slice()),
        None,
        needs,
    );
    if candidates.is_empty() {
        return Err(ExecutionFailure {
            uncertain: false,
            message: "没有可用的生图渠道或账号，请检查图片服务配置".into(),
        });
    }
    let mut ctx = BridgeContext::new(
        format!("tool-img-{}", uuid::Uuid::new_v4()),
        target.model.clone(),
    );
    ctx.deadline = Some(Duration::from_secs(timeout.max(1)));
    let mut attempts: u32 = 0;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    let mut last_uncertain = false;
    for (bridge, channel_id, channel) in candidates {
        if let Some(id) = &channel_id {
            state.channel_store.mark_used(id);
        }
        let upstream = crate::api::openai::resolve_upstream_model(
            &target.model,
            channel.as_ref(),
            &state.model_mapper,
        );
        attempts += 1;
        let outcome = match input {
            PreparedInput::Generate(payload) => {
                let mut body = payload.clone();
                body["model"] = Value::String(upstream);
                tokio::time::timeout(
                    Duration::from_secs(timeout.max(1)),
                    bridge.generate_image(&body, &ctx),
                )
                .await
                .map_err(|_| crate::bridge::BridgeError::Timeout {
                    elapsed_ms: timeout.max(1) * 1000,
                    cause: "工具服务生图超时".into(),
                })
                .and_then(|result| result)
            }
            PreparedInput::Edit(parts) => {
                let mut attempt_parts = parts.clone();
                for part in &mut attempt_parts {
                    if let ImageEditFormPart::Text { name, value } = part {
                        if *name == "model" {
                            *value = upstream.clone();
                        }
                    }
                }
                tokio::time::timeout(
                    Duration::from_secs(timeout.max(1)),
                    bridge.edit_image(&attempt_parts, &ctx),
                )
                .await
                .map_err(|_| crate::bridge::BridgeError::Timeout {
                    elapsed_ms: timeout.max(1) * 1000,
                    cause: "工具服务生图超时".into(),
                })
                .and_then(|result| result)
            }
        };
        match outcome {
            Ok(response) => return Ok(response),
            Err(error) => {
                let status = error.http_status();
                last_uncertain = !matches!(status, 400 | 401 | 403 | 404 | 422 | 429);
                if !images_failover_safe(&error) {
                    return Err(ExecutionFailure {
                        uncertain: attempts > 0 && last_uncertain,
                        message: error.to_string(),
                    });
                }
                if let Some(id) = &channel_id {
                    state.channel_store.mark_cooldown(id, error.to_string(), 60);
                }
                last_error = Some(error);
            }
        }
    }
    Err(ExecutionFailure {
        uncertain: attempts > 0 && last_uncertain,
        message: last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "全部生图渠道均不可用".into()),
    })
}

async fn run_job(
    state: AppState,
    jobs: ImageJobs,
    id: String,
    operation: ImageOperation,
    target: ImageTarget,
    input: PreparedInput,
    timeout: u64,
) {
    match execute(&state, operation, &target, &input, timeout).await {
        Ok(response) => match prepare_files(response) {
            Ok((metadata, files)) => {
                if let Err(error) = jobs.finish(&id, metadata, files) {
                    let _ = jobs.fail(
                        &id,
                        true,
                        &format!(
                            "图片已生成，但结果保存失败：{}；请勿重新提交",
                            error.message
                        ),
                    );
                }
            }
            Err(error) => {
                let _ = jobs.fail(
                    &id,
                    true,
                    &format!(
                        "图片已生成，但结果处理失败：{}；请勿重新提交",
                        error.message
                    ),
                );
            }
        },
        Err(failure) => {
            tracing::warn!("[Tools] 图片任务 {id}：{}", failure.message);
            let _ = jobs.fail(&id, failure.uncertain, &failure.message);
        }
    }
}

/// 上游 `data[].b64_json` → 本地图片文件（+ 结果元信息白名单字段）。
fn prepare_files(response: Value) -> Result<(Value, Vec<ImageFile>), ImageToolError> {
    let items = response["data"]
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or_else(|| ImageToolError::new(500, "image_result_invalid", "上游未返回图片"))?;
    let mut files = Vec::with_capacity(items.len());
    for item in items {
        let encoded = item["b64_json"].as_str().ok_or_else(|| {
            ImageToolError::new(500, "image_result_invalid", "生图结果缺少图片字节")
        })?;
        if encoded.len() > (image_jobs::MAX_IMAGE_BYTES as usize).div_ceil(3) * 4 {
            return Err(ImageToolError::new(
                500,
                "image_result_invalid",
                "生成图片过大",
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| ImageToolError::new(500, "image_result_invalid", "图片编码无效"))?;
        let format = image::guess_format(&bytes)
            .map_err(|_| ImageToolError::new(500, "image_result_invalid", "图片格式无效"))?;
        let mime = match format {
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Jpeg => "image/jpeg",
            image::ImageFormat::WebP => "image/webp",
            _ => {
                return Err(ImageToolError::new(
                    500,
                    "image_result_invalid",
                    "生图结果仅支持 PNG、JPEG、WebP",
                ))
            }
        };
        let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
            .into_dimensions()
            .map_err(|_| ImageToolError::new(500, "image_result_invalid", "图片尺寸无效"))?;
        files.push(ImageFile {
            id: image_jobs::random_id()?,
            mime: mime.to_string(),
            width,
            height,
            bytes,
        });
    }
    let mut metadata = json!({});
    for key in [
        "created",
        "usage",
        "points_consumed",
        "size",
        "quality",
        "output_format",
    ] {
        if let Some(value) = response.get(key) {
            metadata[key] = value.clone();
        }
    }
    Ok((metadata, files))
}

/* ─── 结果形状（与上游 tool_image_results 对齐） ───────────── */

pub fn job_result(job: &ImageJob, base: &str, management: bool) -> Value {
    let mut result = job.result.clone().unwrap_or_else(|| json!({}));
    if !management {
        if let Some(object) = result.as_object_mut() {
            object.remove("actual_account_id");
            object.remove("actual_account_name");
        }
    }
    result["job_id"] = json!(job.id);
    result["request_id"] = json!(job.request_id);
    result["status"] = json!(job.state);
    result["operation"] = json!(job.operation);
    result["model"] = json!(job.model);
    result["provider"] = json!(job.provider);
    result["created_at"] = json!(job.created_at);
    result["updated_at"] = json!(job.updated_at);
    result["expires_at"] = json!(job.expires_at);
    result["deduplicated_until"] = json!(job.expires_at + 7 * 24 * 60 * 60 * 1000);
    result["error"] = json!(job.error);
    if management {
        result["account_id"] = json!(job.account_id);
    }
    result["poll_after_seconds"] = if job.state == "running" {
        json!(5)
    } else {
        Value::Null
    };
    result["guidance"] = json!(match job.state.as_str() {
        "running" => "Generation is in progress. Call get_image_result with this job_id; do not submit again.",
        "succeeded" => "Images are ready. Download using the same gateway API key before expires_at. HTTP mode has not saved any files on the client machine.",
        "expired" => "The result has expired or was cleared. It has not been regenerated. Ask the user before creating a new job.",
        "uncertain" => "The upstream may have consumed credits. The result is uncertain; do not automatically generate again.",
        _ => "Generation did not complete. No automatic resubmission was made; ask the user before starting a new job.",
    });
    if let Some(images) = result.get_mut("images").and_then(Value::as_array_mut) {
        for image in images {
            let id = image["id"].as_str().unwrap_or("").to_string();
            let prefix = if management {
                MANAGEMENT_FILE_PREFIX
            } else {
                MCP_FILE_PREFIX
            };
            image["url"] = json!(format!("{}{prefix}/{id}", base.trim_end_matches('/')));
            image["ref"] = json!(format!("image:{id}"));
            image["requires_auth"] = json!(true);
        }
    }
    result
}

pub fn mcp_result(
    jobs: &ImageJobs,
    job: &ImageJob,
    base: &str,
    preview: bool,
) -> Result<Value, ImageToolError> {
    let mut payload = job_result(job, base, false);
    let mut preview_content = None;
    if preview && job.state == "succeeded" {
        if let Some(id) = payload
            .pointer("/images/0/id")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            match jobs.read_file(&id).and_then(|(_, file)| thumbnail(&file)) {
                Ok(data) => {
                    preview_content =
                        Some(json!({ "type": "image", "data": data, "mimeType": "image/png" }))
                }
                Err(_) => {
                    payload["preview_notice"] = json!(
                        "Preview unavailable; download the original image. Do not generate again."
                    )
                }
            }
        }
    }
    let mut content = vec![
        json!({ "type": "text", "text": serde_json::to_string(&payload).unwrap_or_default() }),
    ];
    if let Some(image) = preview_content {
        content.push(image);
    }
    Ok(json!({
        "resultType": "complete",
        "content": content,
        "structuredContent": payload,
        "isError": !matches!(job.state.as_str(), "running" | "succeeded"),
    }))
}

fn thumbnail(file: &ImageFile) -> Result<String, ImageToolError> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&file.bytes))
        .with_guessed_format()
        .map_err(|_| ImageToolError::new(500, "image_result_invalid", "图片预览格式无效"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| ImageToolError::new(500, "image_result_invalid", "图片预览解码失败"))?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    decoded
        .thumbnail(512, 512)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|_| ImageToolError::new(500, "image_result_invalid", "图片预览编码失败"))?;
    if bytes.get_ref().len() > 2 * 1024 * 1024 {
        return Err(ImageToolError::new(
            500,
            "image_result_invalid",
            "图片预览过大",
        ));
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()))
}

pub fn file_response(file: ImageFile) -> Response {
    let extension = match file.mime.as_str() {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    };
    let mut response = Response::new(Body::from(file.bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&file.mime)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"image-{}.{extension}\"",
        file.id
    )) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/* ─── 错误形状 ─────────────────────────────────────────────── */

/// 数据面错误响应：`{ "error": { "code", "message" } }`。
pub fn error_response(error: ImageToolError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        axum::Json(json!({ "error": { "code": error.code, "message": error.message } })),
    )
        .into_response()
}

/// MCP `isError` 结果：保留 code/message/request_id/job_id + 重试指引。
pub fn error_result(error: &ImageToolError, args: &Value) -> Value {
    let payload = json!({
        "status": "error",
        "code": error.code,
        "message": error.message,
        "request_id": args.get("request_id"),
        "job_id": args.get("job_id"),
        "guidance": "Do not automatically submit a new generation. Keep the original request_id; use get_image_result after an uncertain submission.",
    });
    json!({
        "resultType": "complete",
        "isError": true,
        "content": [{ "type": "text", "text": payload.to_string() }],
        "structuredContent": payload,
    })
}

/// 供管理面复用的错误 → `(StatusCode, Json)` 映射。
pub fn admin_error(error: ImageToolError) -> (StatusCode, axum::Json<Value>) {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        axum::Json(json!({ "success": false, "error": error.message, "code": error.code })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::AccountSessionStore;
    use crate::api::auth::ApiKeyStore;
    use crate::storage::FileStore;
    use crate::tool_services::image_config::{ImageConfig, ImageTarget};
    use crate::tool_services::ToolServices;
    use std::sync::Arc;

    fn args(value: Value) -> Value {
        value
    }

    /// 单测用调用上下文：`state: None` 时只有「需要完整应用状态」的分支不可用，
    /// 参数/配置/查询错误路径都能照常覆盖（生产路径始终是 `Some`）。
    struct CallHarness {
        services: ToolServices,
        sessions: AccountSessionStore,
        keys: ApiKeyStore,
        client: reqwest::Client,
        _dir: tempfile::TempDir,
    }

    impl CallHarness {
        fn new(editing: bool) -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
            let target = ImageTarget {
                provider: "workbuddy".into(),
                model: "glm-image".into(),
                account_id: String::new(),
            };
            let config = ImageConfig {
                generation: target.clone(),
                editing: editing.then_some(target),
                max_images: 2,
                max_concurrency: 2,
                retention_hours: 24,
                storage_mb: 512,
            };
            let services = ToolServices::new(store.clone());
            services
                .update(|value| value.image_generation = Some(config))
                .unwrap();
            Self {
                sessions: AccountSessionStore::new(store.clone()),
                keys: ApiKeyStore::new(store, dir.path().to_path_buf()),
                services,
                client: crate::test_http_client(),
                _dir: dir,
            }
        }

        fn ctx(&self) -> McpRequestContext<'_> {
            McpRequestContext {
                services: &self.services,
                sessions: &self.sessions,
                keys: &self.keys,
                client: &self.client,
                base_url: "http://127.0.0.1".into(),
                allowed_providers: None,
                allowed_models: None,
                owner: "key-1".into(),
                state: None,
            }
        }
    }

    #[tokio::test]
    async fn generate_rejects_bad_request_id_before_state() {
        let harness = CallHarness::new(false);
        let ctx = harness.ctx();
        let error = call(
            "generate_images",
            &json!({ "request_id": "short", "prompt": "a cat" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "invalid_image_input");
    }

    #[tokio::test]
    async fn edit_disabled_is_configuration_error() {
        let harness = CallHarness::new(false);
        let ctx = harness.ctx();
        let error = call(
            "edit_images",
            &json!({
                "request_id": "request-0001",
                "prompt": "add a hat",
                "images": ["https://example.com/a.png"]
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "image_configuration");
        assert!(error.message.contains("尚未启用"));
    }

    #[tokio::test]
    async fn edit_enabled_reaches_execution_and_needs_state() {
        let harness = CallHarness::new(true);
        let ctx = harness.ctx();
        let error = call(
            "edit_images",
            &json!({
                "request_id": "request-0001",
                "prompt": "add a hat",
                "images": ["https://example.com/a.png"]
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "image_runtime_unavailable");
    }

    #[tokio::test]
    async fn query_missing_job_is_not_found() {
        let harness = CallHarness::new(false);
        let ctx = harness.ctx();
        let error = call(
            "get_image_result",
            &json!({ "job_id": "0".repeat(64) }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "image_job_not_found");
    }

    /// 同名 request_id 但请求哈希不同 → 冲突，不重复生图。
    #[test]
    fn same_request_rejects_hash_mismatch() {
        let job = ImageJob {
            id: "j".into(),
            owner: "key-1".into(),
            request_id: "request-0001".into(),
            request_hash: "hash-a".into(),
            operation: "generate".into(),
            provider: "workbuddy".into(),
            model: "glm-image".into(),
            account_id: String::new(),
            state: "running".into(),
            created_at: 0,
            updated_at: 0,
            expires_at: i64::MAX,
            deadline_at: 0,
            reserved_bytes: 0,
            result: None,
            error: None,
        };
        assert_eq!(same_request(job.clone(), "hash-a").unwrap().id, "j");
        assert_eq!(
            same_request(job, "hash-b").unwrap_err().code,
            "image_request_conflict"
        );
    }

    /// MCP spec 之外的纯参数校验：request_id 形状 / wait_seconds / preview。
    #[test]
    fn submission_id_validation() {
        assert!(submission_id(&args(json!({ "request_id": "short" }))).is_err());
        assert!(submission_id(&args(json!({ "request_id": "bad id with spaces" }))).is_err());
        assert_eq!(
            submission_id(&args(json!({ "request_id": "request-0001" }))).unwrap(),
            "request-0001"
        );
    }

    #[test]
    fn options_bounds_and_defaults() {
        assert_eq!(options(&json!({}), false).unwrap(), (10, false));
        assert_eq!(options(&json!({}), true).unwrap(), (0, false));
        assert!(options(&json!({ "wait_seconds": 21 }), false).is_err());
        assert!(options(&json!({ "preview": "yes" }), true).is_err());
        assert!(options(&json!({ "job_id": "x" }), false).is_err());
    }

    /// 任务归属鉴权：owner 不符 / provider 或 model 不在 Key 白名单 → 404。
    #[test]
    fn authorize_job_checks_owner_and_scope() {
        let job = ImageJob {
            id: "j".into(),
            owner: "key-1".into(),
            request_id: "request-0001".into(),
            request_hash: "h".into(),
            operation: "generate".into(),
            provider: "workbuddy".into(),
            model: "glm-image".into(),
            account_id: String::new(),
            state: "succeeded".into(),
            created_at: 0,
            updated_at: 0,
            expires_at: i64::MAX,
            deadline_at: 0,
            reserved_bytes: 0,
            result: None,
            error: None,
        };
        assert!(authorize_job(&job, "key-1", None, None).is_ok());
        assert!(authorize_job(&job, "key-2", None, None).is_err());
        assert!(authorize_job(
            &job,
            "key-1",
            Some(&["openai_compatible".to_string()]),
            None
        )
        .is_err());
        assert!(authorize_job(
            &job,
            "key-1",
            Some(&["workbuddy".to_string()]),
            Some(&["glm-image".to_string()])
        )
        .is_ok());
    }

    #[test]
    fn data_uri_roundtrip_and_rejects() {
        // 1x1 PNG（与 image_jobs 单测同一字节序列）
        const PNG: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(PNG)
        );
        let (mime, bytes) = decode_data_uri(&uri).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, PNG);
        // 非图片字节 / 非 base64 编码都拒绝
        assert!(decode_data_uri("data:image/png;base64,aGVsbG8=").is_err());
        assert!(decode_data_uri("data:image/png,not-base64").is_err());
        assert!(decode_data_uri("data:text/plain;base64,aGVsbG8=").is_err());
    }
}
