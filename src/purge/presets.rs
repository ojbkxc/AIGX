//! 破甲预设库：浏览内置 prompt 集合，并把它一键落到两种生效位置。
//!
//! 目录本身由 `preset_catalog.rs` 在编译期提供（`include_str!` 内联，运行时
//! 零磁盘依赖），本模块只负责检索、截断预览和应用。
//!
//! ── 两种应用目标 ────────────────────────────────────────────
//! - [`ApplyTarget::StructuredTemplate`]：把正文 upsert 进
//!   `structured_templates`，可选同时置 `active_template`。**不落库**——保存由
//!   调用方（管理面 PUT）负责，这样页面上的草稿和已保存状态不会打架。
//! - [`ApplyTarget::SkillFile`]：写成 `<skill_dir>/<id>/SKILL.md`，交给既有的
//!   `skill_inject` 按关键词命中注入。只写盘，不改配置。
//!
//! 预设正文是第三方公开研究集合的原文，这里只做搬运与应用，不做改写。

use anyhow::{bail, Context, Result};

use super::preset_catalog::{BuiltinPreset, BUILTIN_PRESETS};
use super::{expand_home, PurgeSettings};

/// 预览截断长度（字符数，不是字节数——按 char 边界截断才不会切坏多字节字符）。
const PREVIEW_CHARS: usize = 200;

/// 列表默认条数。
pub const DEFAULT_LIMIT: usize = 50;

/// 列表条数上限。
pub const MAX_LIMIT: usize = 500;

/// 列表条目（不含正文，只带预览）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PresetBrief {
    pub id: String,
    pub vendor: String,
    pub model: String,
    pub title: String,
    pub tags: Vec<String>,
    pub bytes: usize,
    pub sha256: String,
    pub preview: String,
}

/// 详情（含完整正文）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PresetDetail {
    pub id: String,
    pub vendor: String,
    pub model: String,
    pub title: String,
    pub tags: Vec<String>,
    pub bytes: usize,
    pub sha256: String,
    pub body: String,
}

/// 厂商条目（带数量，供 UI 的过滤 chip 用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VendorCount {
    pub vendor: String,
    pub count: usize,
}

/// 应用目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyTarget {
    /// 写成结构化模板（可选激活）。
    StructuredTemplate,
    /// 写成技能目录下的 SKILL.md。
    SkillFile,
}

/// 应用结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppliedPreset {
    /// `structured_template` / `skill_file`
    pub target: String,
    /// 模板名或技能名
    pub name: String,
    /// 写盘路径（仅 skill_file 有）
    pub path: Option<String>,
}

fn to_brief(preset: &BuiltinPreset) -> PresetBrief {
    PresetBrief {
        id: preset.id.to_string(),
        vendor: preset.vendor.to_string(),
        model: preset.model.to_string(),
        title: preset.title.to_string(),
        tags: preset.tags.iter().map(|t| t.to_string()).collect(),
        bytes: preset.bytes,
        sha256: preset.sha256.to_string(),
        preview: preview_of(preset.body),
    }
}

fn to_detail(preset: &BuiltinPreset) -> PresetDetail {
    PresetDetail {
        id: preset.id.to_string(),
        vendor: preset.vendor.to_string(),
        model: preset.model.to_string(),
        title: preset.title.to_string(),
        tags: preset.tags.iter().map(|t| t.to_string()).collect(),
        bytes: preset.bytes,
        sha256: preset.sha256.to_string(),
        body: preset.body.to_string(),
    }
}

/// 取正文前 `PREVIEW_CHARS` 个字符。
///
/// 用 `chars().take()` 而不是 `&body[..n]`：后者在 UTF-8 多字节字符中间
/// 切开会 panic，而预设正文里中文/emoji 都很常见。
fn preview_of(body: &str) -> String {
    let mut out: String = body.chars().take(PREVIEW_CHARS).collect();
    if body.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

fn matches(preset: &BuiltinPreset, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    preset.id.to_lowercase().contains(&needle)
        || preset.vendor.to_lowercase().contains(&needle)
        || preset.model.to_lowercase().contains(&needle)
        || preset.title.to_lowercase().contains(&needle)
        || preset
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(&needle))
}

/// 列表检索。
///
/// 返回 `(条目, 厂商计数, 过滤后总数)`。厂商计数按**过滤后**的结果统计，
/// 这样 UI 上的 chip 数量会跟着搜索词变，不会出现「chip 显示 8 条但列表空」。
/// `total` 是过滤后的总数（`items` 已被 `limit` 截断），供前端显示「共 N 条」。
pub fn list(
    q: Option<&str>,
    vendor: Option<&str>,
    limit: usize,
) -> (Vec<PresetBrief>, Vec<VendorCount>, usize) {
    let needle = q.map(str::trim).filter(|s| !s.is_empty());
    let vendor = vendor.map(str::trim).filter(|s| !s.is_empty());
    let limit = limit.clamp(1, MAX_LIMIT);

    let filtered: Vec<&BuiltinPreset> = BUILTIN_PRESETS
        .iter()
        .filter(|p| needle.is_none_or(|n| matches(p, n)))
        .filter(|p| vendor.is_none_or(|v| p.vendor.eq_ignore_ascii_case(v)))
        .collect();

    let total = filtered.len();

    let mut vendors: Vec<VendorCount> = Vec::new();
    for preset in &filtered {
        match vendors
            .iter_mut()
            .find(|v| v.vendor.eq_ignore_ascii_case(preset.vendor))
        {
            Some(entry) => entry.count += 1,
            None => vendors.push(VendorCount {
                vendor: preset.vendor.to_string(),
                count: 1,
            }),
        }
    }
    vendors.sort_by_key(|a| a.vendor.to_lowercase());

    let items = filtered
        .into_iter()
        .take(limit)
        .map(to_brief)
        .collect::<Vec<_>>();

    (items, vendors, total)
}

/// 按 id 取详情。
pub fn get(id: &str) -> Option<PresetDetail> {
    BUILTIN_PRESETS.iter().find(|p| p.id == id).map(to_detail)
}

/// 模型标识归一化：折叠大小写与分隔符，`DeepSeek V4` / `deepseek-v4` / `deepseek_v4`
/// 视为同一个模型。只做这一步，不做模糊前缀——猜错会把无关预设套上去。
pub fn normalize_model_id(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(*c, ' ' | '\t' | '\r' | '\n' | '.' | '_' | '-' | '/'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// 命中强度：预设目标模型就是当前模型（允许分隔符差异）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetMatch {
    /// 同名模型（含「一行覆盖多模型」的双向包含写法）。
    Exact,
    /// 操作者在 `auto_preset_id` 里点名的预设。
    Pinned,
}

/// 找写给这个模型的预设：**固定 id > 同名模型**，都没有就是 `None`。
///
/// 纯函数：不读盘、不联网、不改配置。目录是编译期常量，线性扫描 53 条足够快
/// （每个请求一次，归一化只发生在模型串上）。
///
/// 名字对不上时**坚决不猜**：把写给别的模型的 prompt 静默套上去，比不套更糟。
pub fn preset_for_model(model: &str, pinned_id: &str) -> Option<(PresetDetail, PresetMatch)> {
    let pinned = pinned_id.trim();
    if !pinned.is_empty() {
        return get(pinned).map(|detail| (detail, PresetMatch::Pinned));
    }

    let active = normalize_model_id(model);
    if active.is_empty() {
        return None;
    }
    for preset in BUILTIN_PRESETS {
        let target = normalize_model_id(preset.model);
        if target.is_empty() {
            continue;
        }
        // 「deepseekv4.1 and v4.1-1017」这种一行覆盖两个模型的写法，取双向包含。
        if active == target || active.contains(&target) || target.contains(&active) {
            return Some((to_detail(preset), PresetMatch::Exact));
        }
    }
    None
}

/// id 是否可安全用作单级目录名。
///
/// 预设 id 由同步脚本生成，形态固定为 `[a-z0-9-]`；这里显式校验是因为
/// `apply` 会把它拼进文件路径——外部传入的 id 不能信任，`..`、`/`、盘符
/// 任何一样漏过去都会变成任意路径写入。
fn is_safe_segment(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// 应用预设到某个目标。
///
/// - `StructuredTemplate`：改内存里的 `cfg`，由调用方负责落库；
/// - `SkillFile`：直接写盘，不碰 `cfg`。
pub fn apply(
    cfg: &mut PurgeSettings,
    id: &str,
    target: ApplyTarget,
    activate: bool,
) -> Result<AppliedPreset> {
    let preset = BUILTIN_PRESETS
        .iter()
        .find(|p| p.id == id)
        .with_context(|| format!("预设不存在：{id}"))?;

    if !is_safe_segment(preset.id) {
        bail!("预设 id 不可用作路径段：{}", preset.id);
    }

    match target {
        ApplyTarget::StructuredTemplate => {
            let name = preset.title.to_string();
            let text = preset.body.to_string();
            match cfg.structured_templates.iter_mut().find(|t| t.name == name) {
                Some(existing) => existing.text = text,
                None => cfg.structured_templates.push(super::PurgeTemplate {
                    name: name.clone(),
                    text,
                }),
            }
            if activate {
                cfg.active_template = name.clone();
            }
            Ok(AppliedPreset {
                target: "structured_template".to_string(),
                name,
                path: None,
            })
        }
        ApplyTarget::SkillFile => {
            let dir = expand_home(&cfg.skill_dir).join(preset.id);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建技能目录失败：{}", dir.display()))?;
            let file = dir.join("SKILL.md");
            std::fs::write(&file, preset.body)
                .with_context(|| format!("写入技能文件失败：{}", file.display()))?;
            Ok(AppliedPreset {
                target: "skill_file".to_string(),
                name: preset.title.to_string(),
                path: Some(file.to_string_lossy().to_string()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_not_empty_and_ids_unique() {
        assert!(!BUILTIN_PRESETS.is_empty());
        let mut ids: Vec<&str> = BUILTIN_PRESETS.iter().map(|p| p.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "预设 id 必须唯一");
    }

    #[test]
    fn catalog_sorted_by_vendor_then_model() {
        let mut sorted: Vec<(String, String)> = BUILTIN_PRESETS
            .iter()
            .map(|p| (p.vendor.to_lowercase(), p.model.to_lowercase()))
            .collect();
        let expected = sorted.clone();
        sorted.sort();
        assert_eq!(sorted, expected, "预设必须按 (vendor, model) 排序");
    }

    #[test]
    fn list_is_stable_across_calls() {
        let first = list(None, None, 10).0;
        let second = list(None, None, 10).0;
        assert_eq!(
            first.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            second.iter().map(|p| p.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn list_limit_is_clamped() {
        let (items, _, total) = list(None, None, 0);
        assert_eq!(items.len(), 1, "limit=0 应被 clamp 到 1");
        assert!(total >= items.len());

        let (items, _, _) = list(None, None, MAX_LIMIT + 1000);
        assert!(items.len() <= MAX_LIMIT);
    }

    #[test]
    fn list_total_counts_all_matches_not_just_page() {
        let (items, _, total) = list(None, None, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(total, BUILTIN_PRESETS.len(), "total 应是过滤后全量");
    }

    #[test]
    fn list_filters_by_vendor() {
        let vendor = BUILTIN_PRESETS[0].vendor;
        let (items, vendors, total) = list(None, Some(vendor), MAX_LIMIT);
        assert!(items.iter().all(|p| p.vendor == vendor));
        assert_eq!(vendors.len(), 1);
        assert_eq!(vendors[0].count, total);
    }

    #[test]
    fn list_filters_by_query() {
        let target = &BUILTIN_PRESETS[0];
        let (items, _, _) = list(Some(target.id), None, MAX_LIMIT);
        assert!(items.iter().any(|p| p.id == target.id));
        assert!(items.iter().all(|p| p.id.contains(target.id)
            || p.vendor.to_lowercase().contains(target.id)
            || p.model.to_lowercase().contains(target.id)
            || p.title.to_lowercase().contains(target.id)
            || p.tags.iter().any(|t| t.contains(target.id))));
    }

    #[test]
    fn list_query_with_no_match_returns_empty() {
        let (items, vendors, total) = list(Some("zzz-不存在的预设-zzz"), None, 10);
        assert!(items.is_empty());
        assert!(vendors.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn preview_never_splits_multibyte_chars() {
        for preset in BUILTIN_PRESETS.iter().take(10) {
            let brief = to_brief(preset);
            // 构造本身就会 panic 的话这个测试直接失败；再校验长度约束。
            assert!(brief.preview.chars().count() <= PREVIEW_CHARS + 1);
        }
        // 纯中文构造：按字节切会 panic，按字符切不会。
        let chinese = "破甲".repeat(300);
        let preview = preview_of(&chinese);
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
    }

    #[test]
    fn get_returns_detail_and_none_for_unknown() {
        let target = &BUILTIN_PRESETS[0];
        let detail = get(target.id).expect("应能取到详情");
        assert_eq!(detail.id, target.id);
        assert_eq!(detail.body, target.body);
        assert!(get("no-such-preset").is_none());
    }

    #[test]
    fn sha256_matches_body() {
        for preset in BUILTIN_PRESETS.iter().take(3) {
            assert_eq!(preset.sha256.len(), 64, "sha256 应是 64 位十六进制");
            assert!(
                preset.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "sha256 应只含十六进制字符"
            );
            assert_eq!(preset.bytes, preset.body.len(), "bytes 应是正文字节数");
        }
    }

    #[test]
    fn apply_structured_template_upserts_and_activates() {
        let mut cfg = PurgeSettings::default();
        let target = &BUILTIN_PRESETS[0];

        let applied =
            apply(&mut cfg, target.id, ApplyTarget::StructuredTemplate, true).expect("应用应成功");
        assert_eq!(applied.target, "structured_template");
        assert_eq!(applied.name, target.title);
        assert!(applied.path.is_none());
        assert_eq!(cfg.active_template, target.title);
        assert!(cfg
            .structured_templates
            .iter()
            .any(|t| t.name == target.title && t.text == target.body));

        // 再应用一次不应产生重复条目（同名 upsert）。
        let before = cfg.structured_templates.len();
        apply(&mut cfg, target.id, ApplyTarget::StructuredTemplate, false).expect("重复应用应成功");
        assert_eq!(cfg.structured_templates.len(), before, "同名应覆盖而非追加");
    }

    #[test]
    fn apply_without_activate_keeps_active_template() {
        let mut cfg = PurgeSettings {
            active_template: "已有模板".to_string(),
            ..Default::default()
        };
        let target = &BUILTIN_PRESETS[0];
        apply(&mut cfg, target.id, ApplyTarget::StructuredTemplate, false).expect("应用应成功");
        assert_eq!(cfg.active_template, "已有模板");
    }

    #[test]
    fn apply_skill_file_writes_into_skill_dir() {
        let dir = std::env::temp_dir().join(format!("aigx-purge-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut cfg = PurgeSettings {
            skill_dir: dir.to_string_lossy().to_string(),
            ..Default::default()
        };
        let templates_before = cfg.structured_templates.len();
        let active_before = cfg.active_template.clone();
        let target = &BUILTIN_PRESETS[0];

        let applied =
            apply(&mut cfg, target.id, ApplyTarget::SkillFile, false).expect("写盘应成功");
        assert_eq!(applied.target, "skill_file");
        let path = std::path::PathBuf::from(applied.path.expect("skill_file 应返回路径"));
        assert!(path.ends_with("SKILL.md"));
        assert_eq!(path.parent().unwrap().file_name().unwrap(), target.id);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), target.body);

        // 写技能文件不应改动配置（默认已带 6 个内置模板，所以比对数量与激活值）。
        assert_eq!(cfg.structured_templates.len(), templates_before);
        assert_eq!(cfg.active_template, active_before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rejects_unknown_id() {
        let mut cfg = PurgeSettings::default();
        assert!(apply(
            &mut cfg,
            "no-such-preset",
            ApplyTarget::StructuredTemplate,
            false
        )
        .is_err());
        assert!(apply(&mut cfg, "../etc/passwd", ApplyTarget::SkillFile, false).is_err());
        assert!(apply(&mut cfg, "C:/windows", ApplyTarget::SkillFile, false).is_err());
    }

    #[test]
    fn safe_segment_rules() {
        assert!(is_safe_segment("anthropic-opus-v1"));
        assert!(is_safe_segment("0x8d-jailbreaks-gpt-new"));
        assert!(!is_safe_segment(""));
        assert!(!is_safe_segment(".."));
        assert!(!is_safe_segment("../x"));
        assert!(!is_safe_segment("a/b"));
        assert!(!is_safe_segment("-lead"));
        assert!(!is_safe_segment("trail-"));
        assert!(!is_safe_segment("UPPER"));
        assert!(!is_safe_segment("with space"));
    }

    // ── 按模型找预设 ──────────────────────────────────────────

    #[test]
    fn normalize_folds_case_and_separators() {
        assert_eq!(normalize_model_id("DeepSeek V4"), "deepseekv4");
        assert_eq!(normalize_model_id("deepseek-v4"), "deepseekv4");
        assert_eq!(normalize_model_id("deepseek_v4"), "deepseekv4");
        assert_eq!(normalize_model_id("deepseek/v4"), "deepseekv4");
        assert_eq!(normalize_model_id("   "), "");
    }

    #[test]
    fn preset_for_model_hits_the_same_name_and_never_guesses() {
        let target = &BUILTIN_PRESETS[0];
        let hit = preset_for_model(target.model, "").expect("同名模型应当命中");
        assert_eq!(hit.0.id, target.id);
        assert_eq!(hit.1, PresetMatch::Exact);

        // 大小写/分隔符差异仍算同一个模型。
        let spaced = target.model.replace('-', " ");
        assert_eq!(
            preset_for_model(&spaced, "").map(|(detail, _)| detail.id),
            Some(target.id.to_string())
        );

        // 完全对不上的模型：不猜。
        assert!(preset_for_model("zzz-unknown-model-9", "").is_none());
        // 空模型：不猜。
        assert!(preset_for_model("   ", "").is_none());
    }

    #[test]
    fn a_pinned_id_wins_over_model_matching() {
        let pinned = &BUILTIN_PRESETS[1];
        let hit = preset_for_model("zzz-unknown-model-9", pinned.id).expect("固定 id 应当命中");
        assert_eq!(hit.0.id, pinned.id);
        assert_eq!(hit.1, PresetMatch::Pinned);

        // 死 id：什么都不套，也不回落到模型匹配。
        assert!(preset_for_model(BUILTIN_PRESETS[0].model, "no-such-preset").is_none());
        // 空白 id 等于没填：回到同名匹配。
        assert_eq!(
            preset_for_model(BUILTIN_PRESETS[0].model, "   ").map(|(_, kind)| kind),
            Some(PresetMatch::Exact)
        );
    }
}
