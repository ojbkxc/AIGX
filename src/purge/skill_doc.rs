//! 技能文档（`SKILL.md`）的模板、关键词归一与写入校验。
//!
//! ── 为什么单独一个模块 ───────────────────────────────────────
//! `skill_import` 只处理**归档**（ZIP/TAR/TGZ 的安全解压），而「新建技能 / 粘贴
//! 正文 / 上传单个 .md」这条路径不涉及归档，却必须与**后端解析器**（本模块旁的
//! `mod::parse_skill`）逐字对齐：模板生成的 frontmatter 只要与解析器差一个字符
//! （少个 `---`、`keywords` 拼成 `keyword`、冒号用了全角），技能就永远扫不到或被
//! 静默降级成「只有 name 一个关键词」——而界面上它看起来完全正常。
//!
//! 所以判据只留一份：[`keywords_from`] 是关键词归一（解析器与模板生成同源），
//! [`validate`] 是写盘前的解析回读（**用解析器本身**校验，不另写一套规则）。
//!
//! ── frontmatter 契约（与 `mod::parse_skill` 完全一致）──────────
//! ```text
//! ---
//! name: pdf-report             # 必填；缺省时回落父目录名（模板里必须写）
//! description: 导出 PDF 报表    # 可选；词按空白/中英标点切分，≥2 字符的进关键词
//! keywords: 报表, 对账, 月结     # 可选；逗号/顿号/分号/空白分隔，显式关键词
//! ---
//!
//! 正文（命中后注入 system 的内容）
//! ```
//! 解析器只认 `---` 包起来的**首段**：`keywords:` 是本轮新增的显式键，旧文件
//! （只有 name/description）行为逐字节不变——它们的关键词仍由 name + description
//! 派生（`keywords_from` 的 `explicit` 为空即旧口径）。

use std::path::{Path, PathBuf};

/// 文件名（解析器按大小写不敏感匹配这个名字）。
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// 关键词分隔符（中英标点都认：技能作者常混用）。
fn is_keyword_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | '，' | '、' | ';' | '；' | ':' | '：' | '/' | '|')
}

/// 关键词归一的**唯一实现**（解析器与模板生成都调它）。
///
/// 顺序 = `name` → 显式 `keywords:` → `description` 派生词。顺序有意义：命中
/// 明细回显「第一个命中的关键词」，name 是作者最强的意图声明，放在最前。
///
/// 去重是**大小写不敏感的集合去重**（旧实现用 `Vec::dedup` 只压相邻重复，
/// 同一关键词隔一个词就会重复出现——匹配上无害，但命中明细会报两次）。
pub fn keywords_from(name: &str, description: &str, explicit: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |token: &str| {
        let token = token.trim();
        if token.is_empty() {
            return;
        }
        let key = token.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(token.to_string());
        }
    };
    push(name);
    for keyword in explicit {
        push(keyword);
    }
    for token in description.split(is_keyword_sep) {
        // 单字符描述词没有检索价值，且会把命中面撑得极宽（子串匹配）。
        if token.trim().chars().count() >= 2 {
            push(token);
        }
    }
    out
}

/// 解析 `keywords:` 行的值（显式关键词）。空串 → 空表。
pub fn parse_keywords_value(raw: &str) -> Vec<String> {
    raw.split(is_keyword_sep)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

/// 生成技能骨架（**格式即解析器契约**，见模块头）。
///
/// `name` / `keywords` 都为空时仍产出可解析的文档（name 由调用方决定必填，
/// 见 [`validate`]）；正文用占位说明，避免「新建出来是空文件、解析器直接丢弃」。
pub fn skeleton(name: &str, description: &str, keywords: &[String], body: &str) -> String {
    let name = name.trim();
    let description = description.trim();
    let keywords: Vec<String> = keywords
        .iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    let body = body.trim();
    let body = if body.is_empty() {
        "（这里写命中后要注入的正文：说明这个技能解决什么问题、给模型的约束或步骤。）"
    } else {
        body
    };
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    if !description.is_empty() {
        out.push_str(&format!("description: {description}\n"));
    }
    // `keywords:` **永远输出**（即使为空）。
    //
    // 旧实现「没给关键词就不写这行」有个后果：前端「生成骨架 → 填正文 → 写入」
    // 是设计好的主路径，而它产出的 SKILL.md 必然缺 `keywords:` 键 → 体检立刻报
    // `skill_no_keywords`（「只能靠 name/description 派生关键词，命中面很窄」）。
    // 生成器产出自己告警的东西，用户照着做还是有一条告警消不掉。
    //
    // 空值 `keywords:` 解析出来是空表（`parse_keywords` 对空串返回空），语义不变，
    // 但键**可见**了，用户知道有这个字段。行内不加 `#` 注释，避免污染解析。
    out.push_str(&format!("keywords: {}\n", keywords.join(", ")));
    out.push_str("---\n\n");
    out.push_str(body);
    out.push('\n');
    out
}

/// 校验结果：写盘前**用解析器本身**回读的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validated {
    pub name: String,
    pub keywords: Vec<String>,
    /// 正文是否非空（解析器丢弃空正文 → 技能永远不会命中）。
    pub body_chars: usize,
    /// frontmatter 是否带显式 `keywords:` 键。
    pub has_keywords_key: bool,
    /// 非致命提示（写盘照做，界面展示）。
    pub warnings: Vec<String>,
}

/// 校验一份待写入的技能正文。
///
/// - `Err`：解析器读不出正文（写进去等于没写，必须拒）；
/// - `Ok.warnings`：能生效但大概率不好用（没显式 keywords、只有 name 一个词、
///   正文超过注入上限 → 会被截断）。
///
/// `inject_limit` 传当前配置的 `skill_inject_limit`，用于「会截断」提示；
/// 传 0 表示不校验长度。
///
/// 这个入口**要求 frontmatter 里有 name**（骨架/预览路径还没有目录名可兜底）。
/// 写盘路径请用 [`validate_with_fallback`]，那里目录名已经定了，与扫描器
/// `parse_skill` 的「name 缺省回落父目录名」保持一致。
pub fn validate(content: &str, inject_limit: usize) -> Result<Validated, String> {
    validate_with_fallback(content, inject_limit, "")
}

/// [`validate`] 的带目录名兜底版：frontmatter 没写 `name` 时用 `dir_name`。
/// 从 SKILL.md 的 frontmatter 首段里取 `description:` 的值（无则空串）。
///
/// 口径与 `mod::parse_skill_parts` 一致：只看 `---` 包起来的首段，忽略引号。
fn extract_description_line(content: &str) -> String {
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return String::new();
    };
    let Some(end) = rest.find("\n---") else {
        return String::new();
    };
    for line in rest[..end].lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("description:") {
            return value.trim().trim_matches('"').to_string();
        }
    }
    String::new()
}

pub fn validate_with_fallback(
    content: &str,
    inject_limit: usize,
    dir_name: &str,
) -> Result<Validated, String> {
    // 判据是「解析器能不能读出正文」，不是「有没有 frontmatter」：
    // 裸正文（无 frontmatter）按「整份是正文 + 名字回落目录名」处理，是预设库的
    // 既有形态，必须放行；只有连正文都读不出来（body 为空）才拒。
    //
    // （此前误加过「必须有 frontmatter」的前置，结果把裸正文这条主路径挡住了，
    // 已撤回 —— 见 raw_body_without_frontmatter_still_writes 的注释。）
    let (mut name, mut keywords, has_keywords_key, body) = super::parse_skill_parts(content)
        .ok_or_else(|| {
            "解析器读不出正文：需要 `---` 包起来的 frontmatter，且 `---` 之后要有正文".to_string()
        })?;
    let fallback = dir_name.trim();
    if name.trim().is_empty() {
        if fallback.is_empty() {
            return Err(
                "缺少 name（frontmatter 第一段里的 `name: <技能名>`，或显式传目录名）".to_string(),
            );
        }
        // 与 `mod::parse_skill` 同口径：名字取目录名，关键词随之重算——
        // 否则「没写 name 的技能」连一个关键词都没有，永远不命中。
        //
        // description 一并重算：只传目录名会把 frontmatter 里 description 派生的
        // 词一起丢掉，等于「名字有了、词没了」。description 从原始内容里再取一次
        // （`parse_skill_parts` 的返回元组不带它）。
        name = fallback.to_string();
        let description = extract_description_line(&content);
        keywords = keywords_from(fallback, &description, &[]);
    }
    let body_chars = body.chars().count();
    let mut warnings = Vec::new();
    if !has_keywords_key {
        warnings.push(
            "frontmatter 没有 `keywords:` 行：关键词只能从 name/description 派生，命中面会很窄（只有名字本身能命中）"
                .to_string(),
        );
    }
    if keywords.len() <= 1 {
        warnings.push(
            "解析后只有 1 个关键词（name）：消息里必须逐字出现这个名字才会命中，建议补 `keywords:` 或 description"
                .to_string(),
        );
    }
    if inject_limit > 0 && body_chars > inject_limit {
        warnings.push(format!(
            "正文 {body_chars} 字符，超过注入上限 {inject_limit}：命中后只注入前 {inject_limit} 字符，尾部会被截掉"
        ));
    }
    Ok(Validated {
        name,
        keywords,
        body_chars,
        has_keywords_key,
        warnings,
    })
}

/// 文本导入结果（与归档导入的 `ImportReport` 键集兼容的超集）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TextImportReport {
    /// `text`：单文件（粘贴/上传 .md）；`template`：骨架新建。
    pub mode: &'static str,
    /// 实际落盘的目录名（技能目录下的单级目录）。
    pub dir: String,
    /// 写入的普通文件数（恒 1）。
    pub files: usize,
    /// 写入字节数。
    pub bytes: u64,
    /// 恒 true（否则不会写盘）。
    pub has_skill_md: bool,
    pub name: String,
    pub keywords: Vec<String>,
    pub body_chars: usize,
    pub has_keywords_key: bool,
    pub warnings: Vec<String>,
    /// `SKILL.md` 的绝对路径（界面「去哪里改」用）。
    pub path: String,
}

/// 把一份技能正文写进 `<skill_root>/<dir>/SKILL.md`。
///
/// 语义与 `presets::apply(SkillFile)` 一致：只写盘、不改配置。区别在于这里
/// **先校验再写**（解析器读不出正文的直接拒绝），并用「临时文件 + rename」
/// 落位——写到一半断电不会留下半个 `SKILL.md` 被扫描器当成合法技能。
pub fn write_skill(
    skill_root: &Path,
    dir_name: &str,
    content: &str,
    overwrite: bool,
    inject_limit: usize,
    mode: &'static str,
) -> Result<TextImportReport, String> {
    let slug = super::skill_import::slugify(dir_name).ok_or_else(|| {
        "目录名为空或不合法（仅允许字母/数字/连字符，来自 name 或显式 dir）".to_string()
    })?;
    // 校验放在 slug 之后：`dir_name` 就是扫描器将来兜底用的父目录名，
    // 所以「缺 name」在这里是**可恢复**的（与 `parse_skill` 同口径）。
    let validated = validate_with_fallback(content, inject_limit, &slug)?;
    let dir = skill_root.join(&slug);
    if dir.exists() && !overwrite {
        return Err(format!(
            "目标目录已存在：{slug}（如需覆盖请勾选「覆盖同名技能」）"
        ));
    }
    // 覆盖前校验是普通目录（不跟随符号链接）：`SKILL.md` 可能是别人放的软链，
    // 顺着写会把内容写到技能目录之外。
    if dir.exists() {
        let meta = std::fs::symlink_metadata(&dir)
            .map_err(|e| format!("读取目标目录信息失败：{slug}（{e}）"))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(format!("目标路径不是普通目录，拒绝覆盖：{slug}"));
        }
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建技能目录失败：{}（{e}）", dir.display()))?;

    let file = dir.join(SKILL_FILE_NAME);
    let temp = dir.join(format!(".SKILL.md.tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temp, content).map_err(|e| format!("写入临时文件失败：{e}"))?;
    if let Err(e) = std::fs::rename(&temp, &file) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("落位 SKILL.md 失败：{e}"));
    }
    let bytes = content.len() as u64;

    Ok(TextImportReport {
        mode,
        dir: slug,
        files: 1,
        bytes,
        has_skill_md: true,
        name: validated.name,
        keywords: validated.keywords,
        body_chars: validated.body_chars,
        has_keywords_key: validated.has_keywords_key,
        warnings: validated.warnings,
        path: file.to_string_lossy().to_string(),
    })
}

/// 技能目录里一份新建技能的落点（不写盘，供 API 回显 / 前端预览）。
pub fn target_path(skill_root: &Path, dir_name: &str) -> Option<PathBuf> {
    super::skill_import::slugify(dir_name).map(|slug| skill_root.join(slug).join(SKILL_FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aigx-skill-doc-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 模板 → 解析器：**格式与解析器逐字一致**（这是本模块的存在理由）。
    ///
    /// 用后端解析器本身回读，而不是自己再写一遍 frontmatter 正则——
    /// 两份实现漂移时，界面新建出来的技能会「看起来正常、永远不命中」。
    #[test]
    /// 不传关键词时，骨架**仍然**要带 `keywords:` 行。
    ///
    /// 旧实现只在 keywords 非空时才写这行，于是「生成骨架 → 填正文 → 写入」
    /// 这条主路径产出的 SKILL.md 必然缺该键 → 体检报 `skill_no_keywords`。
    /// 生成器产出自己告警的东西，用户照做还是消不掉。
    #[test]
    fn skeleton_always_emits_keywords_line() {
        let doc = skeleton("plain-skill", "", &[], "");
        assert!(
            doc.contains("keywords:"),
            "骨架必须带 keywords 行（哪怕为空）：{doc}"
        );
        let validated = validate(&doc, 1500).expect("空关键词的骨架也必须可解析");
        assert!(
            validated.has_keywords_key,
            "解析器必须能看见这个键：{validated:?}"
        );
        // 空值解析成空表：语义不变，只让键可见
        assert_eq!(validated.name, "plain-skill");
        assert!(
            validated.keywords.iter().all(|k| !k.is_empty()),
            "空表里不该有空串关键词：{validated:?}"
        );
    }

    fn skeleton_round_trips_through_parser() {
        let keywords = vec!["对账".to_string(), "月结".to_string()];
        let doc = skeleton("pdf-report", "导出 PDF 报表", &keywords, "");
        let validated = validate(&doc, 1500).expect("模板必须是可解析的");
        assert_eq!(validated.name, "pdf-report");
        assert!(validated.has_keywords_key, "模板必须带显式 keywords 行");
        assert_eq!(
            &validated.keywords[..3],
            &[
                "pdf-report".to_string(),
                "对账".to_string(),
                "月结".to_string()
            ],
            "name 在最前，显式关键词随后：{validated:?}"
        );
        assert!(
            validated.keywords.iter().any(|k| k.as_str() == "导出"),
            "description 派生的词也要进集合：{validated:?}"
        );
        assert!(
            validated.body_chars > 0,
            "空正文会被解析器丢弃，模板必须带占位正文"
        );
        assert!(
            validated.warnings.is_empty(),
            "模板不该自带告警：{validated:?}"
        );
    }

    /// 显式 `keywords:` 被解析进关键词集合；旧文件（无该行）行为不变。
    #[test]
    fn explicit_keywords_parse_and_merge() {
        let doc = "---\nname: demo\nkeywords: 报表, 对账、月结;结算\n---\n正文";
        let validated = validate(doc, 0).expect("应可解析");
        assert!(validated.has_keywords_key);
        for expected in ["报表", "对账", "月结", "结算"] {
            assert!(
                validated.keywords.iter().any(|k| k.as_str() == expected),
                "显式关键词 {expected} 应进集合：{:?}",
                validated.keywords
            );
        }

        // 旧口径：只有 name + description
        let legacy = "---\nname: legacy\n---\n正文";
        let validated = validate(legacy, 0).expect("旧文件必须照旧可解析");
        assert!(!validated.has_keywords_key);
        assert_eq!(validated.keywords, vec!["legacy".to_string()]);
        assert!(
            validated.warnings.iter().any(|w| w.contains("keywords")),
            "缺 keywords 必须提示：{:?}",
            validated.warnings
        );
    }

    /// 关键词去重是大小写不敏感的**全表**去重（旧实现只压相邻重复）。
    #[test]
    fn keywords_dedupe_case_insensitively() {
        let keywords = keywords_from("GLM", "glm 报表 REPORT 报表", &["glm".to_string()]);
        assert_eq!(
            keywords,
            vec!["GLM".to_string(), "报表".to_string(), "REPORT".to_string()],
            "同名不同大小写只留首个：{keywords:?}"
        );
    }

    /// 解析器读不出正文 / 缺 `name` → 拒绝写盘（写进去等于没写）。
    #[test]
    fn validate_rejects_unparsable_and_empty_body() {
        assert!(validate("没有 frontmatter 的正文", 0).is_err());
        // frontmatter 完整但正文为空：解析器返回 None
        assert!(validate("---\nname: x\n---\n\n", 0).is_err());
        // 有正文但没有 name → 写盘前拒绝。
        //
        // 与扫描器的 `parse_skill` 不冲突：那边能用父目录名兜底（文件已经在某个
        // 目录里了），而**写盘时目录名正是由 name 推导的**——没有 name 就没有
        // 落点，这里必须由调用方给 `dir`（API 层 `dir` 缺省时同样会拿 name 兜底，
        // 两处都不猜）。
        let err = validate("---\ndescription: 报表\n---\n正文", 0).unwrap_err();
        assert!(err.contains("name"), "应点名缺 name：{err}");
        // **写盘路径**：目录名已经定了 → 与扫描器的父目录名兜底同口径，不拒
        let with_dir =
            validate_with_fallback("---\ndescription: 报表\n---\n正文", 0, "fallback-dir")
                .expect("有目录名兜底时应可解析");
        assert_eq!(with_dir.name, "fallback-dir");
        assert_eq!(
            with_dir.keywords,
            vec!["fallback-dir".to_string(), "报表".to_string()],
            "名字取目录名，关键词随之重算（否则永远不会命中）：{with_dir:?}"
        );
        // 有正文 + 有 name（缺 keywords）→ 可解析，关键词含 name 与 derived 词
        let validated =
            validate("---\nname: n1\ndescription: 报表\n---\n正文", 0).expect("应可解析");
        assert_eq!(
            validated.keywords,
            vec!["n1".to_string(), "报表".to_string()]
        );
    }

    /// 内置预设的「裸正文」也能导入：**没有 frontmatter 也必须能落盘**。
    ///
    /// 这不是边角情况：`presets::apply(SkillFile)` 写进 `SKILL.md` 的就是预设正文
    /// 原文（实测 52 份预设里 **0 份**带 frontmatter），而扫描器 `parse_skill` 对
    /// 无 frontmatter 的文件按「整份是正文 + 名字回落父目录名」处理。写盘路径必须
    /// 同口径，否则「先从预设库落一份技能、再想改一个字」这条最自然的路径会被
    /// 我们自己的校验挡住——而那份技能明明是能生效的。
    #[test]
    fn raw_body_without_frontmatter_still_writes() {
        let root = temp_root("raw");
        let body = "### REGISTER\n\nYou are VANTA. 正文没有 frontmatter。";
        let report = write_skill(&root, "0x8d-jailbreaks-gpt-new", body, false, 1500, "text")
            .expect("裸正文必须能落盘（预设库的既有形态）");
        assert_eq!(report.dir, "0x8d-jailbreaks-gpt-new");
        assert!(
            report.name.starts_with("0x8d"),
            "没写 name 时取目录名：{}",
            report.name
        );
        assert!(!report.has_keywords_key, "裸正文没有 keywords 行");
        assert_eq!(report.body_chars, body.chars().count(), "整份都是正文");
        assert!(
            report.warnings.iter().any(|w| w.contains("keywords")),
            "应提示缺 keywords（用户能据此补）：{:?}",
            report.warnings
        );
        let file = root.join("0x8d-jailbreaks-gpt-new").join(SKILL_FILE_NAME);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            body,
            "正文逐字落盘"
        );
        // 落盘后**扫描器的完整路径**读得回来：名字回落父目录名、关键词随之重算
        // （`parse_skill_parts` 只拆 frontmatter，名字兜底在 `mod::parse_skill`）。
        let (raw_name, _, has_key, read_back) =
            super::super::parse_skill_parts(body).expect("裸正文应可解析");
        assert_eq!(raw_name, "", "没有 frontmatter 姓名");
        assert!(!has_key);
        assert_eq!(read_back, body);
        let entry = super::super::parse_skill(body, "0x8d-jailbreaks-gpt-new", &file)
            .expect("扫描器应读得出这份技能");
        assert_eq!(entry.name, "0x8d-jailbreaks-gpt-new", "名字回落父目录名");
        assert_eq!(
            entry.keywords,
            vec!["0x8d-jailbreaks-gpt-new".to_string()],
            "关键词只有名字（所以体检会提示缺 keywords）：{:?}",
            entry.keywords
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 正文超过注入上限 → 提示会截断（不是错误，写盘照做）。
    #[test]
    fn validate_warns_on_body_over_limit() {
        let body = "字".repeat(40);
        let doc = format!("---\nname: long\nkeywords: 长文\n---\n{body}");
        let over = validate(&doc, 16).expect("应可解析");
        assert!(
            over.warnings.iter().any(|w| w.contains("超过注入上限")),
            "超限必须提示：{:?}",
            over.warnings
        );
        // 正向对照：正好等于上限时不提示（否则告警恒真）
        let exact = validate(&doc, over.body_chars).expect("应可解析");
        assert!(
            exact.warnings.iter().all(|w| !w.contains("超过注入上限")),
            "等于上限不该报截断：{:?}",
            exact.warnings
        );
    }

    /// 写盘：落 `<dir>/SKILL.md`、可被解析器读回；同名不覆盖时拒绝。
    #[test]
    fn write_skill_lands_and_respects_overwrite() {
        let root = temp_root("write");
        let doc = "---\nname: land\nkeywords: 落地\n---\n正文内容";
        let report =
            write_skill(&root, "Land Skill", doc, false, 1500, "text").expect("写盘应成功");
        assert_eq!(report.dir, "land-skill");
        assert_eq!(report.name, "land");
        assert_eq!(report.files, 1);
        let file = root.join("land-skill").join(SKILL_FILE_NAME);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), doc);
        assert!(report.path.ends_with("SKILL.md"));

        // 同名不覆盖 → 拒绝且原文件不变
        let err = write_skill(
            &root,
            "land-skill",
            "---\nname: other\n---\n新",
            false,
            0,
            "text",
        )
        .unwrap_err();
        assert!(err.contains("已存在"), "应提示已存在：{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), doc);

        // 覆盖 → 内容替换
        let report = write_skill(
            &root,
            "land-skill",
            "---\nname: other\n---\n新",
            true,
            0,
            "text",
        )
        .unwrap();
        assert_eq!(report.name, "other");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "---\nname: other\n---\n新"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 解析器读不出的内容一律不落盘（连目录都不建）。
    #[test]
    fn write_skill_rejects_before_touching_disk() {
        let root = temp_root("reject");
        assert!(write_skill(&root, "broken", "无 frontmatter", false, 0, "text").is_err());
        assert!(
            std::fs::read_dir(&root).unwrap().next().is_none(),
            "校验失败不该留下任何目录"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
