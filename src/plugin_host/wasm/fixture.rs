// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 测试夹具：手写最小可装载的 Wasm 模块。
//!
//! ## 为什么手写字节
//!
//! 本仓不依赖 `wat`（Cargo.toml 的 wasmtime 只开了
//! `async/cranelift/runtime/std`），而「沙箱能装载」这条如果没有真跑过的
//! 证据，就只是「应该可以」。这里用最小编码器在测试期生成合法二进制：
//!
//! - **零导入**（`runtime::load` 只拒绝**非白名单**导入，零导入合法）
//! - 导出 `memory`（2 页）、`a2a_abi_version`、`a2a_alloc`、`a2a_call`
//! - `a2a_call` **对任何方法**都返回同一段预置 JSON：
//!   `{"result":{"providers":[{"id":"<provider>","label":"<label>"}]}}`
//!
//! 因此它只能验证 **init 握手 + 装载/卸载链路**；`conversation` 的真实
//! SSE、`tasks.list` 的任务数组都**无法**用它模拟（那需要在 wasm 里解析
//! 方法名字符串，超出「最小夹具」的范围）。
//!
//! 需要**真实流式**的用例（数据面 `conversation` 分支）用
//! [`streaming_wasm_module`]：它真导入宿主 `agent2api.call`，按请求 JSON
//! 的 method 首字符分派 init / conversation，并在 conversation 分支里
//! 主动下发 `stream.head` + 每段 `stream.chunk`（零 mock、真跑 wasmtime）。

/// 生成一个最小可装载的 Wasm 模块。
///
/// `provider` / `label` 会写进 `init` 应答，必须与包清单的 provider id 一致
/// （`host::start` 会校验「提供商身份与安装清单不一致」）。
pub(crate) fn minimal_wasm_module(provider: &str, label: &str) -> Vec<u8> {
    let response = serde_json::json!({
        "result": { "providers": [{ "id": provider, "label": label }] }
    })
    .to_string();
    module_with_response(&response)
}

/// 同 [`minimal_wasm_module`]，但任意指定 `a2a_call` 的应答 JSON。
///
/// 用于验证**错误传播**：传 `{"error":{"message":"...","code":"..."}}` 时，
/// 宿主会把该应答解码成 `WireError`，沙箱据此走 `sink.fail` →
/// 消费方 `take_error()` 拿到同一个错误。
pub(crate) fn module_with_response(response: &str) -> Vec<u8> {
    module_with_segments(&[response], |_| {
        let data = response.as_bytes();
        let mut body = vec![0x42];
        body.extend_from_slice(&leb_i64(packed(data.len(), 0)));
        body.push(0x0b);
        body
    })
}

/// 按**请求长度**分派两条应答的最小模块。
///
/// 夹具不会解析方法名（那需要在 wasm 里做字符串比较，超出「最小夹具」），
/// 但「init 握手」与「某次具体调用」的请求字节长度在实践中互不相同 ——
/// 测试方用与宿主**同一套** `serde_json::to_vec` 算出期望长度，即可让
/// `a2a_call` 对这条请求返回另一段应答。
///
/// - 请求字节长度 `== expected_len` → 返回 `matched`（落在数据段偏移 0）
/// - 其余任何请求 → 返回 `fallback`（落在偏移 2048，通常是 `init` 应答）
///
/// 两条数据段都在 4 KiB 以下，避开 `a2a_alloc` 返回的输入缓冲区（偏移
/// 4096），因此请求写入不会覆盖应答。
///
/// **当前无调用方**：探针 5 需要三路分派，用的是下面的
/// [`module_with_two_len_dispatch`]。本函数保留是因为它是两路场景的最小
/// 形态，将来加「只验一条方法」的探针时直接可用 —— 不是接线遗漏。
#[allow(dead_code)]
pub(crate) fn module_with_len_dispatch(
    expected_len: usize,
    matched: &str,
    fallback: &str,
) -> Vec<u8> {
    assert!(
        matched.len() < 2048 && fallback.len() < 2048,
        "应答必须落在 4 KiB 输入缓冲区之前"
    );
    module_with_segments(&[matched, fallback], |offsets| {
        // (ptr, len) -> i64：len == expected_len ? matched : fallback
        let mut body = vec![0x20, 0x01, 0x41]; // local.get 1; i32.const
        body.extend_from_slice(&leb_i64(expected_len as i64));
        body.extend_from_slice(&[0x46, 0x04, 0x7e, 0x42]); // i32.eq; if (result i64); i64.const
        body.extend_from_slice(&leb_i64(packed(matched.len(), offsets[0])));
        body.push(0x05); // else
        body.push(0x42);
        body.extend_from_slice(&leb_i64(packed(fallback.len(), offsets[1])));
        body.extend_from_slice(&[0x0b, 0x0b]); // end(if) + end(function)
        body
    })
}

/// 按**请求长度**分派三条应答的最小模块（探针 5：init / `tasks.list` /
/// `tasks.run` 共用一个宿主时必须三路分开）。
///
/// - `len == first_len`  → `first`
/// - `len == second_len` → `second`
/// - 其余（含 init）      → `fallback`
///
/// 三段应答依次落在数据段偏移 0 / 2048 / 4096+20736 —— 注意第三个偏移必须
/// 跳开 `a2a_alloc` 的输入缓冲区（4096）。因此这里把**第三段放到内存末页**
/// 附近：内存 2 页 = 131072 字节，放偏移 65536 仍远小于上限，且与输入
/// 缓冲区（4096 起、请求 ≤16 MiB 但夹具请求只有几百字节）不重叠。
pub(crate) fn module_with_two_len_dispatch(
    first_len: usize,
    first: &str,
    second_len: usize,
    second: &str,
    fallback: &str,
) -> Vec<u8> {
    const TAIL: usize = 65_536;
    assert!(
        first.len() < 2048 && second.len() < 2048 && fallback.len() < 2048,
        "应答长度必须受限，便于落在预分配偏移"
    );
    assert!(TAIL + second.len() < 131_072, "第三段必须在 2 页内存内");
    module_with_segments_at(&[0, 2048, TAIL], &[first, second, fallback], |offsets| {
        // local.get 1 (len)；== first_len ? first : (== second_len ? second : fallback)
        let mut body = vec![0x20, 0x01, 0x41];
        body.extend_from_slice(&leb_i64(first_len as i64));
        body.extend_from_slice(&[0x46, 0x04, 0x7e, 0x42]); // i32.eq; if (result i64); i64.const
        body.extend_from_slice(&leb_i64(packed(first.len(), offsets[0])));
        body.push(0x05); // else
        body.extend_from_slice(&[0x20, 0x01, 0x41]); // local.get 1; i32.const
        body.extend_from_slice(&leb_i64(second_len as i64));
        body.extend_from_slice(&[0x46, 0x04, 0x7e, 0x42]); // i32.eq; if (result i64); i64.const
        body.extend_from_slice(&leb_i64(packed(second.len(), offsets[1])));
        body.push(0x05); // else
        body.push(0x42);
        body.extend_from_slice(&leb_i64(packed(fallback.len(), offsets[2])));
        body.extend_from_slice(&[0x0b, 0x0b, 0x0b]); // end(if) * 2 + end(function)
        body
    })
}

/// **流式对话夹具** —— 真导入宿主 `agent2api.call`，按请求 JSON 的 method
/// 首字符分派：
///
/// - `i`（init）→ 返回 providers 应答（含 `capabilities.chat = true`）
/// - `c`（conversation）→ 依次经宿主导入调用 `stream.head`、每段
///   `stream.chunk`，最后返回 `{"result":null}`；宿主据此正常收尾并给
///   消费方下发 `StreamEvent::End`
///
/// ## 为什么可以只比首字符
///
/// 宿主把请求编码成 `serde_json::to_vec(&json!({"method": ..., "params": ...}))`；
/// serde_json 的 Map 按键排序（`method` < `params`），请求固定以
/// `{"method":"` 开头（12 字节），偏移 11 就是 method 值的首字符。夹具只
/// 需要区分 init / conversation 两条路（它不声明 `tasks.v1`，启用期不会
/// 出现第三种调用），因此 `i32.load8_u(ptr + 11)` 与 `'c'` 比较即分派。
///
/// 本函数产出的模块会真的导出 `a2a_call` 并调用导入的 `agent2api.call`
/// —— 这正是「零 mock、真跑 wasmtime」证据链的一环：host 的
/// `stream.head` / `stream.chunk` 由 wasm 主动发起，而不是宿主替它发。
///
/// `chunks` 是**已解码正文**（通常是一段 OpenAI SSE 字节）；编码器负责
/// base64 后写进 `stream.chunk` 的参数。三段以上应答的落点依次为
/// 8192 + n*2048，全部避开 `a2a_alloc` 的输入缓冲区（4096）与 init 回包
/// 区（0/2048）。
pub(crate) fn streaming_wasm_module(provider: &str, label: &str, chunks: &[&[u8]]) -> Vec<u8> {
    use base64::Engine as _;

    let init = serde_json::json!({
        "result": {
            "providers": [{
                "id": provider,
                "label": label,
                "capabilities": { "chat": true }
            }]
        }
    })
    .to_string();
    // 宿主 `open_stream` 的 Ok 分支要求 `{"result": ...}` 信封（errors::decode）
    let done = r#"{"result":null}"#.to_string();
    // 宿主 `stream.head` 的参数形状（headers 是 [[name, value]]）
    let head = r#"{"method":"stream.head","params":{"status":200,"headers":[["content-type","text/event-stream"]]}}"#.to_string();
    let mut texts: Vec<String> = vec![init, done, head];
    for chunk in chunks {
        let data = base64::engine::general_purpose::STANDARD.encode(chunk);
        texts.push(
            serde_json::json!({ "method": "stream.chunk", "params": { "data": data } }).to_string(),
        );
    }
    let offsets: Vec<usize> = (0..texts.len())
        .map(|index| match index {
            0 => 0,
            1 => 2048,
            _ => 8192 + (index - 2) * 2048,
        })
        .collect();
    for (offset, text) in offsets.iter().zip(&texts) {
        assert!(text.len() < 2048, "夹具应答必须落在预分配的 2 KiB 区间内");
        assert!(
            offset + text.len() <= 131_072,
            "夹具应答 {offset}+{} 超出 2 页内存",
            text.len()
        );
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"\0asm\x01\0\0\0");

    // ── Type 段：4 个签名（第 4 个给宿主导入 call）──
    let mut types = leb_u32(4);
    types.extend_from_slice(&[0x60, 0x00, 0x01, 0x7f]); // () -> i32
    types.extend_from_slice(&[0x60, 0x01, 0x7f, 0x01, 0x7f]); // (i32) -> i32
    types.extend_from_slice(&[0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7e]); // (i32,i32) -> i64
    types.extend_from_slice(&[0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f]); // (i32,i32) -> i32
    out.extend(section(1, types));

    // ── Import 段：agent2api.call（函数索引 0）──
    let mut imports = leb_u32(1);
    imports.extend(name("agent2api"));
    imports.extend(name("call"));
    imports.push(0x00); // 函数导入
    imports.extend(leb_u32(3)); // (i32,i32) -> i32
    out.extend(section(2, imports));

    // ── Function 段：3 个本模块函数（索引 1..=3）──
    let mut funcs = leb_u32(3);
    funcs.extend(leb_u32(0));
    funcs.extend(leb_u32(1));
    funcs.extend(leb_u32(2));
    out.extend(section(3, funcs));

    // ── Memory 段：1 个内存，2 页 ──
    let mut memory = leb_u32(1);
    memory.extend_from_slice(&[0x00, 0x02]);
    out.extend(section(5, memory));

    // ── Export 段（导入函数占索引 0，故本模块函数从 1 起）──
    let mut exports = leb_u32(4);
    for (field, kind, index) in [
        ("memory", 0x02u8, 0u32),
        ("a2a_abi_version", 0x00, 1),
        ("a2a_alloc", 0x00, 2),
        ("a2a_call", 0x00, 3),
    ] {
        exports.extend(name(field));
        exports.push(kind);
        exports.extend(leb_u32(index));
    }
    out.extend(section(7, exports));

    // ── Code 段 ──
    let mut call_body: Vec<u8> = Vec::new();
    // load8_u(ptr + 11) == 'c'
    call_body.extend_from_slice(&[0x20, 0x00]); // local.get 0
    call_body.extend_from_slice(&[0x41, 0x0b]); // i32.const 11
    call_body.push(0x6a); // i32.add
    call_body.extend_from_slice(&[0x2d, 0x00, 0x00]); // i32.load8_u align=1 offset=0
    call_body.push(0x41); // i32.const 'c'（有符号 LEB：0x63 需补 0x00，否则解成 -29）
    call_body.extend_from_slice(&leb_i64(b'c' as i64));
    call_body.push(0x46); // i32.eq
    call_body.extend_from_slice(&[0x04, 0x7e]); // if (result i64)；conversation 逐段调宿主导入
    for (index, text) in texts.iter().enumerate().skip(2) {
        call_body.push(0x41); // i32.const <payload ptr>
        call_body.extend_from_slice(&leb_i64(offsets[index] as i64));
        call_body.push(0x41); // i32.const <payload len>
        call_body.extend_from_slice(&leb_i64(text.len() as i64));
        call_body.extend_from_slice(&[0x10, 0x00]); // call 0（agent2api.call）
        call_body.push(0x1a); // drop（reply 留在宿主，本夹具不读回）
    }
    call_body.push(0x42); // i64.const packed(done)
    call_body.extend_from_slice(&leb_i64(packed(texts[1].len(), offsets[1])));
    call_body.push(0x05); // else
    call_body.push(0x42); // i64.const packed(init)
    call_body.extend_from_slice(&leb_i64(packed(texts[0].len(), offsets[0])));
    call_body.extend_from_slice(&[0x0b, 0x0b]); // end(if) + end(function)

    let mut code = leb_u32(3);
    code.extend(body(&[0x41, 0x01, 0x0b])); // ABI = 1
    code.extend(body(&[0x41, 0x80, 0x20, 0x0b])); // 4096（输入缓冲区）
    code.extend(body(&call_body));
    out.extend(section(10, code));

    // ── Data 段 ──
    let mut data_section = leb_u32(texts.len() as u32);
    for (index, text) in texts.iter().enumerate() {
        data_section.extend(leb_u32(0)); // 活动段，内存 0
        data_section.push(0x41); // i32.const offset
        data_section.extend(leb_i64(offsets[index] as i64));
        data_section.push(0x0b);
        data_section.extend(leb_u32(text.len() as u32));
        data_section.extend_from_slice(text.as_bytes());
    }
    out.extend(section(11, data_section));

    out
}

/// `(len << 32) | ptr` —— 宿主按此解包应答地址。
fn packed(len: usize, ptr: usize) -> i64 {
    ((len as i64) << 32) | ptr as i64
}

/// 共用骨架：Type/Function/Memory/Export 段 + 调用方给定的 `a2a_call` 体。
///
/// `responses` 依次落在数据段偏移 0 / 2048 / 4096+（测试里最多三段）。
/// 第四条起会撞上输入缓冲区，故断言上限。
fn module_with_segments(
    responses: &[&str],
    call_body: impl FnOnce(&[usize]) -> Vec<u8>,
) -> Vec<u8> {
    let offsets: Vec<usize> = (0..responses.len()).map(|i| i * 2048).collect();
    module_with_segments_at(&offsets, responses, call_body)
}

/// 同 [`module_with_segments`]，但**显式给出**每段应答的落点偏移。
fn module_with_segments_at(
    offsets: &[usize],
    responses: &[&str],
    call_body: impl FnOnce(&[usize]) -> Vec<u8>,
) -> Vec<u8> {
    assert!(responses.len() <= 3, "最多三段应答");
    assert_eq!(offsets.len(), responses.len(), "偏移与应答必须一一对应");
    for (offset, response) in offsets.iter().zip(responses) {
        assert!(
            offset + response.len() <= 131_072,
            "应答 {offset}+{} 超出 2 页内存",
            response.len()
        );
    }

    let mut out = Vec::new();
    // magic + version
    out.extend_from_slice(b"\0asm\x01\0\0\0");

    // ── Type 段：3 个签名 ──
    let mut types = leb_u32(3);
    types.extend_from_slice(&[0x60, 0x00, 0x01, 0x7f]); // () -> i32
    types.extend_from_slice(&[0x60, 0x01, 0x7f, 0x01, 0x7f]); // (i32) -> i32
    types.extend_from_slice(&[0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7e]); // (i32,i32) -> i64
    out.extend(section(1, types));

    // ── Function 段：三个函数分别用 type 0/1/2 ──
    let mut funcs = leb_u32(3);
    funcs.extend(leb_u32(0));
    funcs.extend(leb_u32(1));
    funcs.extend(leb_u32(2));
    out.extend(section(3, funcs));

    // ── Memory 段：1 个内存，最小 2 页（128 KiB） ──
    let mut memory = leb_u32(1);
    memory.extend_from_slice(&[0x00, 0x02]);
    out.extend(section(5, memory));

    // ── Export 段 ──
    let mut exports = leb_u32(4);
    for (field, kind, index) in [
        ("memory", 0x02u8, 0u32),
        ("a2a_abi_version", 0x00, 0),
        ("a2a_alloc", 0x00, 1),
        ("a2a_call", 0x00, 2),
    ] {
        exports.extend(name(field));
        exports.push(kind);
        exports.extend(leb_u32(index));
    }
    out.extend(section(7, exports));

    // ── Code 段 ──
    let mut code = leb_u32(3);
    // f0: () -> i32 { i32.const ABI }
    code.extend(body(&[0x41, 0x01, 0x0b]));
    // f1: (i32) -> i32 { i32.const 4096 }  —— 输入缓冲区地址
    code.extend(body(&[0x41, 0x80, 0x20, 0x0b]));
    // f2: (i32,i32) -> i64 { 调用方给定 }
    code.extend(body(&call_body(offsets)));
    out.extend(section(10, code));

    // ── Data 段：每条应答一个活动段 ──
    let mut data_section = leb_u32(responses.len() as u32);
    for (index, response) in responses.iter().enumerate() {
        data_section.extend(leb_u32(0)); // 活动段，内存 0
        data_section.push(0x41); // i32.const offset
        data_section.extend(leb_i64(offsets[index] as i64));
        data_section.push(0x0b);
        data_section.extend(leb_u32(response.len() as u32));
        data_section.extend_from_slice(response.as_bytes());
    }
    out.extend(section(11, data_section));

    out
}

/// 一个 section：`id` + 载荷长度 + 载荷。
fn section(id: u8, payload: Vec<u8>) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(leb_u32(payload.len() as u32));
    out.extend(payload);
    out
}

/// 一个函数体：局部变量数（0）+ 指令 + `end`，前置长度。
fn body(instrs: &[u8]) -> Vec<u8> {
    let mut payload = leb_u32(0); // 无局部变量
    payload.extend_from_slice(instrs);
    let mut out = leb_u32(payload.len() as u32);
    out.extend(payload);
    out
}

/// 名字（UTF-8 长度前缀）。
fn name(raw: &str) -> Vec<u8> {
    let mut out = leb_u32(raw.len() as u32);
    out.extend_from_slice(raw.as_bytes());
    out
}

/// 无符号 LEB128。
fn leb_u32(mut n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut byte = (n & 0x7f) as u8;
        n >>= 7;
        if n != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
    out
}

/// 有符号 LEB128（`i64.const` 用）。
fn leb_i64(mut n: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        let done = (n == 0 && byte & 0x40 == 0) || (n == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// e2e 夹具制品常量 —— **必须与 `tests/wasm_data_plane_e2e.rs` 逐字一致**。
    pub(crate) const E2E_PROVIDER: &str = "wasm-e2e-provider";
    pub(crate) const E2E_LABEL: &str = "Wasm E2E";
    pub(crate) const E2E_CHUNK_1: &[u8] = b"data: {\"id\":\"chatcmpl-wasm-e2e\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"wasm-e2e-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n\n";
    pub(crate) const E2E_CHUNK_2: &[u8] = b"data: {\"id\":\"chatcmpl-wasm-e2e\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"wasm-e2e-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\",\"finish_reason\":\"stop\"}}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\ndata: [DONE]\n\n";

    /// 结构自证：流式夹具必须真的**导入**宿主的 `agent2api.call` 且内嵌
    /// head / chunk 载荷（否则「真跑流式」就退化成宿主替插件发事件）。
    #[test]
    fn streaming_module_imports_host_call() {
        let bytes = streaming_wasm_module(E2E_PROVIDER, E2E_LABEL, &[E2E_CHUNK_1, E2E_CHUNK_2]);
        assert!(bytes.starts_with(b"\0asm\x01\0\0\0"));
        // 导入段：模块名与字段名都以长度前缀 + UTF-8 出现
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("agent2api") && text.contains("call"));
        assert!(text.contains("stream.head"));
        assert!(text.contains("stream.chunk"));
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(E2E_CHUNK_1);
        assert!(
            text.contains(&encoded),
            "chunk 载荷必须 base64 后写进数据段"
        );
    }

    /// **e2e 制品一致性**：`tests/e2e/fixtures/stream_plugin.wasm.b64` 是
    /// `tests/wasm_data_plane_e2e.rs` 装包用的模块字节。改夹具 = 必须同步
    /// 重生成该制品，否则这里立即红（防「夹具与 e2e 制品漂移」）。
    #[test]
    fn streaming_module_matches_checked_in_e2e_artifact() {
        use base64::Engine as _;
        let bytes = streaming_wasm_module(E2E_PROVIDER, E2E_LABEL, &[E2E_CHUNK_1, E2E_CHUNK_2]);
        let expected = include_str!("../../../tests/e2e/fixtures/stream_plugin.wasm.b64");
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(bytes),
            expected.trim(),
            "夹具变更必须同步 tests/e2e/fixtures/stream_plugin.wasm.b64"
        );
    }

    #[test]
    fn encodes_leb128() {
        assert_eq!(leb_u32(0), vec![0x00]);
        assert_eq!(leb_u32(127), vec![0x7f]);
        assert_eq!(leb_u32(128), vec![0x80, 0x01]);
        assert_eq!(leb_u32(4096), vec![0x80, 0x20]);
        // 有符号：小的正数末字节符号位必须为 0
        assert_eq!(leb_i64(1), vec![0x01]);
        assert_eq!(leb_i64(64), vec![0xc0, 0x00]);
        assert_eq!(leb_i64(-1), vec![0x7f]);
    }

    #[test]
    fn module_has_magic_and_sections() {
        let bytes = minimal_wasm_module("demo", "Demo");
        assert!(bytes.starts_with(b"\0asm\x01\0\0\0"));
        assert!(bytes.len() > 64, "{} bytes", bytes.len());
        // 数据段里应含 provider id
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("demo"));
    }
}
