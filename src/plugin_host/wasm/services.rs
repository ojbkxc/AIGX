// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 宿主能力分派 —— 插件经 `agent2api.call` 请求的每一项能力在这里落地。
//!
//! 与上游逐条对齐（方法名、参数名、上限、错误码）：
//! `clock.now` / `random.bytes` / `account.get` / `account.update` /
//! `ledger.get|put` / `settings.get` / `catalog.get` / `lock.acquire|release` /
//! `session.put|get|remove` / `native.call` / `state.get|compare_exchange` /
//! `resource.*` / `clock.sleep` / `http.open` / `ws.open` / `http.read|close` /
//! `ws.read|send|close` / `stream.head|chunk`。
//!
//! 每次调用都先查能力白名单（[`Context::permission`]），未授权的能力一律
//! `forbidden` —— 插件拿不到「没申报但宿主有」的任何东西。

use base64::Engine;
use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// DeepSeek 网页验证计算只允许 `deepseek-web`（与上游 manifest 限制同口径）。
fn deepseek_pow_provider_allowed(provider: &str) -> bool {
    provider == "deepseek-web"
}

/// 深验证难度的硬上限（与上游 `MAX_DIFFICULTY` 一致，防 CPU 被拉爆）。
fn deepseek_pow_difficulty_allowed(difficulty: i64) -> bool {
    (1..=2_000_000).contains(&difficulty)
}

/// 已知**未服务**的方法命名空间 → 它对应的能力名。
///
/// 为什么需要这张表：这些名字在上游 ABI 里存在，AIGX 的 `CAPABILITIES` 也放行
/// 声明，但宿主没实现（`SERVED_CAPABILITIES` 里没有对应项，`contract()` 里的
/// `served` 也是 false）。此前它们落进兜底分支只回 `NO_METHOD`「未支持的主程序
/// 能力」，插件作者分不清「名字写错」和「网关这家没做」——登录流走到一半撞上
/// 尤其难查。这里改成显式 `unsupported` + 指名能力，语义与 `contract()` 一致，
/// 不改变任何「已服务」判定。
const UNSERVED_METHODS: &[(&str, &str)] = &[
    ("login.desktop.credentials.take", "login.desktop.chatglm.v1"),
    ("checkin.claim", "checkin.rolling.v1"),
    ("resource.cookie-file.take", "resources.cookie-file.v1"),
    (
        "runtime.client_fingerprint",
        "runtime.client-fingerprint.v1",
    ),
    // `credentials.legacy.take` 与其能力 `credentials.legacy-import.v1` 已随
    // 上游 beta.8 删除（legacy_credentials 模块移除）；名字留在这里只为让
    // 装了旧版插件的调用拿到指名报错而不是 NO_METHOD。能力已不在
    // CAPABILITIES 里，插件清单也不再放行该声明。
    ("credentials.legacy.take", "已随上游删除"),
    ("crypto.monkeycode.cap.solve", "crypto.monkeycode-cap.v1"),
];

/// 兜底分支的错误：未服务的方法按能力名明说，其余仍是 `NO_METHOD`。
fn unsupported_method(method: &str) -> WireError {
    match UNSERVED_METHODS.iter().find(|(name, _)| *name == method) {
        Some((_, capability)) => WireError::with_code(
            format!("网关未实现该方法（能力 {capability} 未服务）"),
            "unsupported",
        ),
        None => WireError::with_code("插件请求了未支持的主程序能力", WireError::NO_METHOD),
    }
}

impl Context {
    pub(super) fn permission(&self, name: &str) -> Result<(), WireError> {
        if self.shared.manifest.allows(name) {
            Ok(())
        } else {
            Err(WireError::with_code("插件未获此能力授权", "forbidden"))
        }
    }

    /// 当前调用的账号（校验归属）。
    pub(super) fn account(&self) -> Result<&str, WireError> {
        if self.account.is_empty() {
            return Err(super::wire::error("当前调用没有账号"));
        }
        let provider = &self.shared.manifest.provider;
        if !self.shared.accounts.provider_allowed(provider)
            || self
                .shared
                .accounts
                .account_provider(&self.account)
                .as_ref()
                != Some(provider)
        {
            return Err(WireError::with_code("账号不属于当前插件", "forbidden"));
        }
        Ok(&self.account)
    }

    fn resource(&self, params: &Value, kind: &str) -> Result<String, WireError> {
        let id = text(params, "id");
        if self.resources.get(id).copied() != Some(kind) {
            return Err(WireError::with_code("连接不属于当前调用", "forbidden"));
        }
        Ok(id.to_string())
    }

    pub async fn invoke(&mut self, method: &str, mut params: Value) -> Result<Value, WireError> {
        self.calls += 1;
        if self.calls > 100_000 {
            return Err(WireError::with_code(
                "插件调用次数超出上限",
                "plugin_resource_limit",
            ));
        }
        if method != "clock.now" && !params.is_object() {
            return Err(super::wire::error("插件能力参数必须是 JSON 对象"));
        }
        let provider = self.shared.manifest.provider.clone();
        match method {
            "clock.now" => {
                self.permission("clock.v1")?;
                Ok(json!(super::now_ms()))
            }
            // `runtime.platform`：插件按宿主平台分支（上游取值 `win`/`mac`/`linux`/`other`）。
            // 只回本进程常量，零参数、不泄露任何环境细节。
            "runtime.platform" => {
                self.permission("runtime.platform.v1")?;
                if params.as_object().is_none_or(|fields| !fields.is_empty()) {
                    return Err(WireError::with_code("系统类型查询不接受参数", "bad_params"));
                }
                Ok(json!(match std::env::consts::OS {
                    "windows" => "win",
                    "macos" => "mac",
                    "linux" => "linux",
                    _ => "other",
                }))
            }
            // `runtime.architecture`：CPU 架构三元组里的 arch 段，零参数。
            "runtime.architecture" => {
                self.permission("runtime.architecture.v1")?;
                if params.as_object().is_none_or(|fields| !fields.is_empty()) {
                    return Err(WireError::with_code("运行环境查询不接受参数", "bad_params"));
                }
                Ok(json!(std::env::consts::ARCH))
            }
            "random.bytes" => {
                self.permission("random.v1")?;
                let count = params["length"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= 256)
                    .ok_or_else(|| super::wire::error("随机字节长度无效"))?
                    as usize;
                let mut bytes = vec![0; count];
                getrandom::getrandom(&mut bytes)
                    .map_err(|_| super::wire::error("系统随机数不可用"))?;
                Ok(json!(
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ))
            }
            "account.get" => {
                self.permission("accounts.cas.v1")?;
                if self.account.is_empty() {
                    return Err(WireError::with_code(
                        "当前操作未被主程序授予账号上下文",
                        "forbidden",
                    ));
                }
                let record = self
                    .shared
                    .accounts
                    .store_get(&provider, &self.account)
                    .await?;
                self.account = record["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| super::wire::error("账号记录缺少标识"))?
                    .to_string();
                self.account()?;
                self.identity = record["identity"]
                    .as_str()
                    .unwrap_or(&self.account)
                    .to_string();
                self.user_agent = Some(
                    record["public"]["userAgent"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                );
                if !self.proxy_selected {
                    self.proxy = match record["proxy"]["state"].as_str() {
                        Some("failed") => return Err(super::wire::error("账号代理不可用")),
                        Some("resolved") => record["proxy"]["proxy"].clone(),
                        Some("static") => self
                            .shared
                            .egress
                            .resolve_proxy(&json!({ "config": record["proxy"]["config"] }))?,
                        _ => Value::Null,
                    };
                    self.proxy_selected = true;
                }
                let mut record = record;
                if let Some(map) = record.as_object_mut() {
                    map.remove("proxy");
                }
                Ok(record)
            }
            "account.update" => {
                self.permission("accounts.cas.v1")?;
                if params.to_string().len() > 512 * 1024 {
                    return Err(super::wire::error("插件账号写入超过大小限制"));
                }
                let account = self.account()?.to_string();
                if !params["secret"].is_object()
                    || !params["expect"].is_object()
                    || !params["public"].is_object()
                {
                    return Err(super::wire::error(
                        "账号写入必须包含凭据原值、新值和公开视图",
                    ));
                }
                super::public_view::validate(&params["public"], &params["secret"])?;
                self.shared
                    .accounts
                    .store_update_with_public(
                        &account,
                        params["expect"].clone(),
                        params["secret"].clone(),
                        params["public"].clone(),
                    )
                    .await
            }
            "ledger.get" | "ledger.put" => {
                self.permission("accounts.cas.v1")?;
                if method == "ledger.put" && params["value"].to_string().len() > 64 * 1024 {
                    return Err(super::wire::error("插件台账超过大小限制"));
                }
                let account = self.account()?.to_string();
                let kind = text(&params, "kind").to_string();
                if kind.len() > 80 || kind.is_empty() {
                    return Err(super::wire::error("台账名称无效"));
                }
                if method == "ledger.get" {
                    self.shared.accounts.ledger_get(&account, &kind).await
                } else {
                    self.shared
                        .accounts
                        .ledger_put(&account, &kind, params["value"].clone())
                        .await?;
                    Ok(Value::Null)
                }
            }
            "settings.get" => {
                self.permission("settings.v1")?;
                let key = text(&params, "key");
                if key.len() > 128 || key.is_empty() {
                    return Err(super::wire::error("插件设置名称无效"));
                }
                self.shared.accounts.settings_get(&provider, key).await
            }
            "catalog.get" => {
                self.permission("catalog.v1")?;
                if self.shared.manifest.allows("catalog.accounts.v1") {
                    return super::catalog::cached(&provider, self.account()?).await;
                }
                self.shared
                    .accounts
                    .settings_get(
                        &provider,
                        crate::plugin_host::model_catalog::SETTING_MODEL_CATALOG,
                    )
                    .await
            }
            "lock.acquire" => {
                self.permission("locks.v1")?;
                let account = self.account()?.to_string();
                if !self.guards.is_empty() {
                    return Err(super::wire::error("同一次调用不能嵌套申请账号锁"));
                }
                let scope = text(&params, "scope");
                let key = if scope == "conversation" {
                    let session = match params.get("conversation") {
                        Some(value) => value
                            .as_str()
                            .filter(|key| {
                                !key.is_empty() && key.len() <= 256 && !key.contains('\0')
                            })
                            .map(|key| format!("plugin:{key}"))
                            .ok_or_else(|| super::wire::error("会话锁标识无效"))?,
                        None => self.session.clone(),
                    };
                    if session.is_empty() {
                        return Err(super::wire::error("当前请求缺少会话标识"));
                    }
                    format!("{provider}:{account}:conversation:{session}")
                } else if scope.is_empty() || scope == "account" {
                    format!("{provider}:{account}")
                } else {
                    return Err(super::wire::error("锁范围无效"));
                };
                // beta.4：账号锁等待受「调用截止 / 10s 上限」约束（`call_deadline.min(now+10s)`）。
                // AIGX 原先无上界，锁争用会一直等到外层调用超时，最终折成 timeout/aborted；
                // 这里改为超时即返回可重试的 plugin_busy。beta.4 仅给
                // `with_code(message, "plugin_busy")`（原因码后续按文案反查），AIGX 直接把
                // reason/retry_after 结构化带出（可选字段，缺省省略、向后兼容）。
                let deadline = self
                    .call_deadline
                    .min(tokio::time::Instant::now() + std::time::Duration::from_secs(10));
                let guard =
                    tokio::time::timeout_at(deadline, self.shared.account_lock(&key).lock_owned())
                        .await
                        .map_err(|_| {
                            WireError::busy(
                                "当前账号还有其他操作，等待账号锁超时，请稍后重试",
                                "account_lock",
                                1,
                            )
                        })?;
                self.guards.insert("account".into(), guard);
                Ok(json!("account"))
            }
            "lock.release" => {
                self.guards.remove(text(&params, "id"));
                Ok(Value::Null)
            }
            "session.put" | "session.get" | "session.remove" => {
                self.permission("sessions.v1")?;
                let now = super::now_ms();
                let mut sessions = self
                    .shared
                    .sessions
                    .lock()
                    .map_err(|_| super::wire::error("插件临时会话不可用"))?;
                sessions.retain(|_, (until, _)| *until > now);
                let id = text(&params, "id");
                if id.is_empty() || id.len() > 256 {
                    return Err(super::wire::error("插件临时会话标识无效"));
                }
                if method == "session.remove" {
                    sessions.remove(id);
                    return Ok(Value::Null);
                }
                if method == "session.get" {
                    return Ok(sessions
                        .get(id)
                        .map(|(_, data)| data.clone())
                        .unwrap_or(Value::Null));
                }
                if (sessions.len() >= 64 && !sessions.contains_key(id))
                    || params["value"].to_string().len() > 64 * 1024
                {
                    return Err(super::wire::error("插件临时会话超过限额"));
                }
                let ttl = params["ttl_ms"]
                    .as_i64()
                    .unwrap_or(300_000)
                    .clamp(1000, 900_000);
                sessions.insert(id.to_string(), (now + ttl, params["value"].clone()));
                Ok(Value::Null)
            }
            "native.call" => Box::pin(self.native(&params)).await,
            "state.get" | "state.compare_exchange" => self.state(method, &params).await,
            "resource.create" | "resource.append" | "resource.seal" | "resource.read"
            | "resource.remove" => {
                self.permission("resources.v1")?;
                let id = text(&params, "id");
                match method {
                    "resource.create" => self.blobs.create(text(&params, "mime")),
                    "resource.append" => self.blobs.append(id, text(&params, "data")),
                    "resource.seal" => self.blobs.seal(id),
                    "resource.read" => self.blobs.read(
                        id,
                        params["offset"].as_u64().unwrap_or(0) as usize,
                        params["length"].as_u64().unwrap_or(64 * 1024) as usize,
                    ),
                    _ => {
                        self.blobs.remove(id);
                        Ok(Value::Null)
                    }
                }
            }
            "clock.sleep" => {
                self.permission("clock.v1")?;
                let millis = params["milliseconds"]
                    .as_u64()
                    .filter(|ms| *ms <= 60_000)
                    .ok_or_else(|| super::wire::error("等待时长必须在一分钟以内"))?;
                tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
                Ok(Value::Null)
            }
            "http.open" | "ws.open" => {
                let websocket = method == "ws.open";
                self.permission(if websocket { "websocket.v1" } else { "http.v1" })?;
                if self.resources.len() >= 8 {
                    return Err(super::wire::error("单次插件调用连接数超限"));
                }
                self.shared
                    .manifest
                    .check_url(text(&params, "url"), websocket)?;
                if self.user_agent.is_none() && !self.account.is_empty() {
                    let account = self.account()?.to_string();
                    let record = self.shared.accounts.store_get(&provider, &account).await?;
                    self.user_agent = Some(
                        record["public"]["userAgent"]
                            .as_str()
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                    );
                }
                let user_agent = self.user_agent.as_deref().unwrap_or("");
                if !user_agent.is_empty() {
                    if params.get("headers").is_none_or(Value::is_null) {
                        params["headers"] = json!({});
                    }
                    let headers = params["headers"]
                        .as_object_mut()
                        .ok_or_else(|| super::wire::error("插件请求头格式无效"))?;
                    headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
                    headers.insert("User-Agent".to_string(), json!(user_agent));
                }
                if params["body"]
                    .as_str()
                    .is_some_and(|body| body.len() > super::MAX_MESSAGE)
                {
                    return Err(super::wire::error("插件请求体过大"));
                }
                let id = format!("wasm-{}", super::net_id());
                params["id"] = json!(id);
                params["proxy"] = self.proxy.clone();
                params["provider"] = json!(provider);
                params["trace"] = self.trace.clone();
                params["max_chunk"] = json!(64 * 1024);
                let body = if websocket {
                    None
                } else {
                    super::http::body(self, &params)?
                };
                if let Some(body) = body {
                    let encoded = super::http::encode_body(body)?;
                    params["body_b64"] = json!(encoded.body_b64);
                    if let Some(content_type) = encoded.content_type {
                        if params.get("headers").is_none_or(Value::is_null) {
                            params["headers"] = json!({});
                        }
                        if let Some(headers) = params["headers"].as_object_mut() {
                            headers
                                .entry("Content-Type".to_string())
                                .or_insert_with(|| json!(content_type));
                        }
                    }
                }
                self.resources
                    .insert(id.clone(), if websocket { "ws" } else { "http" });
                let result = if websocket {
                    self.shared.egress.ws_open(params.clone()).await
                } else {
                    self.shared.egress.request(params.clone()).await
                };
                if result.is_err() {
                    self.resources.remove(&id);
                }
                result
            }
            "http.read" => {
                let id = self.resource(&params, "http")?;
                let result = self.shared.egress.read(&id).await;
                if result.as_ref().is_ok_and(|value| value["done"] == true) || result.is_err() {
                    self.resources.remove(&id);
                }
                result
            }
            "http.close" => {
                let id = self.resource(&params, "http")?;
                let result = self.shared.egress.close(&id).await;
                self.resources.remove(&id);
                result
            }
            "ws.read" | "ws.send" | "ws.close" => {
                let id = self.resource(&params, "ws")?;
                match method {
                    "ws.read" => self.shared.egress.ws_read(params.clone()).await,
                    "ws.send" => self.shared.egress.ws_send(params.clone()).await,
                    _ => {
                        let result = self.shared.egress.ws_close(params.clone()).await;
                        self.resources.remove(&id);
                        result
                    }
                }
            }
            "stream.head" => {
                self.permission("chat.stream.v1")?;
                let sink = self
                    .sink
                    .as_ref()
                    .ok_or_else(|| super::wire::error("当前不是流式调用"))?;
                if self.head_sent {
                    return Err(super::wire::error("插件重复发送响应头"));
                }
                let status = params["status"]
                    .as_u64()
                    .and_then(|s| u16::try_from(s).ok())
                    .filter(|s| (100..600).contains(s))
                    .ok_or_else(|| super::wire::error("插件响应状态无效"))?;
                let headers: Vec<(String, String)> =
                    serde_json::from_value(params["headers"].clone())
                        .map_err(|_| super::wire::error("插件响应头格式无效"))?;
                if headers.len() > 64 || params.to_string().len() > 32 * 1024 {
                    return Err(super::wire::error("插件响应头过大"));
                }
                sink.out.head(status, headers)?;
                self.head_sent = true;
                Ok(Value::Null)
            }
            "stream.chunk" => {
                self.permission("chat.stream.v1")?;
                if !self.head_sent {
                    return Err(super::wire::error("插件尚未发送响应头"));
                }
                let sink = self
                    .sink
                    .as_ref()
                    .ok_or_else(|| super::wire::error("当前不是流式调用"))?;
                let data = text(&params, "data");
                if data.len() > 128 * 1024 {
                    return Err(super::wire::error("插件流式分片过大"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| super::wire::error("插件流式编码无效"))?;
                for chunk in bytes.chunks(64 * 1024) {
                    let credit = sink
                        .credit
                        .clone()
                        .acquire_many_owned((chunk.len() + super::FRAME_OVERHEAD) as u32)
                        .await
                        .map_err(|_| super::wire::error("插件流已关闭"))?;
                    sink.out.chunk(chunk.to_vec())?;
                    credit.forget();
                }
                Ok(Value::Null)
            }
            "verification.lease.open" | "verification.lease.check" | "verification.lease.close" => {
                self.verification_lease(method, &params).await
            }
            // 协议转换（`protocol.convert.v1`）：只做转换、不发网络请求；
            // 句柄与输入缓冲归当前调用所有（对齐上游 `wasm/protocol.rs`）。
            "protocol.request" | "protocol.open" | "protocol.push" | "protocol.close" => {
                self.protocol(method, &params).await
            }
            // 托管登录（`login.managed.v1`）：只在宿主签发的同一 flow 会话内可
            // 用；会话不存在即 forbidden（对齐上游 `wasm/managed.rs`）。
            "login.context"
            | "login.input.take"
            | "login.step.publish"
            | "login.loopback.open"
            | "login.loopback.close" => self.managed_login(method, &params).await,
            "managed_task.create"
            | "managed_task.observe"
            | "managed_task.read"
            | "managed_task.status"
            | "managed_task.close"
            | "managed_task.begin"
            | "managed_task.end"
            | "managed_task.abort" => self.managed_task(method, &params).await,
            "http.open_account" => self.open_account(&params).await,
            // 账号池「从本机客户端导入」：插件没有令牌，要求宿主读本机官方客户端的
            // 登录态（workbuddy / raccoon / kiro / kuku / loomy …）。零参数、只读、
            // 固定来源，见 `desktop_credentials` 模块头。
            "desktop.credentials.read" => {
                #[cfg(feature = "sqlite-kv")]
                {
                    Box::pin(super::desktop_credentials::invoke(self, &params)).await
                }
                // 未启用 `sqlite-kv` 的构建里客户端库（SQLite）根本读不了：
                // 如实说"本机构建不支持"，而不是假装读失败。
                #[cfg(not(feature = "sqlite-kv"))]
                {
                    Err(WireError::with_code(
                        "本机构建未启用 sqlite-kv，无法读取本机客户端登录态",
                        "unsupported",
                    ))
                }
            }
            // DeepSeek 网页版 DeepSeekHashV1 PoW 求解（复用原生
            // `bridge::session::deepseek_pow`，仅把既有哈希/穷举暴露给插件宿主）。
            "crypto.deepseek.pow.solve" => self.deepseek_pow_solve(&params).await,
            // Kiro `external-idp` 续期：端点只有插件知道，用注入 egress 换证；
            // 一次性 refresh_token 靠账号级 CAS 占位防重复消耗。
            "credential.kiro.external.refresh" => {
                Box::pin(super::external_refresh::invoke(self, &params)).await
            }
            // `request.context`：插件问「本次请求原本是不是客户端要的流式」。
            // 只有对话请求带这个值；其余调用（目录/签到/登录）如实 forbidden，
            // 不编默认值——插件据此分支的行为必须建立在真实语义上。
            "request.context" => {
                self.permission("request.context.v1")?;
                if params.as_object().is_none_or(|fields| !fields.is_empty()) {
                    return Err(WireError::with_code(
                        "请求上下文查询不接受参数",
                        "bad_params",
                    ));
                }
                let client_stream = self.client_stream.ok_or_else(|| {
                    WireError::with_code("当前调用没有原始对话请求上下文", "forbidden")
                })?;
                Ok(json!({ "client_stream": client_stream }))
            }
            // 公网图片：只产本次调用资源，不借账号 UA/代理/凭据；见 `resource_fetch`。
            "resource.fetch_public" => {
                Box::pin(super::resource_fetch::fetch_public(self, &params)).await
            }
            _ => Err(unsupported_method(method)),
        }
    }

    /// `crypto.deepseek.pow.solve`：把**已存在的**原生 DeepSeekHashV1 PoW
    /// （`bridge::session::deepseek_pow`）暴露给插件宿主。
    ///
    /// 入参 `{challenge, path}`；出参 `{response: base64(JSON{...answer...})}`。
    /// 仅 `deepseek-web` 可用；账户绑定存在时顺带校验归属（拿不到则放行）。
    /// 不新增任何指纹/反检测逻辑 —— 只是把原生求解搬到受控阻塞线程池。
    async fn deepseek_pow_solve(&self, params: &Value) -> Result<Value, WireError> {
        self.permission("crypto.deepseek-pow.v1")?;
        if !deepseek_pow_provider_allowed(&self.shared.manifest.provider) {
            return Err(WireError::with_code(
                "DeepSeek 网页验证计算能力不能跨提供商使用",
                "forbidden",
            ));
        }
        if !self.account.is_empty() {
            // 防御性：账户存在时校验其确属当前插件 provider（deepseek-web）。
            self.account()?;
        }

        let path = text(params, "path");
        if !matches!(path, "/api/v0/chat/completion" | "/api/v0/file/upload_file") {
            return Err(WireError::with_code(
                "DeepSeek 验证目标路径无效",
                "bad_params",
            ));
        }
        let challenge_value = params
            .get("challenge")
            .filter(|value| value.is_object())
            .ok_or_else(|| WireError::with_code("DeepSeek 验证参数无效", "bad_params"))?;
        let challenge =
            crate::bridge::session::deepseek_pow::PowChallenge::from_value(challenge_value)
                .ok_or_else(|| {
                    WireError::with_code("DeepSeek 没有返回有效的验证挑战", "bad_params")
                })?;
        if challenge.algorithm != "DeepSeekHashV1" {
            return Err(WireError::with_code(
                "DeepSeek 网页验证算法已变化，请更新插件",
                "bad_params",
            ));
        }
        if challenge.target_path != path {
            return Err(WireError::with_code(
                "DeepSeek 验证挑战不属于本次目标路径",
                "bad_params",
            ));
        }
        if !deepseek_pow_difficulty_allowed(challenge.difficulty) {
            return Err(WireError::with_code(
                "DeepSeek 验证难度过高或无效",
                "bad_params",
            ));
        }

        // 受控执行：阻塞穷举放到专用线程池，套 20s 上限（且不越过本次调用截止）。
        let deadline = self
            .call_deadline
            .min(tokio::time::Instant::now() + std::time::Duration::from_secs(20));
        let solver = challenge.clone();
        let joined = tokio::task::spawn_blocking(move || {
            crate::bridge::session::deepseek_pow::solve(&solver)
        });
        let answer = match tokio::time::timeout_at(deadline, joined).await {
            Ok(Ok(Some(answer))) => answer,
            Ok(Ok(None)) => {
                return Err(WireError::with_code(
                    "DeepSeek 网页验证没有找到有效答案，未提交对应请求",
                    "deepseek_pow_unsolved",
                ))
            }
            Ok(Err(_)) => {
                return Err(WireError::with_code(
                    "DeepSeek 网页验证计算任务已中断",
                    "deepseek_pow_unavailable",
                ))
            }
            Err(_) => {
                return Err(WireError::with_code(
                    "DeepSeek 网页验证计算超时，未提交对应请求",
                    "deepseek_pow_timeout",
                ))
            }
        };
        let response = challenge.header_with_answer(answer).ok_or_else(|| {
            WireError::with_code("DeepSeek 验证证明生成失败", "deepseek_pow_unavailable")
        })?;
        Ok(json!({ "response": response }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn deepseek_pow_only_deepseek_web() {
        assert!(deepseek_pow_provider_allowed("deepseek-web"));
        for provider in ["deepseek", "chatglm", "openai_compatible", ""] {
            assert!(
                !deepseek_pow_provider_allowed(provider),
                "{provider} 不应被允许求解 DeepSeek PoW"
            );
        }
    }

    #[test]
    fn deepseek_pow_difficulty_bounds() {
        assert!(deepseek_pow_difficulty_allowed(1));
        assert!(deepseek_pow_difficulty_allowed(144_000));
        assert!(deepseek_pow_difficulty_allowed(2_000_000));
        assert!(!deepseek_pow_difficulty_allowed(0));
        assert!(!deepseek_pow_difficulty_allowed(2_000_001));
    }

    /// 复用的原生 PoW：解析 challenge → solve → header 证明，向量与
    /// `bridge::session::deepseek_pow` 的单测一致。
    #[test]
    fn deepseek_pow_solve_roundtrip_matches_native_vector() {
        use crate::bridge::session::deepseek_pow::{deepseek_hash_v1, PowChallenge};
        let (salt, expire, answer) = ("testsalt", 1_700_000_000i64, 42i64);
        let digest = hex::encode(deepseek_hash_v1(
            format!("{salt}_{expire}_{answer}").as_bytes(),
        ));
        let challenge = PowChallenge::from_value(&serde_json::json!({
            "algorithm": "DeepSeekHashV1",
            "challenge": digest,
            "salt": salt,
            "expire_at": expire,
            "difficulty": 1_000,
            "signature": "sig",
            "target_path": "/api/v0/chat/completion",
        }))
        .expect("challenge 应可解析");
        assert_eq!(
            crate::bridge::session::deepseek_pow::solve(&challenge),
            Some(answer)
        );
        let header = challenge.header_with_answer(answer).expect("header");
        let value: Value = serde_json::from_slice(
            &base64::engine::general_purpose::STANDARD
                .decode(header)
                .expect("base64"),
        )
        .expect("json");
        assert_eq!(value["algorithm"], "DeepSeekHashV1");
        assert_eq!(value["answer"], answer);
        assert_eq!(value["target_path"], "/api/v0/chat/completion");
    }

    // ── 未服务方法的诊断（UNSERVED_METHODS 表）─────────────────────────
    //
    // 这张表的价值在于「同名不同因」：名字拼错 → NO_METHOD；名字对但网关没做
    // → unsupported + 指名能力。再加一道静态自检，防止有人把已经服务的能力
    // 又写回表里（那会让真能用的方法被误报为未实现）。

    #[test]
    fn unserved_methods_name_their_capability() {
        // 用一个仍在 UNSERVED_METHODS 里的方法（登录/协议/验证动作已实现，
        // 不能再用它们做「未服务」样例）。
        let error = unsupported_method("checkin.claim");
        assert_eq!(error.code.as_deref(), Some("unsupported"));
        assert!(
            error.message.contains("checkin.rolling.v1"),
            "必须指名是哪项能力未服务，实际: {}",
            error.message
        );
    }

    #[test]
    fn unknown_methods_stay_no_method() {
        let error = unsupported_method("nope.not_a_method");
        assert_eq!(error.code.as_deref(), Some(WireError::NO_METHOD));
    }

    #[test]
    fn unserved_table_never_claims_a_served_capability() {
        use crate::plugin_host::wasm::manifest;
        for &(method, capability) in UNSERVED_METHODS {
            // 「已随上游删除」的占位条目指名的是说明而不是能力，跳过两道能力断言。
            if capability == "已随上游删除" {
                continue;
            }
            assert!(
                manifest::CAPABILITIES.contains(&capability),
                "{method} 指向的能力 {capability} 必须仍在可声明全集里（否则应整条删除）"
            );
            assert!(
                !manifest::served(capability),
                "{method} 指向的能力 {capability} 已在已服务表里 —— 要么删掉这条，要么真的实现它"
            );
        }
    }

    #[test]
    fn unserved_table_has_no_duplicates() {
        let mut seen = std::collections::BTreeSet::new();
        for (method, _) in UNSERVED_METHODS {
            assert!(
                seen.insert(*method),
                "UNSERVED_METHODS 里重复登记: {method}"
            );
        }
    }
}
