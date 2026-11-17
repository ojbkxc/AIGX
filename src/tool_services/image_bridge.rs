// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成 MCP 本地桥接脚本（stdio，Node >= 18）。
//!
//! 与视觉桥接同一边界：网关只**下发脚本**（`/api/tools/mcp-bridge/image-generation.cjs`），
//! 由客户端以 `node image-generation-bridge.cjs` 的 stdio MCP 形态挂载。
//! 脚本负责三件 HTTP 模式做不到的事：
//!   1. `edit_images` 的本地绝对路径参考图（读文件转 data URI，限制在管理员
//!      授权的输出 / 参考目录内，拒绝符号链接）；
//!   2. 生成完成后把图片下载到授权输出目录并返回**真实本地路径**
//!      （同名不同内容拒绝覆盖）；
//!   3. 提交/查询响应丢失时保留 `uncertain` 语义，提示用原 request_id 查询。
//!
//! 配置经环境变量注入（由面板写入客户端 MCP 配置时生成，命名沿用上游
//! `AGENT2API_IMAGE_*` 以免已接入客户端失效）：
//!   AGENT2API_IMAGE_ENDPOINT        远程 MCP 端点完整 URL（含 /mcp/image-generation）
//!   AGENT2API_IMAGE_KEY             网关 API Key（Bearer）
//!   AGENT2API_IMAGE_OUTPUT_DIR      授权输出目录（绝对路径）
//!   AGENT2API_IMAGE_REFERENCE_DIRS  额外参考目录（JSON 数组，最多 16 个）
//!
//! 上游 `image_bridge.rs` 的 Node 探测 / 脚本落盘 / 客户端配置注入属于安装
//! 流程，AIGX 当前的安装模块（`tool_services::install`，非本波改动面）尚未
//! 接入 image-generation；本文件只提供脚本本体，接入请求见报告。

/// 桥接脚本正文（`include_str!`，与上游 `image_bridge.cjs` 逐字一致）。
pub const IMAGE_BRIDGE_SCRIPT: &str = include_str!("image_bridge.cjs");
