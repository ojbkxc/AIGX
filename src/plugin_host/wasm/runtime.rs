// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 运行时 —— wasmtime 引擎、模块编译、宿主导入、单次调用的实例生命周期。
//!
//! ## ABI（与上游一致）
//!
//! 宿主 → 插件：
//! - 导入 `agent2api.call(ptr, len) -> i32`：插件调用宿主能力，宿主把结果
//!   暂存，返回应答字节数（负数表示宿主侧错误）
//! - 导入 `agent2api.read(ptr, capacity) -> i32`：插件把上一步的应答读进内存
//!
//! 插件 → 宿主：
//! - 导出 `a2a_abi_version() -> i32`：必须等于 [`super::manifest::ABI`]
//! - 导出 `a2a_alloc(len) -> i32`：分配输入缓冲区
//! - 导出 `a2a_call(ptr, len) -> i64`：处理请求，返回 `(len << 32) | ptr`
//! - 导出 `memory`
//!
//! ## 安全边界
//!
//! - 模块导入只允许上面两个函数；出现别的导入直接拒绝加载
//! - 每次调用新建 `Store`（内存上限来自清单）+ 实例，调用之间零共享
//! - 燃料 20 亿 + 每 10 万让出调度；超时/取消由外层 `select!` 兜底
//! - 消息 ≤16 MiB；Wasm 文件 ≤16 MiB 且必须带 `\0asm\x01\0\0\0` 魔数

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use wasmtime::{Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimitsBuilder};

use super::context::{Context, Shared, Sink};
use super::wire::WireError;

/// 1 MiB 的字节数（对齐上游 `capacity::MIB`）。
const MIB: usize = 1024 * 1024;

struct State {
    context: Context,
    limits: super::limits::Limits,
    input_bytes: usize,
    // 入站消息超过单次调用上限时在这里留下尺寸；不借用文案判断。
    input_oversize: Option<(usize, usize)>,
    reply: Vec<u8>,
}

pub(crate) struct Runtime {
    pub shared: Arc<Shared>,
    module: Module,
    linker: Linker<State>,
}

fn engine() -> Result<Engine, String> {
    static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let mut config = Config::new();
            config
                .async_support(true)
                .consume_fuel(true)
                .max_wasm_stack(512 * 1024);
            Engine::new(&config).map_err(|error| format!("Wasm 运行环境初始化失败：{error}"))
        })
        .clone()
}

fn memory(caller: &mut Caller<'_, State>) -> wasmtime::Result<Memory> {
    caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("Wasm 缺少内存导出"))
}

fn read(caller: &mut Caller<'_, State>, pointer: i32, length: i32) -> wasmtime::Result<Vec<u8>> {
    if pointer < 0 || length < 0 || length as usize > super::MAX_MESSAGE {
        return Err(wasmtime::Error::msg("Wasm 消息长度无效"));
    }
    let memory = memory(caller)?;
    let end = (pointer as usize)
        .checked_add(length as usize)
        .ok_or_else(|| wasmtime::Error::msg("Wasm 内存越界"))?;
    let data = memory
        .data(&*caller)
        .get(pointer as usize..end)
        .ok_or_else(|| wasmtime::Error::msg("Wasm 内存越界"))?;
    Ok(data.to_vec())
}

impl Runtime {
    /// 编译并链接一个 Wasm 模块（编译串行，全局 1 个槽）。
    pub async fn load(path: std::path::PathBuf, shared: Arc<Shared>) -> Result<Arc<Self>, String> {
        let engine = engine()?;
        let compiler = engine.clone();
        let permit = super::compile_slot()
            .acquire()
            .await
            .map_err(|_| "Wasm 编译队列已关闭")?;
        let module = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let metadata = std::fs::metadata(&path).map_err(|_| "Wasm 文件不可读".to_string())?;
            if metadata.len() > 16 * 1024 * 1024 {
                return Err("Wasm 文件超过 16 MiB 上限".into());
            }
            let bytes = std::fs::read(path).map_err(|_| "Wasm 文件不可读".to_string())?;
            if !bytes.starts_with(b"\0asm\x01\0\0\0") {
                return Err("插件入口不是 Wasm 模块".into());
            }
            Module::new(&compiler, bytes).map_err(|error| format!("Wasm 模块校验失败：{error}"))
        })
        .await
        .map_err(|_| "Wasm 编译任务中断".to_string())??;
        for import in module.imports() {
            if import.module() != "agent2api" || !matches!(import.name(), "call" | "read") {
                return Err("Wasm 模块导入了未授权的系统接口".into());
            }
        }
        let mut linker: Linker<State> = Linker::new(&engine);
        linker
            .func_wrap_async(
                "agent2api",
                "call",
                |mut caller: Caller<'_, State>, (ptr, len): (i32, i32)| {
                    Box::new(async move {
                        let bytes = read(&mut caller, ptr, len)?;
                        let input: Value = serde_json::from_slice(&bytes)
                            .map_err(|_| wasmtime::Error::msg("Wasm 主程序调用不是 JSON"))?;
                        let method = input["method"]
                            .as_str()
                            .ok_or_else(|| wasmtime::Error::msg("Wasm 调用缺少方法"))?;
                        let result = caller
                            .data_mut()
                            .context
                            .invoke(method, input["params"].clone())
                            .await;
                        let result = match result {
                            Ok(result) => json!({ "result": result }),
                            Err(error) => json!({ "error": error }),
                        };
                        let reply = serde_json::to_vec(&result)
                            .map_err(|_| wasmtime::Error::msg("Wasm 回应序列化失败"))?;
                        if reply.len() > super::MAX_MESSAGE {
                            return Err(wasmtime::Error::msg("Wasm 主程序回应过大"));
                        }
                        let length = reply.len() as i32;
                        caller.data_mut().reply = reply;
                        Ok(length)
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        linker
            .func_wrap(
                "agent2api",
                "read",
                |mut caller: Caller<'_, State>,
                 pointer: i32,
                 capacity: i32|
                 -> wasmtime::Result<i32> {
                    let length = caller.data().reply.len();
                    if pointer < 0 || capacity < 0 || (capacity as usize) < length {
                        return Err(wasmtime::Error::msg("Wasm 回应缓冲区不足"));
                    }
                    let memory = memory(&mut caller)?;
                    let reply = std::mem::take(&mut caller.data_mut().reply);
                    memory.write(&mut caller, pointer as usize, &reply)?;
                    Ok(length as i32)
                },
            )
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self {
            shared,
            module,
            linker,
        }))
    }

    /// 执行一次调用（独立 `Store` + 实例）。
    pub async fn invoke(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        sink: Option<Sink>,
    ) -> Result<Value, WireError> {
        let cancel = sink
            .as_ref()
            .map(|sink| sink.cancel.clone())
            .unwrap_or_else(|| self.shared.cancel.child());
        let streaming =
            sink.is_some() || matches!(method, "images.generate" | "images.edit" | "tasks.run");
        let work = async {
            let slots = if streaming {
                &self.shared.slots
            } else {
                &self.shared.control_slots
            };
            let _slot = slots
                .acquire()
                .await
                .map_err(|_| WireError::with_code("Wasm 插件已关闭", "plugin_unavailable"))?;
            let _global = super::global_slots(streaming)
                .acquire()
                .await
                .map_err(|_| WireError::with_code("Wasm 运行环境已关闭", "plugin_unavailable"))?;
            if method != super::methods::gateway::INIT
                && params["provider"].as_str() != Some(self.shared.manifest.provider.as_str())
            {
                return Err(WireError::with_code(
                    "调用不属于当前插件提供商",
                    "forbidden",
                ));
            }
            if matches!(method, "images.generate" | "images.edit")
                && !self.shared.manifest.allows("images.v1")
            {
                return Err(WireError::with_code("插件未声明标准图片能力", "forbidden"));
            }
            if method == "tasks.run" && !self.shared.manifest.allows("tasks.v1") {
                return Err(WireError::with_code("插件未声明后台任务能力", "forbidden"));
            }
            let mut context = Context::new(
                self.shared.clone(),
                &params,
                sink,
                tokio::time::Instant::now() + timeout,
            );
            if context.flow.len() > 256
                || context.session.len() > 1024
                || context.session.contains('\0')
            {
                return Err(WireError::with_code("登录或会话标识无效", "bad_params"));
            }
            if !context.account.is_empty() {
                context.invoke("account.get", json!({})).await?;
            }
            let mut params = params;
            if let Some(map) = params.as_object_mut() {
                for key in ["proxy", "proxy_override", "plugin_trace"] {
                    map.remove(key);
                }
                if let Some(payload) = map.get_mut("payload").and_then(Value::as_object_mut) {
                    payload.remove("proxy");
                }
                if let Some(payload) = map
                    .get_mut("source")
                    .and_then(|source| source.get_mut("payload"))
                    .and_then(Value::as_object_mut)
                {
                    payload.remove("proxy");
                }
                if let Some(headers) = map.get_mut("headers").and_then(Value::as_object_mut) {
                    headers.retain(|name, _| {
                        !matches!(
                            name.to_ascii_lowercase().as_str(),
                            "authorization"
                                | "x-api-key"
                                | "api-key"
                                | "cookie"
                                | "proxy-authorization"
                        )
                    });
                }
            }
            let limits = StoreLimitsBuilder::new()
                .memory_size(self.shared.manifest.memory_mib * 1024 * 1024)
                .instances(1)
                .memories(1)
                .tables(2)
                .table_elements(100_000)
                .trap_on_grow_failure(true)
                .build();
            let limits = super::limits::Limits {
                store: limits,
                exhausted: false,
            };
            let mut store = Store::new(
                self.module.engine(),
                State {
                    context,
                    limits,
                    input_bytes: 0,
                    input_oversize: None,
                    reply: Vec::new(),
                },
            );
            store.limiter(|state| &mut state.limits);
            store
                .set_fuel(2_000_000_000)
                .map_err(|_| super::wire::error("Wasm 计算配额配置失败"))?;
            store
                .fuel_async_yield_interval(Some(100_000))
                .map_err(|_| super::wire::error("Wasm 调度配置失败"))?;
            let result = self.execute(&mut store, method, params).await;
            if let Some((size, limit)) = store.data().input_oversize {
                // 消息在实例化前就放不下：明确告诉调用方尺寸，而不是折成笼统的资源错误。
                let hint = if method == super::methods::gateway::CONVERSATION {
                    "；请新建会话或减少图片后重试"
                } else {
                    ""
                };
                let message = format!(
                    "本次请求约 {:.1} MiB，超过插件单次调用上限 {:.0} MiB{hint}",
                    size as f64 / MIB as f64,
                    limit as f64 / MIB as f64
                );
                return Err(WireError::with_code(message, "plugin_request_too_large"));
            }
            match result {
                Ok(mut value) => {
                    if matches!(method, "images.generate" | "images.edit") {
                        super::resources::materialize(
                            &mut value,
                            &store.data().context.blobs,
                            &self.shared.manifest,
                        )?;
                    }
                    Ok(value)
                }
                Err(_) if store.data().limits.exhausted => {
                    // AIGX 的 Limits（beta.1 形状）不记录 table/host_memory 细分；
                    // 对话方法撞上内存上限时，真正原因是正文/图片太大。给出尺寸与
                    // 下一步，既便于用户处理，也避免被通用策略当成服务故障去换号重试。
                    let limit_mib = self.shared.manifest.memory_mib;
                    let input = store.data().input_bytes;
                    let (message, code) = if method == super::methods::gateway::CONVERSATION {
                        (
                            format!(
                                "本次对话上下文或图片过大：请求约 {:.1} MiB，插件内存上限 {limit_mib} MiB；请新建会话或减少图片后重试",
                                input as f64 / MIB as f64
                            ),
                            "plugin_request_too_large",
                        )
                    } else {
                        (
                            "Wasm 插件超过内存或表容量限制".to_string(),
                            "plugin_resource_limit",
                        )
                    };
                    Err(WireError::with_code(message, code))
                }
                Err(error) => Err(super::errors::interrupted(&error)),
            }
        };
        tokio::select! {
            biased;
            _ = self.shared.cancel.cancelled() => Err(WireError::with_code("Wasm 插件已关闭", "plugin_unavailable")),
            _ = cancel.cancelled() => Err(WireError::with_code("Wasm 调用已取消", WireError::ABORTED)),
            result = tokio::time::timeout(timeout, work) => result
                .map_err(|_| WireError::with_code("Wasm 插件调用超时", "timeout"))?,
        }
    }

    async fn execute(
        &self,
        store: &mut Store<State>,
        method: &str,
        params: Value,
    ) -> wasmtime::Result<Value> {
        let bytes = serde_json::to_vec(&json!({ "method": method, "params": params }))?;
        store.data_mut().input_bytes = bytes.len();
        if bytes.len() > super::MAX_MESSAGE {
            // 先记下真实尺寸，由 invoke() 统一折成可读的 413 文案。
            store.data_mut().input_oversize = Some((bytes.len(), super::MAX_MESSAGE));
            return Err(wasmtime::Error::msg("Wasm 输入消息过大"));
        }
        let instance = self
            .linker
            .instantiate_async(&mut *store, &self.module)
            .await?;
        let abi = instance.get_typed_func::<(), i32>(&mut *store, "a2a_abi_version")?;
        if abi.call_async(&mut *store, ()).await? != super::manifest::ABI as i32 {
            return Err(wasmtime::Error::msg("Wasm 导出接口版本不一致"));
        }
        let allocate = instance.get_typed_func::<i32, i32>(&mut *store, "a2a_alloc")?;
        let call = instance.get_typed_func::<(i32, i32), i64>(&mut *store, "a2a_call")?;
        let memory = instance
            .get_memory(&mut *store, "memory")
            .ok_or_else(|| wasmtime::Error::msg("Wasm 内存导出缺失"))?;
        let pointer = allocate.call_async(&mut *store, bytes.len() as i32).await?;
        if pointer < 0 {
            return Err(wasmtime::Error::msg("Wasm 分配地址无效"));
        }
        memory.write(&mut *store, pointer as usize, &bytes)?;
        let packed = call
            .call_async(&mut *store, (pointer, bytes.len() as i32))
            .await? as u64;
        let pointer = (packed & 0xffff_ffff) as usize;
        let length = (packed >> 32) as usize;
        if length > super::MAX_MESSAGE {
            return Err(wasmtime::Error::msg("Wasm 输出消息过大"));
        }
        let data = memory
            .data(&*store)
            .get(
                pointer
                    ..pointer
                        .checked_add(length)
                        .ok_or_else(|| wasmtime::Error::msg("Wasm 输出越界"))?,
            )
            .ok_or_else(|| wasmtime::Error::msg("Wasm 输出越界"))?;
        serde_json::from_slice(data).map_err(Into::into)
    }
}
