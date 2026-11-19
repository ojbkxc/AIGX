//! 入口源码的 auth.provider 静态扫描（provider.rs）。
//!
//! ## 为什么静态扫描而不是执行代码
//!
//! GitHub 发现的第三方仓没有策展背书，**执行它的代码前必须有人做安装
//! 信任决策**（operations 强制 trusted=true）——目录阶段绝不运行任何
//! 下载来的 JS。但「这个仓真的导出 auth.provider 吗」决定它能不能进
//! 第三方列表。折衷：只读源码文本 + 手写轻量 tokenizer 找字面形态。
//!
//! ## 识别的字面形态（OpenCode v1 auth.provider 惯例）
//!
//! ```js
//! export const MyPlugin = {
//!   auth: {
//!     provider: "my-provider",          // ← 目标
//!     methods: [ ... ],                  // 或
//!     loader: async () => ({ ... }),     // 这两键的存在只用于收紧匹配
//!   },
//! }
//! ```
//!
//! tokenizer 逐词扫 `auth` → `{` → 在该对象字面量内找
//! `provider` → `:` → 字符串字面量；`methods:` / `loader:` 键的存在
//! 作为佐证（降低误报），没有也不否决（ESM 解构、注释生成的变体多）。
//! 不做完整 JS 解析：字符串/正则/模板串内的代码要跳过（tokenizer 处理
//! 这三类字面量），块注释行注释跳过。
//!
//! ## 读取边界（防把目录阶段变成无界爬虫）
//!
//! 从入口文件 BFS 最多 **16 个文件**（相对导入链），累计源码 ≤ **6MB**；
//! 只跟随 `./` `../` 相对导入，包名导入（`node:fs`、`npm-pkg`）不解析。
//! 到达边界即停——没扫到就算没验证（provider_verified=None/false）。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 单个文件源码上限（256KB：真实插件入口远小于此，超限按无效文件跳过）。
const MAX_FILE_BYTES: usize = 256 * 1024;
/// BFS 文件数上限。
pub const MAX_SCAN_FILES: usize = 16;
/// 累计源码字节上限。
pub const MAX_TOTAL_BYTES: usize = 6 * 1024 * 1024;

/// 一次扫描的结果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanOutcome {
    /// 找到的 auth.provider id（一个入口声明多家时取第一个）。
    pub provider_id: Option<String>,
    /// 随 provider 一起找到的展示名（`auth.label` / `auth.name`，可空）。
    pub label: Option<String>,
    /// 实际读过的文件数（诊断用）。
    pub files_read: usize,
}

/// 词法单元（手写 tokenizer 的产出）。
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// 标识符/关键字原样。
    Word(String),
    /// 标点（只关心 { } : , ( ) [ ]）。
    Punct(char),
    /// 字符串字面量的**内容**（'..' ".." / 模板串忽略表达式，取裸文本）。
    Str(String),
    /// 其他不相关 token（数字等）。
    Other,
}

/// 手写 tokenizer：跳过注释与字符串转义，产出线性 token 流。
///
/// 模板串（`` `..` ``）整体当一个字符串收（内嵌表达式里的代码不参与
/// 匹配——宁可漏报不可误报）。正则字面量按除法歧义处理：仅在 `(` `,`
/// `=` `:` `return` 等词后出现的 `/` 才当正则起点。
fn tokenize(src: &str) -> Vec<Tok> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let n = b.len();
    // 上一个有意义的 token，用于正则/除法歧义
    let mut prev_is_value = false;
    while i < n {
        let c = b[i];
        // 注释
        if c == '/' && i + 1 < n && b[i + 1] == '/' {
            while i < n && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && b[i + 1] == '*' {
            i += 2;
            while i + 1 < n && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        // 字符串
        if c == '"' || c == '\'' {
            let (s, ni) = read_string(&b, i, c);
            out.push(Tok::Str(s));
            i = ni;
            prev_is_value = true;
            continue;
        }
        // 模板串
        if c == '`' {
            let (s, ni) = read_template(&b, i);
            out.push(Tok::Str(s));
            i = ni;
            prev_is_value = true;
            continue;
        }
        // 正则字面量（/ 出现在非值位置时）
        if c == '/' && !prev_is_value {
            let mut j = i + 1;
            let mut in_class = false;
            let mut found = false;
            while j < n {
                match b[j] {
                    '\\' => j += 1,
                    '[' => in_class = true,
                    ']' => in_class = false,
                    '/' if !in_class => {
                        found = true;
                        break;
                    }
                    '\n' => break,
                    _ => {}
                }
                j += 1;
            }
            if found {
                i = j + 1;
                // 跳 flags
                while i < n && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push(Tok::Other);
                prev_is_value = true;
                continue;
            }
        }
        // 标识符
        if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == '_' || b[i] == '$') {
                i += 1;
            }
            let w: String = b[start..i].iter().collect();
            prev_is_value = matches!(w.as_str(), "true" | "false" | "null" | "undefined")
                || !matches!(
                    w.as_str(),
                    "return"
                        | "typeof"
                        | "case"
                        | "in"
                        | "of"
                        | "new"
                        | "delete"
                        | "void"
                        | "await"
                );
            out.push(Tok::Word(w));
            continue;
        }
        // 数字
        if c.is_ascii_digit() {
            while i < n && (b[i].is_ascii_alphanumeric() || b[i] == '.') {
                i += 1;
            }
            out.push(Tok::Other);
            prev_is_value = true;
            continue;
        }
        // 标点
        if matches!(c, '{' | '}' | ':' | ',' | '(' | ')' | '[' | ']') {
            prev_is_value = matches!(c, ')' | ']');
            out.push(Tok::Punct(c));
        } else {
            prev_is_value = false;
        }
        i += 1;
    }
    out
}

/// 读 `'..'` / `".."` 字符串，处理转义。返回（内容，结束后的下标）。
fn read_string(b: &[char], mut i: usize, quote: char) -> (String, usize) {
    i += 1; // 跳过开头引号
    let mut s = String::new();
    while i < b.len() {
        match b[i] {
            '\\' => {
                i += 1;
                if i < b.len() {
                    s.push(b[i]);
                    i += 1;
                }
            }
            c if c == quote => {
                i += 1;
                break;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    (s, i)
}

/// 读模板串：只收纯文本段（遇 `${` 起到配对 `}` 全部丢弃）。
fn read_template(b: &[char], mut i: usize) -> (String, usize) {
    i += 1;
    let mut s = String::new();
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            '\\' => {
                i += 1;
                if i < b.len() {
                    s.push(b[i]);
                    i += 1;
                }
            }
            '`' if depth == 0 => {
                i += 1;
                break;
            }
            '$' if i + 1 < b.len() && b[i + 1] == '{' => {
                depth += 1;
                i += 2;
            }
            '{' if depth > 0 => {
                depth += 1;
                i += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                i += 1;
            }
            c if depth == 0 => {
                s.push(c);
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    (s, i)
}

/// 从 token 流里收集 `const/let/var NAME = "value"` 的字符串常量。
///
/// tokenizer 不产出 `=` 标点（只有 `{ } : , ( ) [ ]`），所以声明形态在
/// token 流里表现为连续三元组 `[Word(const|let|var), Word(NAME), Str(value)]`。
/// 真实插件的 provider id 常写成 `const PROVIDER = "wps-lingxi"` 再
/// `provider: PROVIDER` 引用——只认字符串字面量会整片漏掉（生产 count=0
/// 的根因）。
fn collect_consts(toks: &[Tok]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut i = 0;
    while i + 2 < toks.len() {
        let is_decl = matches!(&toks[i], Tok::Word(w) if w == "const" || w == "let" || w == "var");
        if is_decl {
            if let (Tok::Word(name), Tok::Str(val)) = (&toks[i + 1], &toks[i + 2]) {
                map.insert(name.clone(), val.clone());
                i += 3;
                continue;
            }
        }
        i += 1;
    }
    map
}

/// 一条具名 import 绑定：`import { imported as local } from "./spec"`。
#[derive(Clone, Debug, PartialEq)]
struct NamedImport {
    /// 本文件里的本地名（`A as B` 取 B）。
    local: String,
    /// 源文件里导出的名字（`A as B` 取 A）。
    imported: String,
    /// 相对说明符（`./x` / `../y`）。
    spec: String,
}

/// 解析 `import { A, B as C } from "./x"` 形态的具名导入。
///
/// 只处理带 `{ … }` 的具名导入（provider id 常量基本都是具名导出）；
/// 默认导入 / 命名空间导入不参与常量解析（极少承载 provider id）。
fn parse_named_imports(toks: &[Tok]) -> Vec<NamedImport> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if !matches!(&toks[i], Tok::Word(w) if w == "import") {
            i += 1;
            continue;
        }
        // 跳过 `import(` 动态导入与 `import.meta`
        if matches!(toks.get(i + 1), Some(Tok::Punct('('))) {
            i += 1;
            continue;
        }
        // 必须是 `import { … } from "<spec>"`
        if !matches!(toks.get(i + 1), Some(Tok::Punct('{'))) {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        let mut pairs: Vec<(String, String)> = Vec::new(); // (imported, local)
        while j < toks.len() && !matches!(toks.get(j), Some(Tok::Punct('}'))) {
            match toks.get(j) {
                Some(Tok::Word(name)) => {
                    // 可能是 `A` 或 `A as B`
                    if matches!(toks.get(j + 1), Some(Tok::Word(a)) if a == "as") {
                        if let Some(Tok::Word(local)) = toks.get(j + 2) {
                            pairs.push((name.clone(), local.clone()));
                            j += 3;
                            continue;
                        }
                    }
                    pairs.push((name.clone(), name.clone()));
                    j += 1;
                }
                _ => j += 1,
            }
        }
        // 期望 `} from "<spec>"`
        if matches!(toks.get(j), Some(Tok::Punct('}')))
            && matches!(toks.get(j + 1), Some(Tok::Word(f)) if f == "from")
        {
            if let Some(Tok::Str(spec)) = toks.get(j + 2) {
                if spec.starts_with("./") || spec.starts_with("../") {
                    for (imported, local) in pairs {
                        out.push(NamedImport {
                            local,
                            imported,
                            spec: spec.clone(),
                        });
                    }
                }
            }
        }
        i = j.max(i + 1);
    }
    out
}

/// 在 token 流里找 `auth` 后接对象字面量，并在其中找 `provider: <值>`。
///
/// `<值>` 可以是字符串字面量，也可以是标识符（常量引用）；后者交给
/// `resolve` 闭包解析成字符串。两种前缀形态都收（都是真实 npm 包里的常见写法）：
/// - **对象属性** `auth: { … }`（auth 后紧跟 `:`）
/// - **变量赋值** `const auth = { … }` / `export const auth = { … }`
///   （auth 后是 `=`；整个常量对象就是 provider 定义，这是上游脚本
///   导出 provider 的主流写法，漏掉会把有效包误判为「无 auth 证据」）
///
/// 找到 `auth` 词后向后看有限窗口（256 token ≈ 一个对象字面量的合理大小）：
/// 窗口内出现 `provider:` 且值可解析成字符串即命中；`methods`/`loader` 键
/// 提升置信但不强求。
fn find_auth_provider_with(toks: &[Tok], resolve: &dyn Fn(&str) -> Option<String>) -> ScanOutcome {
    let mut out = ScanOutcome::default();
    for (idx, tok) in toks.iter().enumerate() {
        if !matches!(tok, Tok::Word(w) if w == "auth") {
            continue;
        }
        // 三种前缀形态都收（都是真实 npm 包里的常见写法）：
        // - `auth: { … }`（对象属性）
        // - `auth = { … }`（变量赋值）
        // - `auth { … }`（tokenizer 不产出 `=` 标点，赋值号的 `=` 被吞掉，
        //   于是赋值形态在这里表现为 auth 紧跟 `{`）
        let body_start = match (toks.get(idx + 1), toks.get(idx + 2)) {
            (Some(Tok::Punct(':') | Tok::Punct('=')), Some(Tok::Punct('{'))) => idx + 3,
            (Some(Tok::Punct('{')), _) => idx + 2,
            _ => continue,
        };
        // 在对象体内扫：provider: <值>（对象边界用大括号配对）
        let mut depth = 1usize;
        let mut j = body_start;
        let mut provider: Option<String> = None;
        let mut label: Option<String> = None;
        let mut evidence = 0usize;
        while j < toks.len() && j <= idx + 256 {
            match &toks[j] {
                Tok::Punct('{') | Tok::Punct('[') | Tok::Punct('(') => depth += 1,
                Tok::Punct('}') | Tok::Punct(']') | Tok::Punct(')') => {
                    if depth == 1 {
                        break; // auth 对象结束
                    }
                    depth -= 1;
                }
                Tok::Word(w) if depth == 1 => {
                    if w == "methods" || w == "loader" {
                        evidence += 1;
                    }
                    if (w == "provider" || w == "label" || w == "name")
                        && matches!(toks.get(j + 1), Some(Tok::Punct(':')))
                    {
                        // 值：字符串字面量 或 标识符（常量引用）
                        let value = match toks.get(j + 2) {
                            Some(Tok::Str(s)) => Some(s.clone()),
                            Some(Tok::Word(ident)) => resolve(ident),
                            _ => None,
                        };
                        if let Some(s) = value {
                            if w == "provider" {
                                provider = Some(s);
                            } else if label.is_none() && !s.is_empty() {
                                label = Some(s);
                            }
                        }
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if let Some(p) = provider {
            out.provider_id = Some(p);
            out.label = label;
            let _ = evidence;
            return out;
        }
    }
    out
}

/// 只认字符串字面量版本（无常量解析；仅测试用，保留直观的基线断言）。
#[cfg(test)]
fn find_auth_provider(toks: &[Tok]) -> ScanOutcome {
    find_auth_provider_with(toks, &|_| None)
}

/// 为一个文件的 token 流构造常量环境：同文件 `const` + 一层具名 import。
///
/// provider id 在生产插件里几乎都以常量形态出现（`const PROVIDER="x"` 或
/// `import { POOL_PROVIDER } from "./lib"`），只认字面量会整片漏掉。这里
/// 解析同文件常量，并跟随**一层**相对具名导入（provider id 常量极少跨
/// 两层以上，限定一层即可覆盖现实形态且避免无界读文件）。
fn build_const_env(root: &Path, file: &Path, toks: &[Tok]) -> HashMap<String, String> {
    let mut env = collect_consts(toks);
    for imp in parse_named_imports(toks) {
        let Some(target) = resolve_import(file, &imp.spec, root) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&target) else {
            continue;
        };
        if bytes.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(src) = String::from_utf8(bytes) else {
            continue;
        };
        let child = collect_consts(&tokenize(&src));
        if let Some(val) = child.get(&imp.imported) {
            env.insert(imp.local.clone(), val.clone());
        }
    }
    env
}

/// 清单自己声明的提供商。有声明就不再读源码，未声明才回退到入口文件扫描。
///
/// 读取口径（对齐 A2A beta.3）：
/// - `magpie.providers`：数组，元素可为字符串或 `{id}` 对象；也收单字符串；
/// - `magpie.provider`：字符串或 `{id}` 对象；
/// - 每个 id 校验非空、≤128 字符、仅 `[A-Za-z0-9-_./]`，非法值静默丢弃；
/// - `magpie.kind=="provider"` 时即使没有列表也返回 `Some(空)`（自声明即提供商）；
/// - 其余情况返回 `None`（未自声明，交给源码扫描）。
pub(super) fn declared_providers(manifest: &serde_json::Value) -> Option<Vec<String>> {
    fn valid(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || b"-_./".contains(&ch))
    }
    fn push(providers: &mut BTreeSet<String>, value: &str) {
        if valid(value) {
            providers.insert(value.to_string());
        }
    }
    let magpie = &manifest["magpie"];
    let mut providers = BTreeSet::new();
    match &magpie["providers"] {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(value) = item.as_str() {
                    push(&mut providers, value);
                } else if let Some(value) = item.get("id").and_then(serde_json::Value::as_str) {
                    push(&mut providers, value);
                }
            }
        }
        serde_json::Value::String(value) => push(&mut providers, value),
        _ => {}
    }
    match &magpie["provider"] {
        serde_json::Value::String(value) => push(&mut providers, value),
        serde_json::Value::Object(value) => {
            if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
                push(&mut providers, id);
            }
        }
        _ => {}
    }
    if providers.is_empty() && magpie["kind"].as_str() != Some("provider") {
        return None;
    }
    Some(providers.into_iter().collect())
}

/// 解析一个 JS/TS 文件里的相对导入说明符（`./` `../` 开头）。
///
/// 公开给 github.rs：深扫要把相对导入链拉下来才能本地 BFS。
pub fn relative_imports(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in tokenize(src) {
        if let Tok::Str(s) = tok {
            let t = s.trim();
            if t.starts_with("./") || t.starts_with("../") {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// 从一个目录包里解析相对导入的落点文件。
///
/// 说明符 → 实际文件按 Node 解析惯例：先原样、再 `.js` / `.ts` / `.jsx` /
/// `.tsx` / `.mjs` / `.cjs` 后缀、再 `index.*`；不解析包根外的路径
/// （`../` 上溯越过 root 直接放弃——相对链必须封闭在包内）。
fn resolve_import(base_file: &Path, spec: &str, root: &Path) -> Option<PathBuf> {
    let dir = base_file.parent()?;
    let target = dir.join(spec);
    // 越界检查：解析出的目标必须仍在包根内（防 ../ 逃逸出扫描边界）
    let ok_prefix = target.strip_prefix(root).is_ok();
    if !ok_prefix {
        return None;
    }
    const EXTS: [&str; 6] = ["js", "ts", "jsx", "tsx", "mjs", "cjs"];
    let mut expanded: Vec<PathBuf> = vec![target.clone()];
    for e in EXTS {
        let mut p2 = target.clone().into_os_string();
        p2.push(".");
        p2.push(e);
        expanded.push(PathBuf::from(p2));
    }
    for e in ["js", "ts", "mjs", "cjs"] {
        expanded.push(target.join(format!("index.{e}")));
    }
    expanded
        .into_iter()
        .find(|p| p.is_file() && p.starts_with(root))
}

/// 扫描一个已安装/解包目录里的入口，找 auth.provider。
///
/// `entry` 是相对包根的入口路径（package.json main / magpie.provider 键等
/// 由调用方解析好）；入口文件不存在返回 None。
pub fn scan_entry(root: &Path, entry: &Path) -> Option<ScanOutcome> {
    let start = root.join(entry);
    let Ok(bytes) = std::fs::read(&start) else {
        return None;
    };
    if bytes.len() > MAX_FILE_BYTES {
        return None;
    }
    let Ok(src) = String::from_utf8(bytes) else {
        return None;
    };
    let mut visited: HashSet<PathBuf> = HashSet::new();
    visited.insert(start.clone());
    let mut queue: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
    queue.push_back(start.clone());

    let mut total = src.len();
    let entry_toks = tokenize(&src);
    let entry_env = build_const_env(root, &start, &entry_toks);
    let outcome = find_auth_provider_with(&entry_toks, &|id| entry_env.get(id).cloned());
    if outcome.provider_id.is_some() {
        return Some(ScanOutcome {
            files_read: 1,
            ..outcome
        });
    }
    // BFS 相对导入链（入口没有 → 可能 export 自子模块）
    while let Some(file) = queue.pop_front() {
        if visited.len() >= MAX_SCAN_FILES || total >= MAX_TOTAL_BYTES {
            break;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for spec in relative_imports(&text) {
            let Some(target) = resolve_import(&file, &spec, root) else {
                continue;
            };
            if visited.contains(&target) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&target) else {
                continue;
            };
            if bytes.len() > MAX_FILE_BYTES {
                continue;
            }
            total += bytes.len();
            if total > MAX_TOTAL_BYTES {
                break;
            }
            let Ok(child_src) = String::from_utf8(bytes) else {
                continue;
            };
            visited.insert(target.clone());
            let child_toks = tokenize(&child_src);
            let child_env = build_const_env(root, &target, &child_toks);
            let oc = find_auth_provider_with(&child_toks, &|id| child_env.get(id).cloned());
            if oc.provider_id.is_some() {
                return Some(ScanOutcome {
                    files_read: visited.len(),
                    ..oc
                });
            }
            queue.push_back(target);
        }
    }
    Some(ScanOutcome {
        files_read: visited.len(),
        ..ScanOutcome::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_skips_strings_and_comments() {
        let src = r#"
        // auth: { provider: "fake-in-comment" }
        /* auth: { provider: "fake-in-block" } */
        const s = 'auth: { provider: "fake-in-string" }';
        const t = `auth ${ "x" } { provider: "fake-in-template" }`;
        const real = {
            auth: {
                provider: "real-one",
                methods: [{ type: "oauth" }],
                loader: async () => ({}),
            },
        };
        "#;
        let toks = tokenize(src);
        let oc = find_auth_provider(&toks);
        assert_eq!(oc.provider_id.as_deref(), Some("real-one"));
    }

    #[test]
    fn tokenizer_handles_regex_vs_division() {
        let src = r#"
        const ratio = a / b / c;
        const re = /provider: "no"/;
        const obj = { auth: { provider: "yes" } };
        "#;
        let oc = find_auth_provider(&tokenize(src));
        assert_eq!(oc.provider_id.as_deref(), Some("yes"));
    }

    #[test]
    fn finds_provider_with_label() {
        let src = r#"export default {
          auth: {
            provider: "kiro",
            label: "Kiro AI",
            loader: (ctx) => ctx,
          },
        }"#;
        let oc = find_auth_provider(&tokenize(src));
        assert_eq!(oc.provider_id.as_deref(), Some("kiro"));
        assert_eq!(oc.label.as_deref(), Some("Kiro AI"));
    }

    #[test]
    fn no_auth_no_hit() {
        let src = "export const x = { provider: 'bare-provider-not-in-auth' };";
        let oc = find_auth_provider(&tokenize(src));
        assert_eq!(oc.provider_id, None);
    }

    #[test]
    fn bfs_follows_relative_imports() {
        let dir = std::env::temp_dir().join("aigx_scan_test_pkg");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(
            dir.join("entry.js"),
            "export { auth } from './lib/real.js';\nexport const other = 1;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("lib").join("real.js"),
            "export const auth = { provider: 'deep-one', methods: [] };\n",
        )
        .unwrap();
        let oc = scan_entry(&dir, Path::new("entry.js")).unwrap();
        assert_eq!(oc.provider_id.as_deref(), Some("deep-one"));
        assert_eq!(oc.files_read, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_file_const_provider_resolves() {
        // 生产真形态：provider id 来自同文件常量（lingxi: const PROVIDER）。
        let src = r#"
        const PROVIDER = "wps-lingxi"
        export default {
          provider: { id: PROVIDER },
          auth: { provider: PROVIDER, icon: "x" },
        }
        "#;
        let toks = tokenize(src);
        let env = collect_consts(&toks);
        assert_eq!(env.get("PROVIDER").map(String::as_str), Some("wps-lingxi"));
        let oc = find_auth_provider_with(&toks, &|id| env.get(id).cloned());
        assert_eq!(oc.provider_id.as_deref(), Some("wps-lingxi"));
    }

    #[test]
    fn cross_file_imported_const_provider_resolves() {
        // 生产真形态：provider id 来自 `import { POOL_PROVIDER } from "./lib"`
        //（gptpool），常量定义在另一文件。
        let dir = std::env::temp_dir().join("aigx_scan_const_import");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(
            dir.join("index.mjs"),
            "import { createPoolConnector, POOL_PROVIDER } from './lib/pool.mjs';\n\
             export default { auth: { provider: POOL_PROVIDER } };\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("lib").join("pool.mjs"),
            "export const POOL_PROVIDER = \"gptfree-pool\";\nexport function createPoolConnector(){}\n",
        )
        .unwrap();
        let oc = scan_entry(&dir, Path::new("index.mjs")).unwrap();
        assert_eq!(oc.provider_id.as_deref(), Some("gptfree-pool"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aliased_import_const_resolves() {
        // `import { A as B }`：provider: B 应解析成 A 的值。
        let toks = tokenize(
            "import { RAW_ID as ID } from './c.mjs';\nexport default { auth: { provider: ID } };",
        );
        let imports = parse_named_imports(&toks);
        assert_eq!(
            imports,
            vec![NamedImport {
                local: "ID".into(),
                imported: "RAW_ID".into(),
                spec: "./c.mjs".into()
            }]
        );
    }

    #[test]
    fn missing_entry_is_none() {
        assert!(scan_entry(Path::new("/nonexistent"), Path::new("x.js")).is_none());
    }

    #[test]
    fn declared_providers_reads_manifest_forms() {
        // 数组：字符串 + {id} 对象混合，非法 id 丢弃。
        let m = serde_json::json!({"magpie": {"providers": ["kiro", {"id": "wps-lingxi"}, "", "bad id!"]}});
        assert_eq!(
            declared_providers(&m),
            Some(vec!["kiro".to_string(), "wps-lingxi".to_string()])
        );
        // 单字符串 providers。
        let m = serde_json::json!({"magpie": {"providers": "solo"}});
        assert_eq!(declared_providers(&m), Some(vec!["solo".to_string()]));
        // magpie.provider 字符串 / {id}。
        let m = serde_json::json!({"magpie": {"provider": "one"}});
        assert_eq!(declared_providers(&m), Some(vec!["one".to_string()]));
        let m = serde_json::json!({"magpie": {"provider": {"id": "two"}}});
        assert_eq!(declared_providers(&m), Some(vec!["two".to_string()]));
        // kind==provider 无列表 → Some(空)。
        let m = serde_json::json!({"magpie": {"kind": "provider"}});
        assert_eq!(declared_providers(&m), Some(vec![]));
        // 未自声明 → None。
        let m = serde_json::json!({"name": "x"});
        assert_eq!(declared_providers(&m), None);
        // id 超 128 字符被丢弃 → 无有效 id 且非 kind=provider → None。
        let long = "a".repeat(129);
        let m = serde_json::json!({"magpie": {"providers": [long]}});
        assert_eq!(declared_providers(&m), None);
    }
}
