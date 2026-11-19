// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 单次调用内的二进制资源（图片上传 / 结果下载）。
//!
//! 与上游逐字一致：单次调用 ≤16 个资源、累计 ≤96 MiB、分片 ≤96 KiB、
//! 封存后不可再追加；MIME 白名单只收 png / jpeg / webp / octet-stream。

use std::collections::HashMap;

use base64::Engine;
use serde_json::{json, Value};

use super::wire::WireError;

const MAX_BYTES: usize = 96 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct Resources {
    entries: HashMap<String, Resource>,
    bytes: usize,
}

struct Resource {
    mime: String,
    bytes: Vec<u8>,
    sealed: bool,
}

/// 资源 id 生成（不依赖宿主 `random_id`，用进程内计数器 + 随机前缀）。
fn next_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let mut nonce = [0u8; 6];
    let _ = getrandom::getrandom(&mut nonce);
    format!(
        "blob_{}{}",
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

impl Resources {
    pub fn create(&mut self, mime: &str) -> Result<Value, WireError> {
        if self.entries.len() >= 16
            || !matches!(
                mime,
                "image/png" | "image/jpeg" | "image/webp" | "application/octet-stream"
            )
        {
            return Err(super::wire::error("资源数量或类型无效"));
        }
        let id = next_id();
        self.entries.insert(
            id.clone(),
            Resource {
                mime: mime.into(),
                bytes: Vec::new(),
                sealed: false,
            },
        );
        Ok(json!({ "id": id }))
    }

    pub fn append(&mut self, id: &str, data: &str) -> Result<Value, WireError> {
        if data.len() > 96 * 1024 {
            return Err(super::wire::error("资源分片过大"));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| super::wire::error("资源分片编码无效"))?;
        if self.bytes.saturating_add(bytes.len()) > MAX_BYTES {
            return Err(WireError::with_code(
                "单次调用资源总量超过 96 MiB",
                "plugin_resource_limit",
            ));
        }
        let resource = self
            .entries
            .get_mut(id)
            .ok_or_else(|| super::wire::error("资源不属于当前调用"))?;
        if resource.sealed {
            return Err(super::wire::error("资源已封存"));
        }
        self.bytes += bytes.len();
        resource.bytes.extend_from_slice(&bytes);
        Ok(json!({ "size": resource.bytes.len() }))
    }

    pub fn seal(&mut self, id: &str) -> Result<Value, WireError> {
        let resource = self
            .entries
            .get_mut(id)
            .ok_or_else(|| super::wire::error("资源不属于当前调用"))?;
        if resource.bytes.is_empty() {
            return Err(super::wire::error("不能封存空资源"));
        }
        resource.sealed = true;
        Ok(json!({ "id": id, "size": resource.bytes.len(), "mime": resource.mime }))
    }

    pub fn read(&self, id: &str, offset: usize, length: usize) -> Result<Value, WireError> {
        let resource = self
            .entries
            .get(id)
            .ok_or_else(|| super::wire::error("资源不属于当前调用"))?;
        if offset > resource.bytes.len() || length == 0 || length > 64 * 1024 {
            return Err(super::wire::error("资源读取范围无效"));
        }
        let end = offset.saturating_add(length).min(resource.bytes.len());
        Ok(json!({
            "data": base64::engine::general_purpose::STANDARD.encode(&resource.bytes[offset..end]),
            "done": end == resource.bytes.len(),
            "size": resource.bytes.len()
        }))
    }

    pub fn remove(&mut self, id: &str) {
        if let Some(resource) = self.entries.remove(id) {
            self.bytes = self.bytes.saturating_sub(resource.bytes.len());
        }
    }

    pub fn raw(&self, id: &str) -> Result<Vec<u8>, WireError> {
        let resource = self
            .entries
            .get(id)
            .filter(|resource| resource.sealed)
            .ok_or_else(|| super::wire::error("资源未完成或不属于当前调用"))?;
        Ok(resource.bytes.clone())
    }

    pub fn image(&self, id: &str) -> Result<String, WireError> {
        let resource = self
            .entries
            .get(id)
            .filter(|resource| resource.sealed && resource.mime.starts_with("image/"))
            .ok_or_else(|| super::wire::error("图片资源未完成或不属于当前调用"))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(&resource.bytes))
    }
}

/// 把图片结果里的 `resource` 引用替换成内联 base64，并校验 URL 授权。
pub(crate) fn materialize(
    value: &mut Value,
    resources: &Resources,
    options: &super::manifest::WasmManifest,
) -> Result<(), WireError> {
    let Some(result) = value.get_mut("result") else {
        return Ok(());
    };
    let data = result
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .filter(|data| !data.is_empty() && data.len() <= 16)
        .ok_or_else(super::errors::protocol)?;
    let mut used = std::collections::HashSet::new();
    let mut total = 0usize;
    for image in data {
        let map = image.as_object_mut().ok_or_else(super::errors::protocol)?;
        let count = ["resource", "b64_json", "url"]
            .iter()
            .filter(|key| map.contains_key(**key))
            .count();
        if count != 1 {
            return Err(super::errors::protocol());
        }
        if let Some(id) = map.remove("resource") {
            let id = id.as_str().ok_or_else(super::errors::protocol)?;
            if !used.insert(id.to_string()) {
                return Err(super::errors::protocol());
            }
            let data = resources.image(id)?;
            total = total.saturating_add(data.len());
            if total > MAX_BYTES.div_ceil(3) * 4 {
                return Err(super::wire::error("图片结果总量超过限制"));
            }
            map.insert("b64_json".into(), json!(data));
        } else if let Some(url) = map.get("url") {
            options.check_url(url.as_str().ok_or_else(super::errors::protocol)?, false)?;
        } else if !map
            .get("b64_json")
            .and_then(Value::as_str)
            .is_some_and(|data| {
                !data.is_empty()
                    && base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .is_ok()
            })
        {
            return Err(super::errors::protocol());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn create_append_seal_read_roundtrip() {
        let mut resources = Resources::default();
        let id = resources.create("image/png").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let payload = base64::engine::general_purpose::STANDARD.encode(b"hello");
        resources.append(&id, &payload).unwrap();
        let sealed = resources.seal(&id).unwrap();
        assert_eq!(sealed["size"], 5);
        let read = resources.read(&id, 0, 64).unwrap();
        assert_eq!(read["done"], true);
        assert_eq!(resources.raw(&id).unwrap(), b"hello");
    }

    #[test]
    fn rejects_unknown_mime_and_empty_seal() {
        let mut resources = Resources::default();
        assert!(resources.create("text/html").is_err());
        let id = resources.create("image/png").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(resources.seal(&id).is_err());
    }

    #[test]
    fn rejects_append_after_seal() {
        let mut resources = Resources::default();
        let id = resources.create("image/png").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let payload = base64::engine::general_purpose::STANDARD.encode(b"x");
        resources.append(&id, &payload).unwrap();
        resources.seal(&id).unwrap();
        assert!(resources.append(&id, &payload).is_err());
    }

    #[test]
    fn materialize_requires_exactly_one_source() {
        let resources = Resources::default();
        let manifest = super::super::manifest::WasmManifest {
            provider: "p".into(),
            abi: 1,
            requires: vec![],
            origins: vec![],
            helpers: vec![],
            memory_mib: 64,
            concurrency: 8,
            verified: false,
            account_origin: None,
            desktop_launch: None,
            managed_login: None,
            managed_task: None,
            concurrency_hard_limit: None,
        };
        let mut bad = json!({"result": {"data": [{"resource": "x", "url": "https://a"}]}});
        assert!(materialize(&mut bad, &resources, &manifest).is_err());
        let mut empty = json!({"result": {"data": []}});
        assert!(materialize(&mut empty, &resources, &manifest).is_err());
        let mut ok = json!({"result": {"data": [{"b64_json": "aGk="}]}});
        assert!(materialize(&mut ok, &resources, &manifest).is_ok());
    }
}
