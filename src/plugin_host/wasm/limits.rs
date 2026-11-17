// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 实例的资源上限 —— 内存 / 表增长全部经 `ResourceLimiter` 记账。
//!
//! 与上游逐字一致：一次拒绝（`Ok(false)` / `Err`）就把 `exhausted` 置位，
//! 调用方据此把 trap 归类成 `plugin_resource_limit` 而不是 `plugin_trap`。

use wasmtime::{ResourceLimiter, StoreLimits};

pub(crate) struct Limits {
    pub store: StoreLimits,
    pub exhausted: bool,
}

impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let result = self.store.memory_growing(current, desired, maximum);
        if !matches!(result, Ok(true)) {
            self.exhausted = true;
        }
        result
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let result = self.store.table_growing(current, desired, maximum);
        if !matches!(result, Ok(true)) {
            self.exhausted = true;
        }
        result
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.exhausted = true;
        self.store.memory_grow_failed(error)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.exhausted = true;
        self.store.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.store.instances()
    }

    fn tables(&self) -> usize {
        self.store.tables()
    }

    fn memories(&self) -> usize {
        self.store.memories()
    }
}
