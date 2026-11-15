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

use std::collections::HashSet;
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

/// 在 token 流里找 `auth` 后接对象字面量，并在其中找 `provider: "xxx"`。
///
/// 两种前缀形态都收（都是真实 npm 包里的常见写法）：
/// - **对象属性** `auth: { … }`（auth 后紧跟 `:`）
/// - **变量赋值** `const auth = { … }` / `export const auth = { … }`
///   （auth 后是 `=`；整个常量对象就是 provider 定义，这是上游脚本
///   导出 provider 的主流写法，漏掉会把有效包误判为「无 auth 证据」）
///
/// 找到 `auth` 词后向后看有限窗口（256 token ≈ 一个对象字面量的合理大小）：
/// 窗口内出现 `provider:` 且值是字符串即命中；`methods`/`loader` 键
/// 提升置信但不强求。
fn find_auth_provider(toks: &[Tok]) -> ScanOutcome {
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
        // 在对象体内扫：provider: "xxx"（对象边界用大括号配对）
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
                        && matches!(toks.get(j + 2), Some(Tok::Str(_)))
                    {
                        if let Some(Tok::Str(s)) = toks.get(j + 2) {
                            if w == "provider" {
                                provider = Some(s.clone());
                            } else if label.is_none() && !s.is_empty() {
                                label = Some(s.clone());
                            }
                        }
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if let Some(p) = provider {
            // provider 找到了才算命中；methods/loader 是佐证（保存在
            // provider_id 命中时无需额外输出——置信度管理面可见
            // provider_verified=true 即代表「找到 provider 字面量」）
            out.provider_id = Some(p);
            out.label = label;
            let _ = evidence;
            return out;
        }
    }
    out
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
    let outcome = find_auth_provider(&tokenize(&src));
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
            let oc = find_auth_provider(&tokenize(&child_src));
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
    fn missing_entry_is_none() {
        assert!(scan_entry(Path::new("/nonexistent"), Path::new("x.js")).is_none());
    }
}
