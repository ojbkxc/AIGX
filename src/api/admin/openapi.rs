//! 轻量 OpenAPI 自描述端点（AI 可运维第①步）。
//!
//! `GET /api/openapi.json` 返回管理面全部真实路由的 OpenAPI 3.1 文档，
//! 让运维 AI 自发现全部管理 API，无需翻源码。
//!
//! 技术决策（已定）：零依赖静态构造——serde_json::json! 手写，不引入
//! utoipa（新增依赖须 Ask first）。按域分函数组织 paths，保持可维护。
//!
//! 硬规矩：**每个 path 必须在 main.rs 真实路由表中存在，不许虚构**；
//! 单测 `covers_all_real_admin_routes` 硬编码 main.rs 路由表对照清单，
//! 路由增删后该测试红——这是文档与实现同步的机器化契约。
//!
//! 范围：管理面（/api/* + /api-docs + /swagger-ui）。数据面 /v1/* 不在
//! 本版（OpenAI/Anthropic 兼容协议自描述，见 info.description 说明）。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::{json, Map, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

/// GET /api/openapi.json — 管理面 API 自描述文档。
///
/// API 文档本身不泄数据，但暴露 API 面形状——跟随管理面默认鉴权
/// （与 legacy 的无鉴权 `/api-docs/openapi.json` 相比收紧了暴露面）。
pub async fn handle_openapi_v31(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(build_openapi_document()))
}

/// 构造 OpenAPI 3.1 文档（纯函数，供端点与单测共用）。
pub fn build_openapi_document() -> Value {
    let mut paths = Map::new();
    for part in [
        paths_auth(),
        paths_users(),
        paths_user_tools(),
        paths_channels(),
        paths_tokens(),
        paths_pricing(),
        paths_model_manage(),
        paths_checkin_center(),
        paths_zcode_captcha(),
        paths_report(),
        paths_payment(),
        paths_logs(),
        paths_dashboard_usage(),
        paths_settings_ops(),
        paths_monitor_security(),
        paths_playground(),
        paths_accounts_network(),
        paths_docs(),
        paths_diagnostics(),
        paths_mcp(),
    ] {
        if let Value::Object(m) = part {
            for (k, v) in m {
                paths.insert(k, v);
            }
        }
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "AIGX Admin API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "AIGX 管理面 API 自描述文档（AI 原生可运维）。 \
                全部端点随管理面会话鉴权（Bearer），写操作另需对应权限。 \
                数据面协议不在本文档：OpenAI 兼容见 GET /v1/models、 \
                Anthropic 兼容见 POST /v1/messages（sk-xxx 密钥）。 \
                路径参数以 {id} 形式表示，实际注册路由为 axum 的 :id 语法。"
        },
        "servers": [{ "url": "/", "description": "当前服务器" }],
        "security": [{ "bearerAuth": [] }],
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "管理面会话 token（Authorization: Bearer {token}，登录 /api/auth/login 获取）"
                }
            }
        },
        "paths": Value::Object(paths),
    })
}

// ── 操作构造 helper（压缩重复，保持各域函数一行一端点） ────────────

/// 管理面标准操作（带 Bearer 鉴权标注 + 统一响应码示意）。
fn op(tag: &str, summary: &str) -> Value {
    json!({
        "tags": [tag],
        "summary": summary,
        "security": [{ "bearerAuth": [] }],
        "responses": {
            "200": { "description": "操作成功" },
            "401": { "description": "未认证/会话失效" },
            "403": { "description": "权限不足" }
        }
    })
}

/// 公开端点（登录/注册/回调等；`security: []` 覆盖全局默认，表示无鉴权）。
fn op_pub(tag: &str, summary: &str) -> Value {
    json!({
        "tags": [tag],
        "summary": summary,
        "security": [],
        "responses": { "200": { "description": "操作成功" } }
    })
}

/// 管理面操作 + query/path 参数。
fn op_params(tag: &str, summary: &str, params: &[Value]) -> Value {
    let mut v = op(tag, summary);
    v["parameters"] = json!(params);
    v
}

/// 管理面写操作（请求体结构从简：字段名 + 类型一句话说明）。
fn op_body(tag: &str, summary: &str, body: &str) -> Value {
    let mut v = op(tag, summary);
    v["requestBody"] = json!({
        "required": true,
        "content": { "application/json": { "schema": { "type": "object" } } },
        "description": body
    });
    v
}

/// 路径参数条目。
fn path_param(name: &str) -> Value {
    json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" } })
}

/// query 参数条目。
fn query_param(name: &str, ty: &str, desc: &str) -> Value {
    json!({ "name": name, "in": "query", "schema": { "type": ty }, "description": desc })
}

// ── 按域构造 paths（每个 path 都对应 main.rs 真实注册路由） ──────────

fn paths_auth() -> Value {
    json!({
        "/api/auth/login": { "post": op_pub("认证", "用户登录（返回管理会话 token）") },
        "/api/auth/captcha": { "get": op_pub("认证", "领取机器人校验题（ALTCHA challenge；开关关闭时 400，前端据此隐藏校验组件）") },
        "/api/auth/login/send-code": { "post": op_pub("认证", "发送邮箱验证码（验证码登录前置）") },
        "/api/auth/login/code": { "post": op_pub("认证", "邮箱验证码登录") },
        "/api/auth/login/totp": { "post": op_pub("认证", "TOTP 两步验证登录（密码通过后携码换会话）") },
        "/api/auth/totp/setup": { "post": op_body("认证", "开通 TOTP 两步验证", "无请求体，返回待确认的 secret") },
        "/api/auth/totp/enable": { "post": op_body("认证", "确认启用 TOTP", "code: string（6 位验证码）") },
        "/api/auth/totp/disable": { "post": op_body("认证", "关闭 TOTP 两步验证", "password: string") },
        "/api/auth/register": { "post": op_pub("认证", "用户注册（邮箱+密码）") },
        "/api/auth/forgot-password": { "post": op_pub("认证", "忘记密码（生成重置 token）") },
        "/api/auth/reset-password": { "post": op_pub("认证", "凭重置 token 重置密码") },
        "/api/auth/change-password": { "post": op_body("认证", "修改本人密码", "old_password/new_password: string") },
        "/api/auth/logout": { "post": op("认证", "登出（撤销会话）") },
        "/api/session": { "get": op_pub("认证", "首屏登录态概况（configured/authenticated/expiresAt；未登录不报错，email 仅在已认证时返回）") },
        "/api/session/refresh": { "post": op("认证", "刷新当前登录态（同 email 续签新 token，撤销旧 session_id）") },
        "/api/auth/github": { "get": op_pub("认证", "GitHub OAuth 授权跳转") },
        "/api/auth/github/callback": { "get": op_pub("认证", "GitHub OAuth 回调") },
        "/api/auth/google": { "get": op_pub("认证", "Google OAuth 授权跳转") },
        "/api/auth/google/callback": { "get": op_pub("认证", "Google OAuth 回调") },
        "/api/auth/linuxdo": { "get": op_pub("认证", "LinuxDO OAuth 授权跳转") },
        "/api/auth/linuxdo/callback": { "get": op_pub("认证", "LinuxDO OAuth 回调") },
        "/api/auth/github/bind/start": {
            "post": op_body("认证", "发起绑定 GitHub（登录态）", "无请求体，返回授权 URL")
        },
        "/api/auth/google/bind/start": {
            "post": op_body("认证", "发起绑定 Google（登录态）", "无请求体，返回授权 URL")
        },
        "/api/auth/linuxdo/bind/start": {
            "post": op_body("认证", "发起绑定 LinuxDO（登录态）", "无请求体，返回授权 URL")
        },
        "/api/auth/oauth/unbind": {
            "post": op_body("认证", "解绑第三方账号（登录态）", "provider: 'github' | 'google' | 'linuxdo'")
        },
        "/api/auth/oauth/consume": {
            "post": op_pub("认证", "OAuth 一次性 ticket 换会话数据")
        }
    })
}

fn paths_users() -> Value {
    json!({
        "/api/users": {
            "get": op_params("用户", "列出用户（分页）", &[query_param("page", "integer", "页码（默认 1）"), query_param("size", "integer", "每页条数（默认 20）")]),
            "post": op_body("用户", "创建用户", "email/username/password: string, quota: integer, role: string")
        },
        "/api/users/check": {
            "get": op_params("用户", "检查用户名/邮箱是否可用", &[query_param("username", "string", "待检查的用户名或邮箱")])
        },
        "/api/users/me": { "get": op("用户", "当前登录用户信息") },
        "/api/users/self": {
            "put": op_body("用户", "更新本人资料", "可改字段: username/password 等")
        },
        "/api/users/manage": {
            "post": op_body("用户", "管理操作（封禁/解封/调整配额）", "user_id: string, action: string, value 可选")
        },
        "/api/users/{id}": {
            "put": op_params("用户", "更新指定用户", &[path_param("id")]),
            "delete": op_params("用户", "删除指定用户", &[path_param("id")])
        },
        "/api/users/{id}/2fa": {
            "delete": op_params("用户", "管理员强制禁用用户 2FA", &[path_param("id")])
        },
        "/api/aff": { "get": op("用户", "获取本人邀请返利码") },
        "/api/aff_transfer": {
            "post": op_body("用户", "划转返利余额到主配额", "amount: integer")
        },
        "/api/chat/sessions": {
            "get": op("用户", "列出我的聊天会话（按用户隔离的对话记录）"),
            "post": op_body("用户", "新建聊天会话", "title/model 可选")
        },
        "/api/chat/sessions/{id}": {
            "get": op_params("用户", "会话详情 + 全部消息", &[path_param("id")]),
            "delete": op_params("用户", "删除会话及其消息", &[path_param("id")])
        },
        "/api/chat/sessions/{id}/rename": {
            "post": op_body("用户", "重命名会话", "title: string")
        },
        "/api/chat/sessions/{id}/messages": {
            "post": op_body("用户", "追加一条消息", "role/content/extra"),
            "put": op_body("用户", "整表替换消息（编辑/重发后 debounce 保存）", "messages: [{role, content, extra}]")
        },
        "/api/chat/sessions/{id}/fork": {
            "post": op_params("用户", "从指定消息处分叉出新会话（保留前序上下文）", &[path_param("id")])
        },
        "/api/user/records": {
            "get": op_params("用户", "本人积分流水（签到获得 + API 消耗合并时间线）", &[
                query_param("page", "integer", "页码（默认 1）"),
                query_param("size", "integer", "每页条数（默认 20）")
            ])
        },
        "/api/checkin": {
            "get": op("用户", "查询今日签到状态"),
            "post": op("用户", "执行每日签到（送配额）")
        }
    })
}

/// 用户工具策略（#34 §5.1，喂 #65 UserTools 配置页；配置 100% 管理员）。
fn paths_user_tools() -> Value {
    json!({
        "/api/user-tools/policy": {
            "get": op("用户工具", "读当前用户工具策略快照（enabled + 组 allowlist + 生效版本号）"),
            "put": op_body("用户工具", "全量更新用户工具策略（超限 409 + 原因；写后热更；回带保存后的策略）", "UserToolPolicy: { enabled: boolean, groups: { [组名]: { allowlist: string[] } } }，allowlist 含 \"*\" 表示该组获得 LowRisk 全集")
        },
        "/api/user-tools/catalog": {
            "get": op("用户工具", "工具目录（全部工具 + risk + has_user_exec + 当前策略命中状态 policy_hit）")
        }
    })
}

fn paths_channels() -> Value {
    json!({
        "/api/channels": {
            "get": op("渠道", "列出全部渠道（含脱敏密钥）"),
            "post": op_body("渠道", "新增渠道", "name/channel_type/base_url/api_key/models 等渠道字段")
        },
        "/api/channels/{id}": {
            "put": op_params("渠道", "更新渠道", &[path_param("id")]),
            "patch": op_params("渠道", "部分更新渠道字段", &[path_param("id")]),
            "delete": op_params("渠道", "删除渠道", &[path_param("id")])
        },
        "/api/channels/{id}/patch": {
            "patch": op_params("渠道", "部分更新渠道（别名路径）", &[path_param("id")])
        },
        "/api/channels/{id}/test": {
            "post": op_params("渠道", "单渠道连通性测试（结果落库并联动启停）", &[path_param("id")])
        },
        "/api/channels/{id}/balance": {
            "get": op_params("渠道", "查询上游真实余额（仅 OpenAI 兼容渠道）", &[path_param("id")])
        },
        "/api/channels/{id}/health-archive": {
            "get": op_params("渠道", "渠道 30 天健康档案（成功率/P95/熔断次数）", &[path_param("id")])
        },
        "/api/channels/{id}/reset-circuit": {
            "post": op_params("渠道", "手动重置渠道断路器（写操作）", &[path_param("id")])
        },
        "/api/channels/fetch_models": {
            "post": op_body("渠道", "拉取上游模型列表（自动发现）", "channel_id/channel_type/base_url/api_key: string")
        },
        "/api/channels/chat_test": {
            "post": op_body("渠道", "渠道对话测试（真实上游调用）", "channel_id/model/prompt: string")
        },
        "/api/models/available": {
            "get": op("渠道", "用户侧可用模型聚合（启用渠道 models 并集）")
        },
        "/api/models/meta": { "get": op("渠道", "模型元信息覆盖列表") },
        "/api/models/meta/{model}": {
            "put": op_params("渠道", "设置模型元信息覆盖", &[path_param("model")]),
            "delete": op_params("渠道", "删除模型元信息覆盖", &[path_param("model")])
        }
    })
}

fn paths_tokens() -> Value {
    json!({
        "/api/tokens": {
            "get": op_params("令牌", "列出令牌（本人；管理员可携 all/user 过滤）。响应附 providers/modelsByProvider 候选表（白名单选择器数据源）；令牌对象含 allowed_models 与 allowed_providers（provider 白名单，None/空=不限）",
                &[query_param("all", "string", "管理员查看全部（任意非空值生效）"), query_param("user", "string", "按邮箱/UUID 筛选（仅管理员）")]),
            "post": op_body("令牌", "创建令牌", "name: string, allowed_models/allowed_providers/expired_time/remain_quota 等可选字段")
        },
        "/api/tokens/{id}": {
            "put": op_params("令牌", "更新令牌（含 allowed_providers 白名单；空数组=不限）", &[path_param("id")]),
            "delete": op_params("令牌", "删除令牌", &[path_param("id")])
        },
        "/api/tokens/{id}/key": {
            "get": op_params("令牌", "取回令牌明文密钥", &[path_param("id")])
        },
        "/api/tokens/{id}/rotate": {
            "post": op_params("令牌", "轮换令牌密钥", &[path_param("id")])
        },
        "/api/tokens/{id}/reset_used": {
            "post": op_params("令牌", "清零令牌已用额度", &[path_param("id")])
        },
        "/api/tokens/today": { "get": op("令牌", "今日用量摘要（token 维度）") }
        // legacy /api/keys 系列已删除：前端全量收敛到 /api/tokens（处理函数
        // 保留，运维 Agent 工具面仍以库函数复用）
    })
}

fn paths_pricing() -> Value {
    json!({
        "/api/pricing": {
            "get": op("定价", "列出模型定价目录"),
            "post": op_body("定价", "新增/更新模型定价", "model: string, input_price/output_price: number")
        },
        "/api/pricing/{id}": {
            "delete": op_params("定价", "删除定价条目", &[path_param("id")])
        },
        "/api/pricing/estimate": {
            "post": op_body("定价", "费用试算", "model/input_tokens/output_tokens 等估算字段")
        },
        "/api/pricing/exchange-rates": {
            "get": op("定价", "查询多币种汇率"),
            "put": op_body("定价", "更新汇率", "rates: object（币种代码 → 汇率）")
        },
        "/api/pricing/missing-models": {
            "get": op("定价", "启用渠道模型 vs 定价目录差集（缺失定价检测）")
        },
        "/api/pricing/sync": {
            "post": op("定价", "触发多源定价同步")
        },
        "/api/pricing/sync-config": {
            "get": op("定价", "查询定价同步配置"),
            "put": op_body("定价", "更新定价同步配置", "同步源/开关等配置字段")
        },
        "/api/prices": {
            "get": op("定价", "列出模型价格（legacy 别名，deprecated —— 新接口走 /api/models/manage 与 /api/models/price）"),
            "post": op_body("定价", "写入模型价格（legacy 别名，deprecated）", "model: string, price 字段组")
        },
        "/api/prices/{model}": {
            "put": op_params("定价", "按模型写入价格", &[path_param("model")]),
            "delete": op_params("定价", "按模型删除价格", &[path_param("model")])
        },
        "/api/groups": {
            "get": op("定价", "列出用户分组"),
            "post": op_body("定价", "写入分组", "name: string, ratio 等分组属性")
        },
        "/api/groups/{name}": {
            "put": op_params("定价", "按名更新分组", &[path_param("name")]),
            "delete": op_params("定价", "按名删除分组", &[path_param("name")])
        },
        "/api/ratios": {
            "get": op("定价", "查询模型倍率"),
            "put": op_body("定价", "更新模型倍率", "ratios: object（模型 → 倍率）")
        },
        "/api/ratelimit/config": {
            "get": op("定价", "查询限流配置"),
            "put": op_body("定价", "更新限流配置", "多维度 RPM/TPM 配置字段")
        }
    })
}

fn paths_model_manage() -> Value {
    json!({
        "/api/models/manage": {
            "get": op("模型管理", "统一模型管理视图（渠道模型 + 账号池模型聚合，含启停/来源/定价/别名）")
        },
        "/api/models/state": {
            "post": op_body("模型管理", "按 (provider, model) 启停模型", "provider: string, id: string, enabled: bool")
        },
        "/api/models/test": {
            "post": op_body("模型管理", "模型测试：钉 provider+model（可选 account_id/channel_id）走真实链路发最小对话，返回用时/首字/用量/回复/错误", "provider/model: string, account_id?/channel_id?/prompt?/system_prompt?/reasoning_effort?/stream?")
        },
        "/api/models/price": {
            "post": op_body("模型管理", "写入模型定价与倍率", "model: string, input_price/output_price/cache_price/price_type/model_ratio")
        },
        "/api/models/price/remove": {
            "post": op_body("模型管理", "删除模型定价", "model: string")
        },
        "/api/models/alias": {
            "post": op_body("模型管理", "设置全局模型别名", "alias/target: string")
        },
        "/api/models/alias/state": {
            "post": op_body("模型管理", "启停全局模型别名（不改映射，只切是否生效）", "alias: string, enabled: bool")
        },
        "/api/models/capabilities": {
            "post": op_body("模型管理", "设置模型能力标签（vision/tools/reasoning 等）", "provider?: string, model: string, capabilities: object")
        },
        "/api/models/refresh": {
            "post": op("模型管理", "触发上游模型发现刷新（写操作）")
        },
        "/api/models/metadata": {
            "get": op("模型管理", "读取 models.dev 元数据同步开关与最近同步状态"),
            "put": op_body("模型管理", "更新 models.dev 元数据同步开关（写操作）", "enabled: bool")
        },
        "/api/models/metadata/refresh": {
            "post": op("模型管理", "立即拉取 models.dev 元数据（写操作）")
        },
        "/api/models/alias/remove": {
            "post": op_body("模型管理", "删除全局模型别名", "alias: string")
        },
        "/api/models/alias/reasoning": {
            "post": op_body("模型管理", "批量设置映射级思考等级", "aliases: string[], effort: string")
        },
        "/api/models/custom": {
            "post": op_body("模型管理", "添加自定义模型登记", "provider: string, id: string")
        },
        "/api/models/custom/remove": {
            "post": op_body("模型管理", "删除自定义模型登记", "provider: string, id: string")
        },
    })
}

/// 签到中心（管理员替上游账号签到，区别于用户侧 /api/checkin）。
///
/// 单独成函数而不是塞进 `paths_accounts_network`：那个函数已经很大，
/// `json!` 的宏展开深度在合并后触到默认 recursion_limit。
fn paths_checkin_center() -> Value {
    json!({
        "/api/checkin-center": {
            "get": op("签到中心", "总览快照：支持清单 + 今日汇总 + 账号签到状态")
        },
        "/api/checkin-center/records": {
            "get": op_params("签到中心", "签到台账（按天分桶、分页）", &[
                query_param("days", "integer", "回溯天数"),
                query_param("provider", "string", "按上游过滤"),
                query_param("outcome", "string", "按结果过滤（success/already/pending/failed）"),
                query_param("keyword", "string", "关键词（账号名/账号 id/上游/说明）"),
                query_param("offset", "integer", "分页偏移"),
                query_param("limit", "integer", "分页大小（默认 50，≤0 = 全部）"),
            ]),
            "delete": op_params("签到中心", "清空签到台账", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/checkin-center/records/mark-kept-active": {
            "post": op_body("签到中心", "把某账号当日最后一条记录标记「附带保活」", "account_id: string, provider: string")
        },
        "/api/checkin-center/run": {
            "post": op_body("签到中心", "立即签到（全部 / 指定账号 / 指定 provider）", "account_ids?: string[], providers?: string[]")
        },
        "/api/checkin-center/schedules": {
            "get": op("签到中心", "列出定时签到计划"),
            "post": op_body("签到中心", "新增或更新定时签到计划", "id/label/time: string, enabled: bool, providers: string[]")
        },
        "/api/checkin-center/schedules/remove": {
            "post": op_body("签到中心", "删除定时签到计划", "id: string")
        },
        "/api/checkin-center/schedules/run": {
            "post": op_body("签到中心", "立即执行某计划的范围", "id: string")
        },
        "/api/checkin-center/settings": {
            "get": op("签到中心", "读取签到设置"),
            "put": op_body("签到中心", "保存签到设置", "enabled: bool, gap_ms: integer")
        },
        "/api/checkin-center/benefits": {
            "get": op("签到中心", "一次性/限时福利通道清单（新手礼 / 保活 / 成长任务 / 限时套餐 / UMID）")
        },
        "/api/checkin-center/benefits/run": {
            "post": op_body("签到中心", "执行指定福利通道", "id: string, account_ids?: string[]")
        },
        "/api/checkin-center/benefits/keepalive": {
            "get": op("签到中心", "读取保活福利配置"),
            "put": op_body("签到中心", "保存保活福利配置", "enabled: bool, 间隔/范围等字段")
        },
    })
}

/// ZCode 人机验证令牌池（对照 Agent2API `api/zcode_captcha{,_pool}.rs`）。
///
/// 单独成函数：库存概况 / 入池 / 风控配置 / 铸造节奏自成一族，
/// 且 `paths_accounts_network` 已接近 `json!` 的展开深度上限。
fn paths_zcode_captcha() -> Value {
    json!({
        "/api/zcode/captcha": {
            "get": op("验证码池", "池子概况：库存 + 铸造节奏 + 该不该铸（有账号且低于目标）"),
            "post": op_body("验证码池", "令牌入池（前端铸好一个推一个）",
                "tokens?: [{param: string, region?: string}], param?: string, region?: string")
        },
        "/api/zcode/captcha/config": {
            "get": op_params("验证码池", "上游风控配置（前端据此决定弹不弹滑块 / 铸不铸）", &[
                query_param("account_id", "string", "用哪个 ZCode 账号取（省略则自动挑一个）")
            ])
        },
        "/api/zcode/captcha-pool": {
            "get": op("验证码池", "读取铸造节奏（库存目标 / 轮询间隔 / 单轮上限）"),
            "put": op_body("验证码池", "保存铸造节奏",
                "target?: integer(1~10), pollMs?: integer(1000~30000), maxPerRound?: integer(1~5)")
        },
    })
}

/// 报表扩展（移植 Agent2API report 页视角）。
fn paths_report() -> Value {
    json!({
        "/api/dashboard/report/cache_rates": {
            "get": op_params("看板", "缓存命中率多窗口（10m/1h/24h/7d）", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/report/cache_trend_24h": {
            "get": op("看板", "近 24 小时小时粒度缓存命中趋势")
        },
        "/api/dashboard/report/daily_trend": {
            "get": op_params("看板", "按天请求/Token 趋势", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/report/heatmap": {
            "get": op_params("看板", "365 天日粒度消费热力图", &[query_param("days", "integer", "回溯天数（1-365）")])
        },
        "/api/dashboard/report/models": {
            "get": op_params("看板", "按模型聚合的用量占比", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/report/overview": {
            "get": op_params("看板", "报表总览卡（请求/成功率/Token/活跃天数/Top 模型）", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/report/token_trend": {
            "get": op_params("看板", "按天 Token 趋势（prompt/completion 分列）", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/report/top_sources": {
            "get": op_params("看板", "Top 账号 / Top 提供商", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/scheduler_status": {
            "get": op("看板", "渠道调度器状态")
        },
    })
}

fn paths_payment() -> Value {
    json!({
        "/api/epay/config": {
            "get": op("支付", "查询易支付配置"),
            "put": op_body("支付", "更新易支付配置", "商户号/网关地址等配置字段")
        },
        "/api/epay/info": { "get": op("支付", "用户侧充值页信息") },
        "/api/user/epay/notify": {
            "get": op_pub("支付", "易支付异步通知（回调验签）"),
            "post": op_pub("支付", "易支付异步通知（回调验签）")
        },
        "/api/user/epay/return": {
            "get": op_pub("支付", "易支付同步跳转"),
            "post": op_pub("支付", "易支付同步跳转")
        },
        "/api/stripe/topup": {
            "post": op_body("支付", "创建 Stripe 充值意图", "amount: number")
        },
        "/api/user/stripe/webhook": {
            "post": op_pub("支付", "Stripe Webhook 回调（签名校验）")
        },
        "/api/topup": {
            "post": op_body("支付", "发起充值订单（易支付）", "amount: number")
        },
        "/api/topup/amount": {
            "post": op_body("支付", "充值试算（金额→配额）", "amount: number")
        },
        "/api/orders": {
            "get": op("支付", "列出订单（管理员）")
        },
        "/api/orders/me": {
            "get": op("支付", "列出本人订单")
        },
        "/api/orders/{id}": {
            "get": op_params("支付", "订单详情", &[path_param("id")]),
            "delete": op_params("支付", "删除订单", &[path_param("id")])
        },
        "/api/orders/{id}/complete": {
            "post": op_params("支付", "手动补单（人工确认到账）", &[path_param("id")])
        },
        "/api/redemptions": {
            "get": op("支付", "列出兑换码")
        },
        "/api/redemptions/batch": {
            "post": op_body("支付", "批量生成兑换码", "count: integer, quota: integer")
        },
        "/api/redemptions/{id}": {
            "delete": op_params("支付", "删除兑换码", &[path_param("id")])
        },
        "/api/redemptions/redeem": {
            "post": op_body("支付", "兑换码核销", "code: string")
        },
        "/api/plans": {
            "get": op("套餐", "列出按量套餐模板"),
            "post": op_body("套餐", "新增/更新套餐模板", "name/models/quota 等套餐字段")
        },
        "/api/plans/{id}": {
            "delete": op_params("套餐", "删除套餐模板", &[path_param("id")])
        },
        "/api/plans/{id}/issue": {
            "post": op_params("套餐", "按套餐发放 API Key", &[path_param("id")])
        },
        "/api/subscription/plans": {
            "get": op("订阅", "列出可购时长订阅套餐")
        },
        "/api/subscription/self": {
            "get": op("订阅", "本人订阅状态")
        },
        "/api/subscription/balance/pay": {
            "post": op_body("订阅", "余额购买订阅", "plan_id: string")
        },
        "/api/subscription/admin/bind": {
            "post": op_body("订阅", "管理端为用户绑定订阅（写操作）", "user_id/plan_id: string")
        },
        "/api/subscription/admin/users/{id}/subscriptions": {
            "get": op_params("订阅", "管理端查询用户订阅列表", &[path_param("id")])
        },
        "/api/subscription/admin/subscriptions/{id}/cancel": {
            "post": op_params("订阅", "管理端取消订阅（写操作）", &[path_param("id")])
        }
    })
}

fn paths_logs() -> Value {
    json!({
        "/api/logs/requests": {
            "get": op_params("日志", "查询请求日志（分页+多维过滤）", &[
                query_param("user", "string", "按用户（UUID 或邮箱）"),
                query_param("model", "string", "按模型名"),
                query_param("channel", "string", "按渠道 ID"),
                query_param("start", "integer", "起始 unix 时间戳"),
                query_param("end", "integer", "截止 unix 时间戳"),
                query_param("page", "integer", "页码（默认 1）"),
                query_param("size", "integer", "每页条数（默认 20）")
            ]),
            "delete": op_params("日志", "按条件删除请求日志", &[
                query_param("start", "integer", "起始 unix 时间戳"),
                query_param("end", "integer", "截止 unix 时间戳")
            ])
        },
        "/api/logs/requests/export": {
            "get": op_params("日志", "导出请求日志", &[
                query_param("format", "string", "导出格式"),
                query_param("user", "string", "按用户过滤"),
                query_param("model", "string", "按模型过滤"),
                query_param("channel", "string", "按渠道过滤"),
                query_param("start", "integer", "起始时间戳"),
                query_param("end", "integer", "截止时间戳")
            ])
        },
        "/api/logs/requests/clear": {
            "delete": op("日志", "清空全部请求日志（写操作）")
        },
        "/api/logs/requests/clear-preview": {
            "get": op_params("日志", "删除前预览：告诉管理员会清掉多少条请求日志", &[
                query_param("days", "integer", "回溯天数（0/缺省=全部）")
            ])
        },
        "/api/logs/inflight": {
            "get": op("日志", "在途请求快照（谁在跑、跑了多久；RequestLog 只在结束后落库）")
        },
        "/api/logs/inflight/{id}/terminate": {
            "post": op_params("日志", "终止指定在途请求（写操作）", &[path_param("id")])
        },
        "/api/logs/runtime": {
            "get": op_params("日志", "运行日志环形缓冲（level/category 字典 + 500 条 + sinceId 水位）", &[
                query_param("level", "string", "按级别过滤"),
                query_param("category", "string", "按类别过滤"),
                query_param("since_id", "integer", "水位（只取更新的条目）"),
                query_param("limit", "integer", "返回条数上限")
            ])
        },
        "/api/logs/runtime/export": {
            "get": op("日志", "导出运行日志 JSONL（Content-Disposition 附件）")
        },
        "/api/logs/agg/rebuild": {
            "post": op("日志", "手动重建日聚合（逃生门：聚合数字可疑时重算历史，写操作）")
        },
        "/api/logs/agg/tz-migrate": {
            "post": op_body("日志", "报表时区迁移（逃生门：改了 [report] timezone_offset 后重算存量聚合行，写操作）。响应附 tzEffectiveFrom + notices，含「窗口内旧口径聚合行先删除再回填、绕开回填的 shrink 守卫」与「不覆盖今天、切换当日偏低次日自愈」两条坑", "offset_from?/offset_to?（snake_case，另有 camelCase 别名；缺省：旧口径 0 → 新口径取当前生效值）")
        },
        "/api/logs/audits": {
            "get": op_params("日志", "查询审计日志（分页）", &[
                query_param("page", "integer", "页码（默认 1）"),
                query_param("size", "integer", "每页条数（默认 20）")
            ]),
            "delete": op("日志", "按条件删除审计日志（写操作）")
        },
        "/api/logs/audits/clear": {
            "delete": op("日志", "清空全部审计日志（写操作）")
        },
        "/api/logs/retention": {
            "get": op("日志", "查询日志保留策略"),
            "put": op_body("日志", "更新日志保留策略（写操作）", "保留天数/容量上限字段")
        },
        "/api/logs/cleanup": {
            "post": op("日志", "手动触发过期日志清理（写操作）")
        }
    })
}

fn paths_dashboard_usage() -> Value {
    json!({
        "/api/dashboard/consumption_trend": {
            "get": op_params("看板", "消费趋势（按日聚合）", &[query_param("days", "integer", "回溯天数（1-90，默认 30）")])
        },
        "/api/dashboard/model_distribution": {
            "get": op_params("看板", "模型调用分布", &[query_param("days", "integer", "回溯天数（1-90，默认 30）")])
        },
        "/api/dashboard/user_ranking": {
            "get": op_params("看板", "用户用量排行", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/channel_health": {
            "get": op_params("看板", "渠道健康看板（含断路器状态）", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/dashboard/realtime": {
            "get": op("看板", "近 5 分钟实时指标（QPS/错误率/延迟）")
        },
        "/api/dashboard/cache_savings": {
            "get": op_params("看板", "缓存命中节省看板", &[query_param("days", "integer", "回溯天数（1-90，默认 30）")])
        },
        "/api/usage/trend": {
            "get": op_params("看板", "用量趋势（token/请求）", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/usage/models": {
            "get": op_params("看板", "按模型聚合用量", &[query_param("days", "integer", "回溯天数")])
        },
        "/api/usage/summary": {
            "get": op("看板", "用量总摘要"),
            "post": op("看板", "强制刷新用量统计缓存（写操作）")
        }
    })
}

fn paths_settings_ops() -> Value {
    json!({
        "/api/settings": {
            "get": op("设置", "查询系统设置"),
            "put": op_body("设置", "更新系统设置（写操作）", "设置键值对象")
        },
        "/api/oauth/config": {
            "get": op("设置", "查询 OAuth 提供商配置"),
            "put": op_body("设置", "更新 OAuth 配置（写操作）", "github/google/linuxdo 配置组")
        },
        "/api/limits": {
            "get": op("设置", "查询系统限额"),
            "put": op_body("设置", "更新系统限额（写操作）", "限额键值对象")
        },
        "/api/agent/settings": {
            "get": op("AI 助手", "查询 AI 助手设置（模型/提示词/工具开关/轮数）"),
            "put": op_body("AI 助手", "更新 AI 助手设置（写操作）", "model/systemPrompt/disabledTools/maxTurns/approvalTimeout")
        },
        "/api/chat/settings": {
            "get": op("设置", "查询聊天设置（LLM 自动标题开关/模型/渠道/提示字符数），响应 camelCase"),
            "put": op_body("设置", "更新聊天设置（写操作，热生效）", "auto_title/title_model/title_channel/title_prompt_chars（snake_case，缺省保持原值）")
        },
        "/api/report/settings": {
            "get": op("设置", "查询报表设置（日界时区偏移），响应 camelCase：timezoneOffset（配置值）/effectiveOffset（进程生效值）/storedOffset（存量聚合行口径，null=老库无标记）/needsMigration（存量需重算）/minOffset/maxOffset"),
            "put": op_body("设置", "更新报表设置（写操作，保存即热生效；存量聚合行不自动重算，需另调 /api/logs/agg/tz-migrate）", "timezone_offset（整数小时 UTC 偏移，-12~14，另有 camelCase 别名 timezoneOffset；缺省保持原值）")
        },
        // Agent 工作台（src/agent/api.rs 的独立 router，main.rs 里 merge 进来）
        "/api/agent/config": {
            "get": op("AI 助手", "Agent 配置只读视图（与 /api/agent/settings 同源同形状，附 effectiveModel 生效模型）")
        },
        "/api/agent/sessions": {
            "get": op("AI 助手", "列出 Agent 会话"),
            "post": op_body("AI 助手", "新建 Agent 会话", "title?: string, role?: 'observer' | 'operator'")
        },
        "/api/agent/sessions/{id}": {
            "get": op_params("AI 助手", "会话详情 + 全部消息", &[path_param("id")]),
            "delete": op_params("AI 助手", "删除会话及其消息（顺带清免审集）", &[path_param("id")])
        },
        "/api/agent/sessions/{id}/chat": {
            "post": op_params("AI 助手", "对话（SSE 流式推 AgentEvent；body: message/model?/reasoning_effort?/role?）", &[path_param("id")])
        },
        "/api/agent/approvals/{request_id}": {
            "post": op_params("AI 助手", "审批响应（写操作；body: action = allow | deny | remember）", &[path_param("request_id")])
        },
        "/api/gateway/settings": {
            "get": op("网关", "查询网关设置（监听地址/CORS/反代信任）"),
            "put": op_body("网关", "更新网关设置（重启后生效）", "host/port/corsOrigins/trustProxyHeaders")
        },
        "/api/routing": {
            "get": op("路由", "查询路由策略（策略/粘性/断路器/调度权重）与运行时状态"),
            "put": op_body("路由", "更新路由策略（热生效）", "strategy/sticky/breaker/scheduler_weights")
        },
        "/api/routing/defaults": {
            "get": op("路由", "查询路由策略默认值（恢复默认用）")
        },
        // #38 粘滞会话管理面（对照 Agent2API routing_workspace 的 get_sticky/clear_sticky）
        "/api/routing/sticky": {
            "get": op_params("路由", "当前粘滞条目快照（session+model 亲和；条目含渠道名映射与双 TTL 剩余秒；total/truncated 有界 500）", &[query_param("query", "string", "可选：对 session_id/model/channel_id/渠道名的小写包含过滤")])
        },
        "/api/routing/sticky/clear": {
            "post": op_body("路由", "解除粘滞（只删内存亲和记录，不写策略快照；下次该会话选路自然重走 HRW）", "id（单条，GET 返回的 id）| all:true（全清）；两者皆缺 400")
        },
        // #40 多规则档案 + 候选队列 + 判定预演（契约 §5）
        "/api/routing/queue": {
            "get": op_params("路由", "候选队列预演（只读，不推进 round_robin 游标；round_robin 下实际首挑由游标决定）", &[
                query_param("provider", "string", "按上游过滤"),
                query_param("model", "string", "按模型过滤"),
                query_param("session_id", "string", "按会话算粘滞命中")
            ])
        },
        "/api/routing/profiles": {
            "get": op("路由", "读取多规则档案全量（组列表 + 规则）"),
            "put": op_body("路由", "整份写入多规则档案（写操作）", "profiles/version 等档案字段")
        },
        "/api/routing/profiles/group/{id}": {
            "get": op_params("路由", "读取单个规则组", &[path_param("id")]),
            "put": op_params("路由", "写入/更新单个规则组（写操作）", &[path_param("id")]),
            "delete": op_params("路由", "删除单个规则组（写操作）", &[path_param("id")])
        },
        "/api/routing/preview": {
            "post": op_body("路由", "判定预演（与队列共用同一判定序列，可指定 group_id 预演某组）", "provider/model/session_id/group_id 等")
        },
        "/api/assistant/tools": {
            "get": op("助手", "聊天助手工具目录（管理员+开关开启→非 HighRisk 全集；否则空清单）")
        },
        "/api/assistant/chat": {
            "post": op_body("助手", "工具回路聊天（多轮 tool-call 循环，12 轮上限；计费走订阅池+钱包；一条请求日志归集全程）", "model/messages[{role,content}]/channel_id?/max_rounds?")
        },
        "/api/update/cleanup-artifacts": {
            "post": op("更新", "一键清理更新产物（保留当前与上一版各一代回滚候选；返回 removed/bytes）")
        },
        "/api/routing/replay/{request_id}": {
            "get": op_params("路由", "路由回放快照（Phase1-B/#26/#32 只读投影：候选序/过滤原因/选中/策略快照/account_skips 按渠道分组且有界/attempt_details 逐次上游尝试明细（index/channel/provider/model/stickyHit/startedAt/finishedAt/status/error/notice；有界 64+截断计数）/route_truncated 与 attempt_details_truncated 截断计数；route_replay 开关开启的请求才有完整数据）", &[path_param("request_id")])
        },
        "/api/gateway/prompt": {
            "get": op("网关", "查询系统提示词注入配置（模式/文本）"),
            "put": op_body("网关", "更新系统提示词注入（热生效）", "mode(passthrough/custom/append)/text")
        },
        "/api/sanitize": {
            "get": op("网关", "查询额外脱敏请求头名单（指纹脱敏）"),
            "put": op_body("网关", "更新额外脱敏请求头名单（热生效）", "headers: string[]")
        },
        "/api/purge": {
            "get": op("网关", "查询破甲控制台配置 / 进程统计 / 运行信息"),
            "put": op_body("网关", "全量更新破甲控制台配置（热生效）", "enabled/system_normalize/dead_zone_rewrite/reduce_reasoning/cross_source_fallback/skill_inject/structured_prompt 等")
        },
        "/api/cline-headers": {
            "get": op("网关", "查询 Cline 伪装头覆盖表"),
            "put": op_body("网关", "更新 Cline 伪装头覆盖表（热生效）", "headers: object")
        },
        "/api/security/policy": {
            "get": op("安全", "查询安全策略（登录失败锁定 / 登录限流阈值）"),
            "put": op_body("安全", "更新安全策略（保存即生效）", "登录失败次数/锁定时长/限流阈值")
        },
        "/api/queue": {
            "get": op("网关", "查询排队设置（并发满时等待上限）"),
            "put": op_body("网关", "更新排队设置（热生效）", "queueMaxWaits: integer, queueWaitSeconds: integer")
        },
        "/api/scheduled-tasks": {
            "get": op("定时任务", "列出间隔型定时任务（状态/间隔/上次运行）")
        },
        "/api/scheduled-tasks/{id}": {
            "patch": op_params("定时任务", "配置任务开关/间隔（写操作）", &[path_param("id")]),
            "post": op_params("定时任务", "配置任务开关/间隔（POST 别名，与 PATCH 同 handler）", &[path_param("id")])
        },
        "/api/scheduled-tasks/{id}/run": {
            "post": op_params("定时任务", "立即执行一次（写操作）", &[path_param("id")])
        },
        "/api/upgrade": {
            "get": op("更新", "查询数据升级状态（AIGX 原生 SQLite 无旧数据，端点保留对齐 API 面）")
        },
        "/api/upgrade/run": {
            "post": op("更新", "执行数据升级（写操作）")
        },
        "/api/storage": {
            "get": op("数据", "存储概览（KV 总键数/分组统计/库文件大小）")
        },
        "/api/backup/export-encrypted": {
            "post": op_body("数据管理", "生成口令加密备份（AIGXBAK1：Argon2id+AES-256-GCM 流式包；口令只经请求体，不落盘不进日志）", "password（≥8 字符）+ include_logs?")
        },
        "/api/backup/download": {
            "get": op_params("数据管理", "下载加密备份包（?file=export-*.aigxbak，文件名只认本模块产出形态，防路径穿越）", &[query_param("file", "string", "加密包文件名")])
        },
        "/api/backup/import-encrypted": {
            "post": op_body("数据管理", "上传加密包并恢复（multipart：file+password；解密全量校验先行，恢复前自动备份当前全量）", "multipart file+password")
        },
        // 明文导出 /api/backup/export 已移除（#33 安全缺口本体）：
        // 改用 export-encrypted 三件套；明文导入保留（存量旧包迁移）。
        "/api/backup/import": {
            "post": op_body("数据", "导入备份恢复（先自动快照当前全量到 backups/ 再写回）", "BackupDocument{version,exported_at,include_logs,kv}")
        },
        "/api/notify/config": {
            "get": op("通知", "查询通知配置（脱敏）"),
            "put": op_body("通知", "更新通知配置（写操作）", "telegram/email/slack/webhook 配置组")
        },
        "/api/notify/test-telegram": {
            "post": op("通知", "发送 Telegram 测试消息")
        },
        "/api/notify/test-email": {
            "post": op("通知", "发送测试邮件")
        },
        "/api/notify/test-slack": {
            "post": op("通知", "发送 Slack 测试消息")
        },
        "/api/notify/test-webhook": {
            "post": op("通知", "发送 Webhook 测试请求")
        },
        "/api/alerts/rules": {
            "get": op("告警", "列出告警规则"),
            "put": op_body("告警", "更新告警规则（写操作）", "规则数组")
        },
        "/api/alerts/active": {
            "get": op("告警", "当前活跃告警")
        },
        "/api/alerts/resolve": {
            "post": op_body("告警", "标记活跃告警已处理（写操作）", "告警 id")
        },
        "/api/alerts/history": {
            "get": op_params("告警", "告警历史", &[query_param("limit", "integer", "返回条数上限")])
        },
        "/api/alerts/test": {
            "post": op("告警", "触发测试告警")
        },
        "/api/cache/stats": {
            "get": op("缓存", "响应缓存统计")
        },
        "/api/cache/clear": {
            "post": op("缓存", "清空响应缓存（写操作）")
        },
        "/api/ip/filter": {
            "get": op("安全", "查询 IP 黑白名单"),
            "put": op_body("安全", "更新 IP 过滤配置（写操作）", "whitelist/blacklist: string[]")
        },
        "/api/ip/whitelist": {
            "post": op_body("安全", "新增 IP 白名单条目（写操作）", "pattern: string")
        },
        "/api/ip/blacklist": {
            "post": op_body("安全", "新增 IP 黑名单条目（写操作）", "pattern: string")
        },
        "/api/ip/whitelist/{pattern}": {
            "delete": op_params("安全", "删除 IP 白名单条目（写操作）", &[path_param("pattern")])
        },
        "/api/ip/blacklist/{pattern}": {
            "delete": op_params("安全", "删除 IP 黑名单条目（写操作）", &[path_param("pattern")])
        }
    })
}

fn paths_monitor_security() -> Value {
    json!({
        "/api/monitor/system": {
            "get": op("监控", "系统资源快照（CPU/内存/负载，非 Linux 降级）")
        },
        "/api/monitor/health": {
            "get": op("监控", "健康检查（真实 uptime/内存快照）")
        },
        "/api/monitor/healthz": {
            "get": op("监控", "健康检查（无鉴权）")
        },
        "/api/monitor/security": {
            "get": op("监控", "安全汇总（评分与概览）")
        },
        "/api/monitor/security/events": {
            "get": op_params("监控", "安全事件列表（分页）", &[
                query_param("range", "string", "时间范围"),
                query_param("page", "integer", "页码"),
                query_param("page_size", "integer", "每页条数")
            ])
        }
    })
}

fn paths_playground() -> Value {
    json!({
        "/api/playground/chat": {
            "post": op_body("游乐场", "管理面聊天测试（真实上游）", "model/messages 等 OpenAI 兼容请求体")
        },
        "/api/playground/images": {
            "post": op_body("游乐场", "图像生成测试", "model/prompt 等 OpenAI 兼容请求体")
        },
        "/api/playground/tts": {
            "post": op_body("游乐场", "语音合成测试", "model/input 等 OpenAI 兼容请求体")
        },
        "/api/playground/transcriptions": {
            "post": op_body("游乐场", "语音转写测试", "multipart 音频文件 + model")
        }
    })
}

fn paths_accounts_network() -> Value {
    json!({
        "/api/accounts": {
            "get": op("网络层", "列出 Cloudflare 账号池"),
            "post": op_body("网络层", "新增账号", "account 配置字段")
        },
        "/api/accounts/test": {
            "post": op("网络层", "测试账号可用性")
        },
        "/api/accounts/{id}": {
            "put": op_params("网络层", "更新账号", &[path_param("id")]),
            "delete": op_params("网络层", "删除账号", &[path_param("id")])
        },
        "/api/account-sessions": {
            "get": op("账号池", "列出账号型渠道的账号（凭证/代理脱敏）"),
            "post": op_body("账号池", "新增账号", "channel + 凭证/代理/优先级/并发/模型")
        },
        "/api/account-sessions/usage": {
            "post": op_body("账号池", "手动批量/单账号余额查询（写回快照；有界并发 3）", "id?: string（缺省=全部支持查询的账号）")
        },
        "/api/account-sessions/usage/snapshot": {
            "get": op("账号池", "各账号最近一次余额查询快照（手动+自动同源；行带自己的 at，skipped=未查询数）")
        },
        "/api/account-sessions/desktop/sources": {
            "get": op("账号池", "桌面端登录态来源白名单（固定路径；supported 标平台与解密可用性）")
        },
        "/api/account-sessions/desktop-import": {
            "post": op_body("账号池", "桌面端登录态导入（仅 Windows；白名单来源只读，不接受路径参数；凭证 enc1: 落库不落日志）", "source_id（白名单 id）+ name?")
        },
        "/api/account-sessions/batch": {
            "post": op_body("账号池", "批量操作六类（enable/disable/set_proxy/set_max_concurrency/query_settings/limiter/delete），逐账号回执 [{id,ok,error?}]；删除保护 error=referenced_by_channel", "op/ids + 按 op 的 payload（proxy/max_concurrency/usage_query_*/limiter_mode+limiters）")
        },
        "/api/account-sessions/catalog/providers": {
            "get": op("账号池", "支持模型目录发现的供应商清单")
        },
        "/api/account-sessions/catalog/sync": {
            "post": op_body("账号池", "同步上游模型目录（可写回账号白名单）", "")
        },
        "/api/account-sessions/import": {
            "post": op_body("账号池", "导入本机客户端（cc-switch）的供应商为渠道", "")
        },
        "/api/account-sessions/import/agent2api": {
            "post": op_body("账号池", "从 Agent2API 账号库导入（SQLite 路径或记录 JSON，幂等更新）", "")
        },
        "/api/account-sessions/import/sources": {
            "get": op("账号池", "扫描本机可导入的供应商（cc-switch）")
        },
        "/api/account-sessions/login/callback": {
            "post": op_body("账号池", "提交浏览器地址栏里的回调地址", "")
        },
        "/api/account-sessions/login/cancel": {
            "post": op_body("账号池", "取消等待中的网页登录", "")
        },
        "/api/account-sessions/login/oauth/captcha-config": {
            "get": op("账号池", "AutoClaw OAuth 风控验证码配置（前端据此初始化阿里云 SDK）")
        },
        "/api/account-sessions/login/oauth/start": {
            "post": op_body("账号池", "发起 AutoClaw OAuth 登录（Zai/Google，需验证码参数）", "")
        },
        "/api/account-sessions/login/paste": {
            "post": op_body("账号池", "粘贴式登录（归一化 + 校验后落账号）", "")
        },
        "/api/account-sessions/login/sms/send": {
            "post": op_body("账号池", "发送手机验证码", "")
        },
        "/api/account-sessions/login/sms/verify": {
            "post": op_body("账号池", "手机验证码登录并落账号", "")
        },
        "/api/account-sessions/login/start": {
            "post": op_body("账号池", "发起登录态供应商的网页登录", "")
        },
        "/api/account-sessions/login/wait": {
            "get": op("账号池", "轮询登录结果")
        },
        "/api/admin/tickets": {
            "get": op("工单", "管理面工单列表")
        },
        "/api/admin/tickets/close": {
            "post": op_body("工单", "管理员关闭工单", "")
        },
        "/api/admin/tickets/reply": {
            "post": op_body("工单", "管理员回复工单", "")
        },
        "/api/prompts/fetch": {
            "post": op_body("提示词", "抓取远端提示词", "")
        },
        "/api/prompts/sources": {
            "get": op("提示词", "提示词来源列表")
        },
        "/api/prompts/translate": {
            "post": op_body("提示词", "翻译提示词", "")
        },
        "/api/proxy-pool": {
            "get": op("网络代理", "代理池列表（含账号引用统计）"),
            "post": op_body("网络代理", "新建代理池条目（http/https/socks5/socks5h）", "")
        },
        "/api/proxy-pool/enable": {
            "post": op_body("网络代理", "启用/禁用代理池条目", "{ id, enabled }")
        },
        "/api/proxy-pool/remove": {
            "post": op_body("网络代理", "删除代理池条目（有账号引用时拒绝）", "{ id }")
        },
        "/api/proxy-pool/test": {
            "post": op_body("网络代理", "测试代理池条目连通性（结果记入 lastTest）", "{ id }")
        },
        "/api/proxy-pool/test-account": {
            "post": op_body("网络代理", "按账号代理配置测试出口（或测临时串/直连）", "{ accountId } | { proxy }")
        },
        "/api/proxy-pool/sync-clash": {
            "post": op_body("网络代理", "同步 Clash Verge 出口到代理池（镜像监听器）", "")
        },
        "/api/proxy-pool/update": {
            "post": op_body("网络代理", "编辑代理池条目（密码留空=保留旧值）", "")
        },
        "/api/plugins": {
            "get": op("插件", "插件列表（内置 provider 目录包 + 外置插件包 + 发布目录）")
        },
        "/api/plugins/install": {
            "post": op_body(
                "插件",
                "从 URL 安装外置插件包（zip；可选 sha256 校验，装完默认停用）",
                "{ url, sha256? }",
            )
        },
        "/api/plugins/enable": {
            "post": op_body(
                "插件",
                "包级启停（外置包：启用/停用子进程；内置包不支持整包停用）",
                "{ id, enabled }",
            )
        },
        "/api/plugins/provider": {
            "post": op_body("插件", "provider 级启停（数据面选号即时生效）", "{ id, provider, enabled }")
        },
        "/api/plugins/uninstall": {
            "post": op_body("插件", "卸载外置插件包（停进程 + 删包；内置包不支持）", "{ id }")
        },
        "/api/plugins/update": {
            "get": op("插件", "插件更新检查（内置目录随网关版本发布）")
        },
        "/api/plugins/release": {
            "get": op("插件", "发布目录（从 [plugins] source_url 实时拉取）")
        },
        "/api/plugins/progress": {
            "get": op("插件", "在途安装/更新操作（当前恒空）")
        },
        "/api/public/geo-lang": {
            "get": op("公开", "按 IP 推断语言")
        },
        "/api/tickets": {
            "get": op("工单", "工单列表"),
            "post": op_body("工单", "提交工单", "")
        },
        "/api/tickets/close": {
            "post": op_body("工单", "关闭工单", "")
        },
        "/api/tickets/reply": {
            "post": op_body("工单", "回复工单", "")
        },
        "/api/account-sessions/providers": {
            "get": op("账号池", "provider 目录（粘贴凭证型上游与默认地址）")
        },
        "/api/account-sessions/rotation": {
            "get": op("账号池", "当前账号路由参数（冷却区间与单请求最大换号次数）"),
            "put": op_body("账号池", "保存账号路由参数", "base/max/auth 冷却秒数 + max_attempts")
        },
        "/api/account-sessions/stats": {
            "get": op("账号池", "按渠道聚合的账号池统计（启用/冷却/并发/成败）")
        },
        "/api/account-sessions/connections": {
            "get": op("账号池", "各账号实时并发占用快照（连接数列 2s 轮询）")
        },
        "/api/account-sessions/export": {
            "get": op("账号池", "导出账号池 JSON 迁移包（含明文凭证，仅管理员；写进响应体不落盘）")
        },
        "/api/account-sessions/import/json": {
            "post": op_body("账号池", "从 JSON 迁移包导入账号池（幂等更新）", "导出的迁移包结构")
        },
        "/api/account-sessions/bulk": {
            "post": op_body("账号池", "批量导入凭证（每行 Name,Key 或仅 Key）", "channel_id + credentials")
        },
        "/api/account-sessions/{id}": {
            "put": op_params("账号池", "更新账号（渠道不可变、凭证三态）", &[path_param("id")]),
            "patch": op_params("账号池", "部分更新（名称/状态/优先级/权重/并发）", &[path_param("id")]),
            "delete": op_params("账号池", "删除账号", &[path_param("id")])
        },
        "/api/account-sessions/{id}/test": {
            "post": op_params("账号池", "单号探活（走真实推理链路）", &[path_param("id")])
        },
        "/api/account-sessions/{id}/reset-cooldown": {
            "post": op_params("账号池", "清除账号冷却与最近错误", &[path_param("id")])
        },
        "/api/account-sessions/{id}/balance": {
            "get": op_params("账号池", "账号级余额/额度查询（qoder/autoclaw/小浣熊/zcode）", &[path_param("id")])
        },
        "/api/account-sessions/{id}/refresh": {
            "post": op_params("账号池", "刷新账号 token（force 可选；失败不改写凭据）", &[path_param("id")])
        },
        "/api/account-sessions/{id}/promote": {
            "post": op_params("账号池", "账号置顶（提到同渠道同优先级队首，sort_order = 组内最小 - 1）", &[path_param("id")])
        },
        "/api/account-sessions/order": {
            "put": op_body("账号池", "账号整队重排（按给定 id 数组顺序重写 sort_order，未知/重复 id 计入 skipped）", "order: 从队首到队尾的账号 id 数组")
        },
        "/api/update/status": {
            "get": op("更新", "服务器更新状态（版本/下载进度/待重启/可原地更新）")
        },
        "/api/update/check": {
            "post": op("更新", "立即检查 GitHub Release（只读网络请求，受限流冷却约束）")
        },
        "/api/update/apply": {
            "post": op("更新", "下载并应用最新版本（sha256 校验 → 替换二进制/static → 自动重启）")
        },
        "/api/update/restart": {
            "post": op("更新", "重启 AIGX 服务（exec 自重启，约 2 秒后生效）")
        },
        "/api/update/rollback": {
            "post": op("更新", "回滚到上一版本（使用 aigx.bak / static.bak）")
        },
        "/api/update/settings": {
            "get": op("更新", "查询自动更新配置（令牌脱敏）"),
            "put": op_body("更新", "保存自动更新配置", "enabled/auto_check/auto_apply/interval_hours/repo/api_base/proxy/token")
        },
        "/api/network/status": {
            "get": op("网络层", "网络层健康状态")
        },
        "/api/network/config/{config_id}": {
            "put": op_params("网络层", "更新网络层配置（写操作）", &[path_param("config_id")])
        },
        "/api/network/restart": {
            "post": op("网络层", "重启网络层（写操作）")
        },
        "/api/network/accounts": {
            "get": op("网络层", "列出网络层账号")
        },
        "/api/network/accounts/{account_id}": {
            "post": op_params("网络层", "添加网络层账号（写操作）", &[path_param("account_id")]),
            "delete": op_params("网络层", "移除网络层账号（写操作）", &[path_param("account_id")])
        }
    })
}

fn paths_docs() -> Value {
    json!({
        "/api-docs/openapi.json": {
            "get": op_pub("文档", "legacy OpenAPI 3.0 文档（部分端点，无鉴权）")
        },
        "/swagger-ui": {
            "get": op_pub("文档", "Swagger UI 页面（渲染 /api-docs/openapi.json）")
        }
    })
}

fn paths_diagnostics() -> Value {
    json!({
        "/api/diagnostics/summary": {
            "get": op("诊断", "系统体检摘要（渠道统计/今日用量/24h 错误 top/运行信息）")
        },
        "/api/diagnostics/channels": {
            "get": op("诊断", "启用渠道批量测活（并发探测，只探测不落状态）")
        },
        "/api/diagnostics/breakers": {
            "get": op("诊断", "熔断器状态快照（三态/失败计数/冷却剩余/租约）")
        },
        "/api/openapi.json": {
            "get": op("文档", "本文档：管理面 API 自描述（OpenAPI 3.1）")
        }
    })
}

fn paths_mcp() -> Value {
    json!({
        "/api/mcp": {
            "post": op_body(
                "MCP",
                "MCP server（Streamable HTTP，JSON-RPC 2.0：initialize/tools/list/tools/call）",
                "JSON-RPC 2.0 envelope（jsonrpc/id/method/params）；需 Bearer 管理会话鉴权，响应为 application/json 单响应"
            )
        }
    })
}

// ── 能力自发现（供 Agent 工具 aigx_openapi_search 检索）────────────

static OPENAPI_DOC: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// 文档缓存（纯函数产物，进程内不变；路由对照测试不受影响）。
pub fn openapi_document_cached() -> &'static Value {
    OPENAPI_DOC.get_or_init(build_openapi_document)
}

/// 按关键词搜索管理面 API（路径/摘要/标签子串匹配，path 命中优先）。
///
/// 只返回 API 形状（method/path/summary/tags），不含任何数据；
/// 新端点落进本文档即可被 Agent 发现（不依赖工具表同步）。
pub fn search_openapi(query: &str, tag: Option<&str>, limit: usize) -> Vec<Value> {
    let doc = openapi_document_cached();
    let q = query.trim().to_lowercase();
    let mut hits: Vec<(u8, String, Value)> = Vec::new();
    if let Some(paths) = doc.get("paths").and_then(|p| p.as_object()) {
        for (path, methods) in paths {
            let Some(ops) = methods.as_object() else {
                continue;
            };
            for (method, op) in ops {
                if !matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete") {
                    continue;
                }
                let summary = op.get("summary").and_then(|s| s.as_str()).unwrap_or("");
                let tags: Vec<&str> = op
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                if let Some(want) = tag {
                    if !tags.iter().any(|x| x.eq_ignore_ascii_case(want)) {
                        continue;
                    }
                }
                let rank = if q.is_empty() {
                    3u8
                } else if path.to_lowercase().contains(&q) {
                    0
                } else if summary.to_lowercase().contains(&q) {
                    1
                } else if tags.iter().any(|x| x.to_lowercase().contains(&q)) {
                    2
                } else {
                    continue;
                };
                hits.push((
                    rank,
                    path.clone(),
                    json!({
                        "method": method.to_uppercase(),
                        "path": path,
                        "summary": summary,
                        "tags": tags,
                    }),
                ));
            }
        }
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    hits.into_iter()
        .take(limit.clamp(1, 50))
        .map(|(_, _, v)| v)
        .collect()
}

// ── 测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// main.rs 真实管理面路由表（path + method 集合）。
    ///
    /// 来源：对 `build_router` 的 admin_routes 逐条 grep（2026-09 快照）。
    /// 路由增删后此表须同步更新，`covers_all_real_admin_routes` 会红——
    /// 这是"文档不许虚构/不许遗漏"的机器化契约。
    fn real_admin_routes() -> BTreeMap<&'static str, Vec<&'static str>> {
        let mut m = BTreeMap::new();
        let mut add = |p: &'static str, methods: &[&'static str]| {
            m.insert(p, methods.to_vec());
        };
        add("/api-docs/openapi.json", &["get"]);
        add("/api/account-sessions", &["get", "post"]);
        add("/api/account-sessions/batch", &["post"]);
        add("/api/account-sessions/usage", &["post"]);
        add("/api/account-sessions/usage/snapshot", &["get"]);
        add("/api/account-sessions/desktop/sources", &["get"]);
        add("/api/account-sessions/desktop-import", &["post"]);
        add("/api/account-sessions/export", &["get"]);
        add("/api/account-sessions/catalog/providers", &["get"]);
        add("/api/account-sessions/catalog/sync", &["post"]);
        add("/api/account-sessions/connections", &["get"]);
        add("/api/account-sessions/import", &["post"]);
        add("/api/account-sessions/import/agent2api", &["post"]);
        add("/api/account-sessions/import/json", &["post"]);
        add("/api/account-sessions/import/sources", &["get"]);
        add("/api/account-sessions/login/callback", &["post"]);
        add("/api/account-sessions/login/cancel", &["post"]);
        add("/api/account-sessions/login/oauth/captcha-config", &["get"]);
        add("/api/account-sessions/login/oauth/start", &["post"]);
        add("/api/account-sessions/login/paste", &["post"]);
        add("/api/account-sessions/login/sms/send", &["post"]);
        add("/api/account-sessions/login/sms/verify", &["post"]);
        add("/api/account-sessions/login/start", &["post"]);
        add("/api/account-sessions/login/wait", &["get"]);
        add("/api/admin/tickets", &["get"]);
        add("/api/admin/tickets/close", &["post"]);
        add("/api/admin/tickets/reply", &["post"]);
        add("/api/prompts/fetch", &["post"]);
        add("/api/prompts/sources", &["get"]);
        add("/api/prompts/translate", &["post"]);
        add("/api/proxy-pool", &["get", "post"]);
        add("/api/proxy-pool/enable", &["post"]);
        add("/api/proxy-pool/remove", &["post"]);
        add("/api/proxy-pool/test", &["post"]);
        add("/api/proxy-pool/test-account", &["post"]);
        add("/api/proxy-pool/sync-clash", &["post"]);
        add("/api/proxy-pool/update", &["post"]);
        add("/api/plugins", &["get"]);
        add("/api/plugins/install", &["post"]);
        add("/api/plugins/enable", &["post"]);
        add("/api/plugins/provider", &["post"]);
        add("/api/plugins/uninstall", &["post"]);
        add("/api/plugins/update", &["get"]);
        add("/api/plugins/release", &["get"]);
        add("/api/plugins/progress", &["get"]);
        add("/api/public/geo-lang", &["get"]);
        add("/api/tickets", &["get", "post"]);
        add("/api/tickets/close", &["post"]);
        add("/api/tickets/reply", &["post"]);
        add("/api/account-sessions/bulk", &["post"]);
        add("/api/account-sessions/providers", &["get"]);
        add("/api/account-sessions/rotation", &["get", "put"]);
        add("/api/account-sessions/stats", &["get"]);
        add("/api/account-sessions/{id}", &["delete", "patch", "put"]);
        add("/api/account-sessions/{id}/reset-cooldown", &["post"]);
        add("/api/account-sessions/{id}/balance", &["get"]);
        add("/api/account-sessions/{id}/refresh", &["post"]);
        add("/api/account-sessions/{id}/test", &["post"]);
        // SPEC-1.0.49 §9（流 H）：组内置顶 + 整队重排（sort_order）
        add("/api/account-sessions/{id}/promote", &["post"]);
        add("/api/account-sessions/order", &["put"]);
        add("/api/update/apply", &["post"]);
        add("/api/update/check", &["post"]);
        add("/api/update/restart", &["post"]);
        add("/api/update/rollback", &["post"]);
        add("/api/update/settings", &["get", "put"]);
        add("/api/update/status", &["get"]);
        add("/api/accounts", &["get", "post"]);
        add("/api/accounts/test", &["post"]);
        add("/api/accounts/{id}", &["delete", "put"]);
        add("/api/aff", &["get"]);
        add("/api/aff_transfer", &["post"]);
        add("/api/alerts/active", &["get"]);
        add("/api/alerts/resolve", &["post"]);
        add("/api/alerts/history", &["get"]);
        add("/api/alerts/rules", &["get", "put"]);
        add("/api/alerts/test", &["post"]);
        add("/api/auth/captcha", &["get"]);
        add("/api/auth/change-password", &["post"]);
        add("/api/auth/forgot-password", &["post"]);
        add("/api/auth/github", &["get"]);
        add("/api/auth/github/callback", &["get"]);
        add("/api/auth/github/bind/start", &["post"]);
        add("/api/auth/google", &["get"]);
        add("/api/auth/google/callback", &["get"]);
        add("/api/auth/google/bind/start", &["post"]);
        add("/api/auth/linuxdo", &["get"]);
        add("/api/auth/linuxdo/callback", &["get"]);
        add("/api/auth/linuxdo/bind/start", &["post"]);
        add("/api/auth/oauth/unbind", &["post"]);
        add("/api/auth/oauth/consume", &["post"]);
        add("/api/auth/login", &["post"]);
        add("/api/auth/login/code", &["post"]);
        add("/api/auth/login/send-code", &["post"]);
        add("/api/auth/login/totp", &["post"]);
        add("/api/auth/logout", &["post"]);
        add("/api/auth/register", &["post"]);
        add("/api/auth/reset-password", &["post"]);
        add("/api/auth/totp/disable", &["post"]);
        add("/api/auth/totp/enable", &["post"]);
        add("/api/auth/totp/setup", &["post"]);
        add("/api/cache/clear", &["post"]);
        add("/api/cache/stats", &["get"]);
        add("/api/channels", &["get", "post"]);
        add("/api/channels/chat_test", &["post"]);
        add("/api/channels/fetch_models", &["post"]);
        add("/api/channels/{id}", &["delete", "patch", "put"]);
        add("/api/channels/{id}/balance", &["get"]);
        add("/api/channels/{id}/health-archive", &["get"]);
        add("/api/channels/{id}/patch", &["patch"]);
        add("/api/channels/{id}/reset-circuit", &["post"]);
        add("/api/channels/{id}/test", &["post"]);
        add("/api/chat/settings", &["get", "put"]);
        add("/api/chat/sessions", &["get", "post"]);
        add("/api/chat/sessions/{id}", &["delete", "get"]);
        add("/api/chat/sessions/{id}/fork", &["post"]);
        add("/api/chat/sessions/{id}/messages", &["post", "put"]);
        add("/api/chat/sessions/{id}/rename", &["post"]);
        add("/api/checkin", &["get", "post"]);
        add("/api/dashboard/cache_savings", &["get"]);
        add("/api/dashboard/channel_health", &["get"]);
        add("/api/dashboard/consumption_trend", &["get"]);
        add("/api/dashboard/model_distribution", &["get"]);
        add("/api/dashboard/realtime", &["get"]);
        add("/api/dashboard/report/cache_rates", &["get"]);
        add("/api/dashboard/report/cache_trend_24h", &["get"]);
        add("/api/dashboard/report/daily_trend", &["get"]);
        add("/api/dashboard/report/heatmap", &["get"]);
        add("/api/dashboard/report/models", &["get"]);
        add("/api/dashboard/report/overview", &["get"]);
        add("/api/dashboard/report/token_trend", &["get"]);
        add("/api/dashboard/report/top_sources", &["get"]);
        add("/api/dashboard/scheduler_status", &["get"]);
        add("/api/dashboard/user_ranking", &["get"]);
        add("/api/diagnostics/breakers", &["get"]);
        add("/api/diagnostics/channels", &["get"]);
        add("/api/diagnostics/summary", &["get"]);
        add("/api/epay/config", &["get", "put"]);
        add("/api/epay/info", &["get"]);
        add("/api/groups", &["get", "post"]);
        add("/api/groups/{name}", &["delete", "put"]);
        add("/api/ip/blacklist", &["post"]);
        add("/api/ip/blacklist/{pattern}", &["delete"]);
        add("/api/ip/filter", &["get", "put"]);
        add("/api/ip/whitelist", &["post"]);
        add("/api/ip/whitelist/{pattern}", &["delete"]);
        // legacy /api/keys 两条已删（路由与文档表同步移除，见 paths_tokens 注释）
        add("/api/limits", &["get", "put"]);
        add("/api/agent/settings", &["get", "put"]);
        // Agent 工作台（src/agent/api.rs 独立 router）
        add("/api/agent/config", &["get"]);
        add("/api/agent/sessions", &["get", "post"]);
        add("/api/agent/sessions/{id}", &["delete", "get"]);
        add("/api/agent/sessions/{id}/chat", &["post"]);
        add("/api/agent/approvals/{request_id}", &["post"]);
        add("/api/gateway/settings", &["get", "put"]);
        add("/api/report/settings", &["get", "put"]);
        add("/api/routing", &["get", "put"]);
        add("/api/routing/defaults", &["get"]);
        // #46 补登：#40 多规则档案 / 候选队列 / 判定预演四条
        add("/api/routing/queue", &["get"]);
        add("/api/routing/profiles", &["get", "put"]);
        add(
            "/api/routing/profiles/group/{id}",
            &["delete", "get", "put"],
        );
        add("/api/routing/preview", &["post"]);
        // #38 粘滞会话管理面两条。此前漏登且测试不红 —— 本清单与 main.rs
        // 之间是**人工对照**（`covers_all_real_admin_routes` 只保证
        // 文档 ↔ 本清单相等，管不到本清单 ↔ 真实路由），漏 add 时两边
        // 一起少、断言仍绿。#46 已落地机器化对照：CI 闸门
        // `tools/extract_routes.py` 静态提取 main.rs 真实路由，双向断言
        // 「真实 ⊆ 文档∪豁免」+「文档 ⊆ 真实」，越界即红（见 tests::EXEMPT）。
        add("/api/routing/sticky", &["get"]);
        add("/api/routing/sticky/clear", &["post"]);
        add("/api/routing/replay/{request_id}", &["get"]);
        add("/api/update/cleanup-artifacts", &["post"]);
        add("/api/assistant/tools", &["get"]);
        add("/api/assistant/chat", &["post"]);
        add("/api/gateway/prompt", &["get", "put"]);
        // #46 补登：网关指纹/排队 + 安全策略 + 定时任务 + 数据升级
        add("/api/sanitize", &["get", "put"]);
        add("/api/purge", &["get", "put"]);
        add("/api/cline-headers", &["get", "put"]);
        add("/api/queue", &["get", "put"]);
        add("/api/security/policy", &["get", "put"]);
        add("/api/scheduled-tasks", &["get"]);
        add("/api/scheduled-tasks/{id}", &["patch", "post"]);
        add("/api/scheduled-tasks/{id}/run", &["post"]);
        add("/api/upgrade", &["get"]);
        add("/api/upgrade/run", &["post"]);
        add("/api/storage", &["get"]);
        add("/api/backup/export-encrypted", &["post"]);
        add("/api/backup/download", &["get"]);
        add("/api/backup/import-encrypted", &["post"]);
        add("/api/backup/import", &["post"]);
        add("/api/logs/audits", &["delete", "get"]);
        add("/api/logs/audits/clear", &["delete"]);
        add("/api/logs/cleanup", &["post"]);
        add("/api/logs/requests", &["delete", "get"]);
        add("/api/logs/requests/clear", &["delete"]);
        add("/api/logs/requests/export", &["get"]);
        add("/api/logs/retention", &["get", "put"]);
        add("/api/logs/runtime", &["get"]);
        add("/api/logs/runtime/export", &["get"]);
        add("/api/logs/inflight", &["get"]);
        add("/api/logs/inflight/{id}/terminate", &["post"]);
        add("/api/logs/requests/clear-preview", &["get"]);
        add("/api/logs/agg/rebuild", &["post"]);
        add("/api/logs/agg/tz-migrate", &["post"]);
        add("/api/mcp", &["post"]);
        add("/api/models/alias", &["post"]);
        add("/api/models/alias/remove", &["post"]);
        add("/api/models/alias/reasoning", &["post"]);
        add("/api/models/alias/state", &["post"]);
        add("/api/models/capabilities", &["post"]);
        add("/api/models/available", &["get"]);
        add("/api/models/custom", &["post"]);
        add("/api/models/custom/remove", &["post"]);
        add("/api/models/manage", &["get"]);
        add("/api/models/metadata", &["get", "put"]);
        add("/api/models/metadata/refresh", &["post"]);
        add("/api/models/refresh", &["post"]);
        add("/api/models/meta", &["get"]);
        add("/api/models/meta/{model}", &["delete", "put"]);
        add("/api/models/price", &["post"]);
        add("/api/models/price/remove", &["post"]);
        add("/api/models/state", &["post"]);
        add("/api/models/test", &["post"]);
        add("/api/checkin-center", &["get"]);
        add("/api/checkin-center/benefits", &["get"]);
        add("/api/checkin-center/benefits/run", &["post"]);
        add("/api/checkin-center/benefits/keepalive", &["get", "put"]);
        add("/api/checkin-center/records", &["delete", "get"]);
        add("/api/checkin-center/records/mark-kept-active", &["post"]);
        add("/api/checkin-center/run", &["post"]);
        add("/api/checkin-center/schedules", &["get", "post"]);
        add("/api/checkin-center/schedules/remove", &["post"]);
        add("/api/checkin-center/schedules/run", &["post"]);
        add("/api/checkin-center/settings", &["get", "put"]);
        add("/api/monitor/security", &["get"]);
        add("/api/monitor/security/events", &["get"]);
        add("/api/monitor/system", &["get"]);
        add("/api/monitor/health", &["get"]);
        add("/api/monitor/healthz", &["get"]);
        add("/api/network/accounts", &["get"]);
        add("/api/network/accounts/{account_id}", &["delete", "post"]);
        add("/api/network/config/{config_id}", &["put"]);
        add("/api/network/restart", &["post"]);
        add("/api/network/status", &["get"]);
        add("/api/notify/config", &["get", "put"]);
        add("/api/notify/test-email", &["post"]);
        add("/api/notify/test-slack", &["post"]);
        add("/api/notify/test-telegram", &["post"]);
        add("/api/notify/test-webhook", &["post"]);
        add("/api/oauth/config", &["get", "put"]);
        add("/api/openapi.json", &["get"]);
        add("/api/orders", &["get"]);
        add("/api/orders/me", &["get"]);
        add("/api/orders/{id}", &["delete", "get"]);
        add("/api/orders/{id}/complete", &["post"]);
        add("/api/plans", &["get", "post"]);
        add("/api/plans/{id}", &["delete"]);
        add("/api/plans/{id}/issue", &["post"]);
        add("/api/playground/chat", &["post"]);
        add("/api/playground/images", &["post"]);
        add("/api/playground/transcriptions", &["post"]);
        add("/api/playground/tts", &["post"]);
        add("/api/prices", &["get", "post"]);
        add("/api/prices/{model}", &["delete", "put"]);
        add("/api/pricing", &["get", "post"]);
        add("/api/pricing/{id}", &["delete"]);
        add("/api/pricing/estimate", &["post"]);
        add("/api/pricing/exchange-rates", &["get", "put"]);
        add("/api/pricing/missing-models", &["get"]);
        add("/api/pricing/sync", &["post"]);
        add("/api/pricing/sync-config", &["get", "put"]);
        add("/api/ratelimit/config", &["get", "put"]);
        add("/api/ratios", &["get", "put"]);
        add("/api/redemptions", &["get"]);
        add("/api/redemptions/batch", &["post"]);
        add("/api/redemptions/redeem", &["post"]);
        add("/api/redemptions/{id}", &["delete"]);
        add("/api/session", &["get"]);
        add("/api/session/refresh", &["post"]);
        add("/api/settings", &["get", "put"]);
        add("/api/stripe/topup", &["post"]);
        add("/api/subscription/admin/bind", &["post"]);
        add(
            "/api/subscription/admin/subscriptions/{id}/cancel",
            &["post"],
        );
        add("/api/subscription/admin/users/{id}/subscriptions", &["get"]);
        add("/api/subscription/balance/pay", &["post"]);
        add("/api/subscription/plans", &["get"]);
        add("/api/subscription/self", &["get"]);
        add("/api/tokens", &["get", "post"]);
        add("/api/tokens/today", &["get"]);
        add("/api/tokens/{id}", &["delete", "put"]);
        add("/api/tokens/{id}/key", &["get"]);
        add("/api/tokens/{id}/reset_used", &["post"]);
        add("/api/tokens/{id}/rotate", &["post"]);
        add("/api/topup", &["post"]);
        add("/api/topup/amount", &["post"]);
        add("/api/usage/models", &["get"]);
        add("/api/usage/summary", &["get", "post"]);
        add("/api/usage/trend", &["get"]);
        add("/api/user/records", &["get"]);
        add("/api/user/epay/notify", &["get", "post"]);
        add("/api/user/epay/return", &["get", "post"]);
        add("/api/user/stripe/webhook", &["post"]);
        add("/api/users", &["get", "post"]);
        add("/api/users/check", &["get"]);
        add("/api/users/manage", &["post"]);
        add("/api/users/me", &["get"]);
        add("/api/users/self", &["put"]);
        add("/api/users/{id}", &["delete", "put"]);
        add("/api/users/{id}/2fa", &["delete"]);
        add("/api/user-tools/policy", &["get", "put"]);
        add("/api/user-tools/catalog", &["get"]);
        add("/api/zcode/captcha", &["get", "post"]);
        add("/api/zcode/captcha-pool", &["get", "put"]);
        add("/api/zcode/captcha/config", &["get"]);
        add("/swagger-ui", &["get"]);
        m
    }

    /// 有意**不写进** OpenAPI 自描述文档的管理面路由：(路径, 理由)。
    ///
    /// 这些路由真实存在于 main.rs，但公开自描述会把运维 AI 引到它们上面、
    /// 放大凭据暴露面，故只做豁免、不做文档。豁免表是「真实路由 ⊆ 文档」这条
    /// 硬规矩的**唯一**例外出口，每条必须写明理由。
    ///
    /// 消费方：CI 闸门 `tools/extract_routes.py`（按文本解析本表）——
    /// 正向断言用 `文档 ∪ 豁免` 覆盖真实路由，豁免一侧即取自此处。
    /// 改本表格式时须同步脚本的正则。单测 `exempt_routes_stay_undocumented`
    /// 反向自检：豁免项若哪天被写进文档，说明理由已不成立，会红。
    const EXEMPT: &[(&str, &str)] = &[
        (
            "/api/debug",
            "调试模式开关，与 /api/debug/traffic 成对，仅供本机排障，不自描述",
        ),
        (
            "/api/debug/traffic",
            "返回上游原始报文，含 Authorization / API key 等凭据残留，不自描述",
        ),
    ];

    fn document_paths(doc: &Value) -> BTreeMap<String, Vec<String>> {
        let paths = doc["paths"].as_object().expect("paths is object");
        let mut out = BTreeMap::new();
        for (p, item) in paths {
            let mut methods = Vec::new();
            if let Some(ops) = item.as_object() {
                for (verb, _) in ops {
                    methods.push(verb.clone());
                }
            }
            methods.sort();
            out.insert(p.clone(), methods);
        }
        out
    }

    /// 文档形状冒烟：可构造、版本 3.1、paths 非空、全部以 / 开头、
    /// 每个操作有 responses、GET 无 requestBody。
    #[test]
    fn openapi_document_well_formed() {
        let doc = build_openapi_document();
        assert_eq!(doc["openapi"], "3.1.0");
        assert_eq!(doc["info"]["title"], "AIGX Admin API");
        assert!(doc["info"]["version"].as_str().is_some());
        let paths = doc["paths"].as_object().expect("paths is object");
        assert!(!paths.is_empty(), "paths 不能为空");
        for (p, item) in paths {
            assert!(p.starts_with('/'), "path 须以 / 开头: {p}");
            assert!(!p.contains(":id"), "path 须用 {{id}} 语法: {p}");
            for (verb, operation) in item.as_object().unwrap() {
                assert!(["get", "post", "put", "delete", "patch"].contains(&verb.as_str()));
                assert!(
                    operation
                        .get("responses")
                        .is_some_and(|r| !r.as_object().unwrap().is_empty()),
                    "{p} {verb} 缺 responses"
                );
                assert!(
                    operation
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .is_some_and(|s| !s.is_empty()),
                    "{p} {verb} 缺中文 summary"
                );
                if verb == "get" {
                    assert!(
                        operation.get("requestBody").is_none(),
                        "GET {p} 不应有 requestBody"
                    );
                }
            }
        }
        // 全局 Bearer security scheme 存在
        assert!(doc["components"]["securitySchemes"]["bearerAuth"].is_object());
    }

    /// 硬验收：文档 paths 与 main.rs 真实管理面路由一一对应，不虚构不遗漏。
    /// 差集输出（Assert diff）会完整打印两侧，便于定位增删路由后的不同步。
    #[test]
    fn covers_all_real_admin_routes() {
        let expected: BTreeMap<String, Vec<String>> = real_admin_routes()
            .into_iter()
            .map(|(p, methods)| {
                (
                    p.to_string(),
                    methods.into_iter().map(|m| m.to_string()).collect(),
                )
            })
            .collect();
        let actual = document_paths(&build_openapi_document());
        assert_eq!(
            actual, expected,
            "文档 paths 与 main.rs 真实管理面路由不一致（左侧=文档，右侧=路由表）"
        );
    }

    /// 豁免表自检：每条豁免路径必须「真实存在但不入文档」。
    ///
    /// - 理由非空：豁免的唯一出口必须写明为什么，否则后人无从判断可否解除；
    /// - 不在文档里：若哪天有人把豁免路由写进了文档，说明「不公开自描述」的
    ///   理由已不成立，须重新评估（要么撤豁免、要么撤文档条目）。
    ///
    /// 注意本测只覆盖「豁免 ↔ 文档」这一侧；「豁免 ↔ main.rs 真实路由」由
    /// CI 闸门 `tools/extract_routes.py` 覆盖（单测无法读 main.rs 路由）。
    #[test]
    fn exempt_routes_stay_undocumented() {
        let doc = document_paths(&build_openapi_document());
        for (path, reason) in EXEMPT {
            assert!(!reason.trim().is_empty(), "豁免路由 {path} 缺理由");
            assert!(
                !doc.contains_key(*path),
                "豁免路由 {path} 已被写进文档，豁免理由不再成立，请撤豁免或撤文档条目"
            );
        }
    }

    /// 数量锚点：文档路径数必须等于路由对照表的条目数。
    ///
    /// **不写死数字** —— 路由增删时这个数字必然要跟着改，而改的人往往只改了
    /// 一处（文档或对照表），于是这个断言变成了「每次加路由都要手改的常量」。
    /// 改成从 [`real_admin_routes`] 派生后：
    /// - 文档多了一条没进表 → `covers_all_real_admin_routes` 红（逐条比对）；
    /// - 文档少了一条 → 本断言红（数量不等）；
    ///
    /// 两边同步改了就都绿，不用记着改常量。
    #[test]
    fn path_count_anchor() {
        let doc = build_openapi_document();
        let n = doc["paths"].as_object().unwrap().len();
        let expected = real_admin_routes().len();
        assert_eq!(
            n, expected,
            "文档路径数应等于路由对照表条目数（{expected}），实际 {n}"
        );
    }
}
