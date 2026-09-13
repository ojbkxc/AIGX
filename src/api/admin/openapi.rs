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
        paths_channels(),
        paths_tokens(),
        paths_pricing(),
        paths_payment(),
        paths_logs(),
        paths_dashboard_usage(),
        paths_settings_ops(),
        paths_monitor_security(),
        paths_playground(),
        paths_accounts_network(),
        paths_docs(),
        paths_diagnostics(),
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
        "/api/auth/github": { "get": op_pub("认证", "GitHub OAuth 授权跳转") },
        "/api/auth/github/callback": { "get": op_pub("认证", "GitHub OAuth 回调") },
        "/api/auth/google": { "get": op_pub("认证", "Google OAuth 授权跳转") },
        "/api/auth/google/callback": { "get": op_pub("认证", "Google OAuth 回调") },
        "/api/auth/linuxdo": { "get": op_pub("认证", "LinuxDO OAuth 授权跳转") },
        "/api/auth/linuxdo/callback": { "get": op_pub("认证", "LinuxDO OAuth 回调") }
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
        "/api/checkin": {
            "get": op("用户", "查询今日签到状态"),
            "post": op("用户", "执行每日签到（送配额）")
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
            "get": op_params("令牌", "列出令牌（本人；管理员可携 all/user 过滤）",
                &[query_param("all", "string", "管理员查看全部（任意非空值生效）"), query_param("user", "string", "按邮箱/UUID 筛选（仅管理员）")]),
            "post": op_body("令牌", "创建令牌", "name: string, expired_time/remain_quota 等可选字段")
        },
        "/api/tokens/{id}": {
            "put": op_params("令牌", "更新令牌", &[path_param("id")]),
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
        "/api/tokens/today": { "get": op("令牌", "今日用量摘要（token 维度）") },
        "/api/keys": {
            "get": op("令牌", "列出 API Key"),
            "post": op_body("令牌", "新增 API Key", "key/remark: string")
        },
        "/api/keys/{id}": {
            "delete": op_params("令牌", "删除 API Key", &[path_param("id")])
        }
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
            "get": op("定价", "列出模型价格（legacy 别名）"),
            "post": op_body("定价", "写入模型价格", "model: string, price 字段组")
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
        add("/api/accounts", &["get", "post"]);
        add("/api/accounts/test", &["post"]);
        add("/api/accounts/{id}", &["delete", "put"]);
        add("/api/aff", &["get"]);
        add("/api/aff_transfer", &["post"]);
        add("/api/alerts/active", &["get"]);
        add("/api/alerts/history", &["get"]);
        add("/api/alerts/rules", &["get", "put"]);
        add("/api/alerts/test", &["post"]);
        add("/api/auth/change-password", &["post"]);
        add("/api/auth/forgot-password", &["post"]);
        add("/api/auth/github", &["get"]);
        add("/api/auth/github/callback", &["get"]);
        add("/api/auth/google", &["get"]);
        add("/api/auth/google/callback", &["get"]);
        add("/api/auth/linuxdo", &["get"]);
        add("/api/auth/linuxdo/callback", &["get"]);
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
        add("/api/checkin", &["get", "post"]);
        add("/api/dashboard/cache_savings", &["get"]);
        add("/api/dashboard/channel_health", &["get"]);
        add("/api/dashboard/consumption_trend", &["get"]);
        add("/api/dashboard/model_distribution", &["get"]);
        add("/api/dashboard/realtime", &["get"]);
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
        add("/api/keys", &["get", "post"]);
        add("/api/keys/{id}", &["delete"]);
        add("/api/limits", &["get", "put"]);
        add("/api/logs/audits", &["delete", "get"]);
        add("/api/logs/audits/clear", &["delete"]);
        add("/api/logs/cleanup", &["post"]);
        add("/api/logs/requests", &["delete", "get"]);
        add("/api/logs/requests/clear", &["delete"]);
        add("/api/logs/requests/export", &["get"]);
        add("/api/logs/retention", &["get", "put"]);
        add("/api/models/available", &["get"]);
        add("/api/models/meta", &["get"]);
        add("/api/models/meta/{model}", &["delete", "put"]);
        add("/api/monitor/security", &["get"]);
        add("/api/monitor/security/events", &["get"]);
        add("/api/monitor/system", &["get"]);
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
        add("/api/settings", &["get", "put"]);
        add("/api/stripe/topup", &["post"]);
        add("/api/subscription/admin/bind", &["post"]);
        add("/api/subscription/admin/subscriptions/{id}/cancel", &["post"]);
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
        add("/swagger-ui", &["get"]);
        m
    }

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
            assert!(!p.contains(":id"), "path 须用 {id} 语法: {p}");
            for (verb, operation) in item.as_object().unwrap() {
                assert!(["get", "post", "put", "delete", "patch"].contains(&verb.as_str()));
                assert!(
                    operation.get("responses").is_some_and(|r| !r.as_object().unwrap().is_empty()),
                    "{p} {verb} 缺 responses"
                );
                assert!(
                    operation.get("summary").and_then(|s| s.as_str()).is_some_and(|s| !s.is_empty()),
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
            actual,
            expected,
            "文档 paths 与 main.rs 真实管理面路由不一致（左侧=文档，右侧=路由表）"
        );
    }


    /// 数量锚点：管理面真实路径共 142 个（141 + openapi.json 自身）。
    /// 真实路由增删会先在 covers_all_real_admin_routes 红，此断言辅助定位。
    #[test]
    fn path_count_anchor() {
        let doc = build_openapi_document();
        let n = doc["paths"].as_object().unwrap().len();
        assert_eq!(
            n,
            142,
            "管理面路径数应为 142（141 真实注册 + openapi.json），实际 {n}"
        );
    }
}