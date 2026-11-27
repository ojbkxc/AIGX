//! 破甲控制台（PurgeConsole）**只读**诊断工具 —— 让 AI 运维「先体检、再改」。
//!
//! ## 为什么需要这一对工具
//!
//! 破甲配置的读写能力本来就有（`aigx_config_view` / `aigx_config_set` 的 `purge` key），
//! 但只给读写等于让模型**盲改**：`PurgeSettings` 有二十多个字段，每张词表都会改变
//! 所有经过的请求，而「改了到底会不会生效」全靠人眼 —— 异源递进开着但
//! `fallback_models` 为空、技能注入开着但目录里扫不到 `SKILL.md`、异源触发轮次大于
//! 重试上限，这些正是 `crate::purge::config_warning_items` 能静态判定、但 AI 面拿不到
//! 结论的那一类（管理面 GET 里有，模型还得自己从几十个字段里推）。
//!
//! 本文件补两条**只读**工具，把「体检结论」与「预设命中判定」变成可直接消费的读数：
//!
//! - `aigx_purge_check`：体检（`config_warning_items` + 预设命中分布 + 技能目录扫描 +
//!   关键字段现值）。回答「现在这套配置有哪些项目开了却不生效」「196 个模型里多少能
//!   套上预设」「技能注入到底有没有可命中的文件」。
//! - `aigx_purge_preset_probe`：给定一个模型名，回答「会命中哪一档预设、命中依据是
//!   exact/family/universal/pinned 中的哪一档、最终会不会真把正文前插」。
//!
//! ## 为什么只读、为什么这些端点不碰
//!
//! - 写侧仍只有 `aigx_config_set`（`HighRisk`，进审批矩阵）。诊断工具是 `ReadOnly`：
//!   **不能顺手改配置** —— 「体检查出问题就自动修」会把「高危写必须人工批」这条
//!   审批门绕成装饰。
//! - `/api/purge/presets/:id/apply`（按 id 往技能目录写 `SKILL.md`）与
//!   `/api/purge/skills/import`（解压归档落盘）有**磁盘副作用**，`tests.rs` 的
//!   `EXEMPT_PREFIXES` 已写明不向 AI 面开放；`/api/purge/preview` 是同页保存前预演，
//!   同样保持豁免。本文件**只声明 `covers = ["/api/purge"]`**，一个副作用端点都不覆盖，
//!   那些既有决定逐条保留（`wc_purge_tools_touch_no_side_effect_route` 钉住）。
//! - 数据面行为零改动：只读 `state.purge.snapshot()`（快照）与编译期预设目录，
//!   既不调 `apply_auto_preset`，也不碰 `config_warnings` 的判定逻辑 —— 判定留在
//!   `crate::purge`，这里只搬运它的结论。

use super::*;
use crate::purge::presets::{self, PresetMatch};
use crate::purge::{PurgeSettings, Severity, SkillBrief};
use RiskLevel::ReadOnly;

/// 体检里回给模型的「无预设可套」样例上限（全量可能在百条量级，逐条列会撑爆上下文）。
const UNCOVERED_SAMPLE_MAX: usize = 12;

/// 技能扫描条目上限（同上：目录可能有几十个技能，正文大小不重要，名字才是线索）。
const SKILL_SAMPLE_MAX: usize = 20;

pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        td(
            "aigx_purge_check",
            "破甲配置体检",
            "只读：破甲控制台（/api/purge）体检读数 —— 先看再改的那一步，**不写任何东西**。\
             返回 ① `settings_digest`：当前生效的关键字段现值（总开关/各治理开关/两张词表条数与启用条数/\
             异源模型列表/技能目录/结构化模板/预设开关与固定 id/三张字面词表条数）；\
             ② `warnings`：`config_warning_items` 的原样结论（code + severity + 中文说明），\
             专指「开了却不生效 / 配置自相矛盾」（异源开但无候选模型、触发轮次大于重试上限、\
             技能注入开但目录无 SKILL.md、结构化提示开但生效文本为空、改写规则链式二次改写…）；\
             ③ `presets`：启用模型里**能套上预设**的分布（exact/family/universal/none）、\
             真正会前插正文的模型数 `would_apply`、无预设可套的样例 `uncovered`；\
             ④ `skills`：技能目录的展开后绝对路径 + 扫描到的 SKILL.md（名称/关键词/正文字符数）。\
             **用法（先读再改的固定顺序）**：本工具 → 判断要改哪个字段 → `aigx_config_view{key:\"purge\"}` \
             读整份配置 → `aigx_config_set{key:\"purge\"}` 写回**整份对象** → 再调本工具确认 warnings 与命中率的变化。\
             单模型命中判定用 `aigx_purge_preset_probe`。有副作用的端点（预设 apply、技能包导入）不在工具体面。",
            ReadOnly,
            "",
            &["/api/purge"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    require_admin(s, h).await?;
                    let settings = s.purge.snapshot();
                    // 走管理面同一个视图构造函数（与 GET /api/models/manage 同源），
                    // 不另写一份「启用模型」判定 —— 命中率说的必须是线上真在跑的这批量。
                    let view = admin::model_manage::current_view(s);
                    let models: Vec<(String, bool)> = view
                        .models
                        .iter()
                        .map(|m| (m.id.clone(), m.enabled))
                        .collect();
                    // 体检项必须与**页面同一个判据**：`config_warning_items_with` 比静态版
                    // 多四条「只有对照模型清单才判得出」的检查（异源未定价/同厂商/无承载/
                    // 不在清单）。少了这一层，AI 会说「没有告警」而页面明明有四条 —— 那正是
                    // 「两处口径不一致」这个老毛病的新形态。
                    // `main_model` 的取法与 `api/admin/purge.rs::main_model_of` 逐句同源：
                    // 先设置页的默认模型，空则用模型管理视角的首选推理模型。
                    let mut main_model = crate::config::default_model();
                    if main_model.trim().is_empty() {
                        main_model = crate::model::manage::pick_default_model(&view).unwrap_or_default();
                    }
                    let warnings =
                        crate::purge::config_warning_items_with(&settings, Some(&view), &main_model);
                    // `fresh=false`：诊断不该把技能扫描的 30 秒缓存打穿（复扫描是页面
                    // 「重新扫描」按钮的语义）。目录扫描失败/为空都由 warnings 侧表达。
                    let skills = crate::purge::skill_briefs_with(&settings.skill_dir, false);
                    out(Ok(Json(purge_report(
                        settings.as_ref(),
                        &models,
                        &skills,
                        warnings,
                    ))))
                })
            },
        ),
        td(
            "aigx_purge_preset_probe",
            "破甲预设命中判定",
            "只读：给定一个模型名，回答破甲「按模型自动套预设」会怎么判 —— 不写配置、不改统计。\
             返回 ① `normalized`：模型名折叠大小写与分隔符后的归一化形态（判定实际比的就是它）；\
             ② `layers`：逐档依据 —— `exact_or_family`（关掉通用兜底时的判定：同名模型 exact / \
             同厂商族 family）与 `with_universal`（再放行通用兜底 universal），null = 该档无预设；\
             ③ `pinned`：当前配置里 `auto_preset_id` 是否点名了固定预设（点名则任何模型都套它，\
             优先级高于同名/同族/通用三档）；④ `effective`：**最终生效的那一份**（id/vendor/title/\
             字节数与正文字符数）与命中档位；⑤ `would_apply` + `gates`：真正会不会前插正文 \
             —— 需要总开关 enabled ∧ auto_preset 两个开关都开，且命中预设正文非空。\
             名字对不上时**坚决不套**（虚拟路由名 auto/default 也不套）——这是库的设计，不是缺陷；\
             要覆盖「库内没有内容的厂商」用 `universal_fallback`，代价见 #aigx_purge_check 的读数。",
            ReadOnly,
            "model",
            &["/api/purge"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "model": {
                            "type": "string",
                            "description": "要判定的模型名（请求里出现的那个形态，如 claude-sonnet-4-6 / glm-5.3 / deepseek-v4）"
                        }
                    },
                    "required": ["model"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    require_admin(s, h).await?;
                    let model = require_arg(a, "model")?;
                    let settings = s.purge.snapshot();
                    out(Ok(Json(preset_probe(
                        &model,
                        settings.as_ref(),
                    ))))
                })
            },
        ),
    ]
}

/// 管理面校验（诊断工具直读进程内状态，因此自己带这一道闸，而不是依赖某个 handler 内部校验）。
///
/// 与 handler 面的错误口径对齐：非管理员 → JSON-RPC `-32603` + 原样消息
/// （`exec_tool` 的调用方按 `ok:false` + 文本呈现，不需要 HTTP 状态码）。
async fn require_admin(s: &AppState, h: &HeaderMap) -> Result<(), (i64, String)> {
    admin::common::verify_admin(s, h)
        .await
        .map(|_| ())
        .map_err(|(_, Json(body))| {
            let msg = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("admin required")
                .to_string();
            (-32603, msg)
        })
}

/// `PresetMatch` → 对外档位名（与 `GET /api/purge/presets?model=` 的 `match` 字段同一套词）。
fn match_label(match_kind: PresetMatch) -> &'static str {
    match match_kind {
        PresetMatch::Exact => "exact",
        PresetMatch::Family => "family",
        PresetMatch::Universal => "universal",
        PresetMatch::Pinned => "pinned",
    }
}

/// 预设命中摘要（一条预设 → 给模型的形状；**不带正文**，6KB 正文进上下文毫无用处）。
fn preset_brief(detail: &presets::PresetDetail, match_kind: PresetMatch) -> Value {
    json!({
        "id": detail.id,
        "match": match_label(match_kind),
        "vendor": detail.vendor,
        "title": detail.title,
        "bytes": detail.bytes,
        "body_chars": detail.body.chars().count(),
    })
}

/// 启用模型上的预设命中分布 + 「真正会前插」的模型数。
///
/// 判据全部复用 `presets::preset_for_model`（数据面写路径同一个函数、同一份参数），
/// 不在这里另写一套匹配 —— 否则「列表说能套、实际不套」会以第二种形态回来。
///
/// `models` 是 `(模型 id, 是否启用)`：**只统计启用模型**（禁用模型不过数据面，
/// 把它算进分母会让命中率虚低）。
fn preset_hit_rates(models: &[(String, bool)], settings: &PurgeSettings) -> Value {
    let pinned = settings.auto_preset_id.trim();
    let allow_universal = settings.universal_fallback;
    let enabled: Vec<&str> = models
        .iter()
        .filter(|(_, on)| *on)
        .map(|(id, _)| id.as_str())
        .collect();

    let mut exact = 0usize;
    let mut family = 0usize;
    let mut universal = 0usize;
    let mut none = 0usize;
    let mut would_apply = 0usize;
    let mut uncovered: Vec<String> = Vec::new();

    for model in &enabled {
        // `pinned` 传空：这一层要回答的是「库对这批模型本身的覆盖度」，
        // 固定 id 会把所有模型都盖成 pinned，让覆盖度看起来永远 100%。
        match presets::preset_for_model(model, "", allow_universal) {
            // 各档只计数，不解构正文（`_` 不绑定变量名——绑定不用的名字会触发
            // `unused_variables`，而 CI 是 `-D warnings`）。
            Some((_, PresetMatch::Exact)) => exact += 1,
            Some((_, PresetMatch::Family)) => family += 1,
            Some((_, PresetMatch::Universal)) => universal += 1,
            Some((_, PresetMatch::Pinned)) => {
                // 传空 pinned 时不会出现；留着是为了 match 穷尽（新增档位必须在此显式处置）
                exact += 1;
            }
            None => {
                none += 1;
                if uncovered.len() < UNCOVERED_SAMPLE_MAX {
                    uncovered.push((*model).to_string());
                }
            }
        }
        // 「会不会真前插」与数据面同一条前置链：总开关 ∧ auto_preset ∧ 命中 ∧ 正文非空。
        // 这里用真实 pinned 再判一次（与上面刻意不同，见注释）。
        // 写成「一个 if + is_some_and」而不是嵌套 if/if-let：后者会被 clippy
        // `collapsible_if` 盯上（CI 是 `-D warnings`）。
        if settings.enabled && settings.auto_preset {
            let hit = presets::preset_for_model(model, pinned, allow_universal);
            if hit.is_some_and(|(detail, _)| !detail.body.trim().is_empty()) {
                would_apply += 1;
            }
        }
    }

    json!({
        "models_total": models.len(),
        "models_enabled": enabled.len(),
        "auto_match": {
            "exact": exact,
            "family": family,
            "universal": universal,
            "none": none,
        },
        "would_apply": would_apply,
        "uncovered": uncovered,
        "pinned": {
            "id": pinned,
            "active": !pinned.is_empty() && presets::get(pinned).is_some(),
        },
        "gates": {
            "enabled": settings.enabled,
            "auto_preset": settings.auto_preset,
            "universal_fallback": allow_universal,
        },
    })
}

/// 技能目录扫描摘要：`skill_inject` 的可验证性（开了却扫不到 = 永远不命中）。
fn skills_summary(settings: &PurgeSettings, skills: &[SkillBrief]) -> Value {
    let items: Vec<Value> = skills
        .iter()
        .take(SKILL_SAMPLE_MAX)
        .map(|s| {
            json!({
                "name": s.name,
                "keywords": s.keywords,
                "body_chars": s.body_chars,
            })
        })
        .collect();
    json!({
        "dir": settings.skill_dir,
        "resolved": crate::purge::expanded_skill_dir(&settings.skill_dir),
        "inject": settings.skill_inject,
        "inject_limit": settings.skill_inject_limit,
        "count": skills.len(),
        "items": items,
        // 开了但扫不到任何 SKILL.md：关键词永远不命中。这句给模型的话必须是人话，
        // 因为它会直接进「要不要改 skill_dir」的决策。
        "scan_warning": if settings.skill_inject && skills.is_empty() {
            Some("技能注入开着，但该目录里扫不到任何 SKILL.md：关键词永远不命中。放好文件或改 skill_dir（放好后需要在控制台点「重新扫描」清缓存）。")
        } else {
            None
        },
    })
}

/// 关键字段现值（**改前对照用的镜子**：写回是整份替换，漏字段会回落默认）。
fn settings_digest(cfg: &PurgeSettings) -> Value {
    json!({
        "enabled": cfg.enabled,
        "system_normalize": cfg.system_normalize,
        "dead_zone_rewrite": cfg.dead_zone_rewrite,
        "rewrite_rules": cfg.rewrite_rules.len(),
        "rewrite_rules_enabled": cfg.rewrite_rules.iter().filter(|r| r.enabled).count(),
        "synonym_enabled": cfg.synonym_enabled,
        "synonym_pairs": cfg.synonym_pairs.len(),
        "normalize_strip_prefixes": cfg.normalize_strip_prefixes,
        "reduce_reasoning": cfg.reduce_reasoning,
        "reasoning_floor": cfg.reasoning_floor,
        "cross_source_fallback": cfg.cross_source_fallback,
        "fallback_trigger_attempt": cfg.fallback_trigger_attempt,
        "fallback_models": cfg.fallback_models,
        "retry_limit": cfg.retry_limit,
        "skill_inject": cfg.skill_inject,
        "skill_dir": cfg.skill_dir,
        "skill_inject_limit": cfg.skill_inject_limit,
        "structured_prompt": cfg.structured_prompt,
        "active_template": cfg.active_template,
        "structured_suffix_chars": cfg.structured_suffix.chars().count(),
        "structured_templates": cfg.structured_templates.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        "auto_preset": cfg.auto_preset,
        "auto_preset_id": cfg.auto_preset_id,
        "universal_fallback": cfg.universal_fallback,
        "refusal_patterns": cfg.refusal_patterns.len(),
        "failure_filter_keywords": cfg.failure_filter_keywords.len(),
        "failure_empty_keywords": cfg.failure_empty_keywords.len(),
        "upstream_label": cfg.upstream_label,
    })
}

/// `aigx_purge_check` 的结果体（纯函数：给定快照 + 模型集 + 技能扫描 + 体检项，产出读数）。
///
/// 纯函数化的理由：执行体会碰 `AppState`（单测不构造），而这里的每条判据都值得被
/// 逐条钉住（尤其「总开关关闭时体检项不算」这种边界）。
///
/// `warnings` 由调用方传入（`config_warning_items_with` 带模型视图的版本）：判据住在
/// `crate::purge`，本文件只搬运结论 —— 这样页面与 AI 读数永远同一份，不会各判一套。
fn purge_report(
    cfg: &PurgeSettings,
    models: &[(String, bool)],
    skills: &[SkillBrief],
    warnings: Vec<crate::purge::ConfigWarning>,
) -> Value {
    let mut warnings: Vec<Value> = warnings
        .into_iter()
        .map(|item| {
            json!({
                "code": item.code,
                // severity 由 code 纯函数推导（不存第二份）；`Severity` 本身 Serialize
                "severity": crate::purge::severity_of(item.code),
                "message": item.message,
            })
        })
        .collect();

    // 总开关关闭时 `config_warning_items` 直接早退（其余检查无意义），
    // 但「破甲整个关了」这件事本身就是最该报出来的结论 —— 补一条合成项，
    // 免得模型看到 warnings 为空就以为「一切正常」。
    if !cfg.enabled {
        warnings.insert(
            0,
            json!({
                "code": "purge_disabled",
                "severity": Severity::Warn,
                "message": "破甲总开关关闭：数据面不做任何治理（不判定、不改写、不重试、不套预设）；此状态下其余体检项不参与计算。",
            }),
        );
    }

    json!({
        "success": true,
        "data": {
            "settings_digest": settings_digest(cfg),
            "warnings": warnings,
            "presets": preset_hit_rates(models, cfg),
            "skills": skills_summary(cfg, skills),
        }
    })
}

/// `aigx_purge_preset_probe` 的结果体（纯函数，便于逐档钉死判定依据）。
fn preset_probe(model: &str, cfg: &PurgeSettings) -> Value {
    let pinned = cfg.auto_preset_id.trim();
    let allow_universal = cfg.universal_fallback;

    // 逐档求值：同一个 `preset_for_model`，只是把开关调成「只看前两档」与「放行通用兜底」。
    // 这样模型看到的依据是**函数真判出来的**，而不是本文件对库的猜测。
    let narrow = presets::preset_for_model(model, "", false);
    let wide = presets::preset_for_model(model, "", allow_universal);
    let effective = presets::preset_for_model(model, pinned, allow_universal);

    let narrow_json = narrow
        .as_ref()
        .map(|(detail, kind)| preset_brief(detail, *kind))
        .unwrap_or(Value::Null);
    let wide_json = wide
        .as_ref()
        .map(|(detail, kind)| preset_brief(detail, *kind))
        .unwrap_or(Value::Null);
    let effective_json = effective
        .as_ref()
        .map(|(detail, kind)| preset_brief(detail, *kind))
        .unwrap_or(Value::Null);

    let body_not_empty = effective
        .as_ref()
        .is_some_and(|(detail, _)| !detail.body.trim().is_empty());
    let would_apply = cfg.enabled && cfg.auto_preset && body_not_empty;

    json!({
        "success": true,
        "data": {
            "model": model,
            "normalized": presets::normalize_model_id(model),
            "layers": {
                "exact_or_family": narrow_json,
                "with_universal": wide_json,
            },
            "pinned": {
                "id": pinned,
                "configured": !pinned.is_empty(),
                "exists": !pinned.is_empty() && presets::get(pinned).is_some(),
            },
            "effective": effective_json,
            "would_apply": would_apply,
            "gates": {
                "enabled": cfg.enabled,
                "auto_preset": cfg.auto_preset,
                "body_not_empty": body_not_empty,
                "universal_fallback": allow_universal,
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一条库内真实存在的模型名（用来验证「库里的模型至少能命中一档」）。
    ///
    /// 运行时从编译期目录里取，不硬编码 —— 目录由 `tools/sync_purge_presets.py` 重新生成，
    /// 硬编码会在下次同步后变成假红。
    fn real_preset_model() -> String {
        crate::purge::preset_catalog::BUILTIN_PRESETS
            .iter()
            .find(|p| {
                p.id != presets::UNIVERSAL_PRESET_ID && !p.model.trim().is_empty() && p.model != "*"
            })
            .expect("内置预设目录里应至少有一条非通用条目")
            .model
            .to_string()
    }

    /// 「列」侧：两条工具只读、覆盖且只覆盖 `/api/purge`，有副作用的端点一个都不碰。
    ///
    /// 后半句是**保留既有决定**的机器化落点：`tests.rs` 的 `EXEMPT_PREFIXES` 已写明
    /// `/api/purge/presets`（含 `:id/apply`）、`/api/purge/skills/import`、
    /// `/api/purge/preview` 不向 AI 面开放；任何「顺手把它们包进来」的改动在这里变红。
    #[test]
    fn wc_purge_tools_touch_no_side_effect_route() {
        for name in ["aigx_purge_check", "aigx_purge_preset_probe"] {
            let t = find_tool(name).unwrap_or_else(|| panic!("{name} 未注册"));
            assert_eq!(t.risk, RiskLevel::ReadOnly, "诊断工具必须只读");
            assert!(
                t.description.contains("只读"),
                "{name} 描述须标注「只读」风险语义"
            );
            assert_eq!(t.covers, ["/api/purge"], "{name} 只允许覆盖 /api/purge");
        }
        // 全量扫一遍：任何工具都不许把破甲的副作用端点包进 covers（不是只盯本文件两条）。
        for t in all_tools() {
            for c in t.covers {
                for banned in [
                    "/api/purge/presets",
                    "/api/purge/skills/import",
                    "/api/purge/preview",
                    "/api/purge/stats/reset",
                ] {
                    assert!(
                        !c.starts_with(banned),
                        "工具 {} 覆盖了破甲副作用端点 `{c}`（`{banned}` 已声明不向 AI 面开放）",
                        t.name
                    );
                }
            }
        }
    }

    /// 命中率分母只算启用模型，且四档之和 == 启用数（新增档位漏统计会在这里红）。
    #[test]
    fn wc_hit_rates_count_every_enabled_model_once() {
        let real = real_preset_model();
        let models: Vec<(String, bool)> = vec![
            (real.clone(), true),
            ("totally-unknown-model-zzz".to_string(), true),
            ("disabled-model-should-not-count".to_string(), false),
        ];
        // 结构体更新语法而不是 `default() + 逐字段赋值`：后者会撞 clippy
        // `field_reassign_with_default`，而 CI 是 `--all-targets -D warnings`（测试也含在内）。
        let mut cfg = PurgeSettings {
            universal_fallback: false,
            ..PurgeSettings::default()
        };
        let v = preset_hit_rates(&models, &cfg);
        assert_eq!(v["models_total"], json!(3));
        assert_eq!(v["models_enabled"], json!(2), "禁用模型不计入分母");
        let m = &v["auto_match"];
        let sum = m["exact"].as_u64().unwrap() as usize
            + m["family"].as_u64().unwrap() as usize
            + m["universal"].as_u64().unwrap() as usize
            + m["none"].as_u64().unwrap() as usize;
        assert_eq!(sum, 2, "四档之和必须等于启用模型数: {m}");
        assert_eq!(
            m["none"].as_u64(),
            Some(1),
            "关掉通用兜底时，库里没有内容的模型必须落在 none（不能凭空套）"
        );
        assert_eq!(m["universal"].as_u64(), Some(0));
        assert_eq!(
            v["uncovered"],
            json!(["totally-unknown-model-zzz"]),
            "无预设可套的样例要报出来（这才是可操作的那一半）"
        );
        // 库内真实模型名至少命中前两档之一（exact/family），证明命中判定真的在跑。
        assert!(
            m["exact"].as_u64().unwrap() + m["family"].as_u64().unwrap() >= 1,
            "库内模型名 `{real}` 应至少命中 exact/family 之一"
        );

        // 打开通用兜底：同一条未知模型落到 universal，none 归零。
        cfg.universal_fallback = true;
        let v2 = preset_hit_rates(&models, &cfg);
        assert_eq!(v2["auto_match"]["universal"].as_u64(), Some(1));
        assert_eq!(v2["auto_match"]["none"].as_u64(), Some(0));
        assert!(v2["uncovered"].as_array().unwrap().is_empty());
    }

    /// `would_apply` 与数据面同一条前置链：总开关 / auto_preset 任一关闭即 0。
    #[test]
    fn wc_would_apply_follows_data_plane_gates() {
        let models: Vec<(String, bool)> = vec![("claude-sonnet-4-6".to_string(), true)];
        let mut cfg = PurgeSettings {
            enabled: true,
            auto_preset: true,
            ..PurgeSettings::default()
        };
        assert_eq!(
            preset_hit_rates(&models, &cfg)["would_apply"].as_u64(),
            Some(1),
            "两个开关都开且命中 → 会前插"
        );
        cfg.auto_preset = false;
        assert_eq!(
            preset_hit_rates(&models, &cfg)["would_apply"].as_u64(),
            Some(0),
            "auto_preset 关闭 → 一份都不前插"
        );
        cfg.auto_preset = true;
        cfg.enabled = false;
        assert_eq!(
            preset_hit_rates(&models, &cfg)["would_apply"].as_u64(),
            Some(0),
            "总开关关闭 → 数据面零治理"
        );
    }

    /// 总开关关闭时必须显式报出来（`config_warning_items` 会早退返回空，最容易被读成「全部正常」）。
    #[test]
    fn wc_report_flags_disabled_switch_instead_of_empty_health() {
        let cfg = PurgeSettings {
            enabled: false,
            ..PurgeSettings::default()
        };
        // 总开关关闭时管理面 `config_warning_items_with` 也会早退 → 空表，
        // 这里刻意传空表，验证「合成一条 purge_disabled」不依赖上游是否给了项。
        let v = purge_report(&cfg, &[], &[], Vec::new());
        assert_eq!(v["data"]["settings_digest"]["enabled"], json!(false));
        let codes: Vec<&str> = v["data"]["warnings"]
            .as_array()
            .expect("warnings 数组")
            .iter()
            .map(|w| w["code"].as_str().unwrap_or_default())
            .collect();
        assert!(
            codes.contains(&"purge_disabled"),
            "总开关关闭必须单独报一条，实际: {codes:?}"
        );
    }

    /// 体检项直接搬 `config_warning_items` 的结论（不另写一套判据，也不漏项）。
    #[test]
    fn wc_report_carries_config_warning_items_verbatim() {
        let cfg = PurgeSettings {
            cross_source_fallback: true,
            fallback_models: Vec::new(),
            ..PurgeSettings::default()
        };
        // 体检项只搬运上游结论：这里用**带视图**的那一版（与页面 GET 同源），
        // 并逐条对上 code 清单 —— 若本文件偷偷自己判一套，这条会立刻红。
        let upstream = crate::purge::config_warning_items_with(&cfg, None, "");
        let expected: Vec<String> = upstream.iter().map(|w| w.code.to_string()).collect();
        assert!(
            !expected.is_empty(),
            "该形态应至少产一条体检项（前置条件失效）"
        );
        let v = purge_report(&cfg, &[], &[], upstream);
        let got: Vec<String> = v["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(got, expected, "体检项必须与上游逐条一致");
        // severity 也必须带上（前端/模型都按级排序，缺字段等于降级成同等严重）
        assert!(v["data"]["warnings"][0]["severity"].as_str().is_some());
    }

    /// 技能注入开着但扫不到文件：`scan_warning` 必须给出可执行的下一句。
    #[test]
    fn wc_skills_summary_warns_when_inject_has_nothing_to_hit() {
        let mut cfg = PurgeSettings {
            skill_inject: true,
            ..PurgeSettings::default()
        };
        let v = skills_summary(&cfg, &[]);
        assert_eq!(v["count"], json!(0));
        assert!(
            v["scan_warning"]
                .as_str()
                .unwrap_or_default()
                .contains("重新扫描"),
            "空目录告警要说清下一步（含清缓存动作）"
        );
        // 未开技能注入：不给这句噪音告警
        cfg.skill_inject = false;
        assert!(
            skills_summary(&cfg, &[]).is_null()
                || skills_summary(&cfg, &[])["scan_warning"].is_null()
        );
        // 扫描到文件后不再告警
        let one = vec![SkillBrief {
            name: "demo".to_string(),
            keywords: vec!["kw".to_string()],
            body_chars: 10,
        }];
        cfg.skill_inject = true;
        assert!(skills_summary(&cfg, &one)["scan_warning"].is_null());
    }

    /// 逐档依据：关掉通用兜底时未知模型两档皆空；打开后 wide 命中 universal 而 narrow 仍空。
    #[test]
    fn wc_probe_explains_each_layer() {
        let cfg = PurgeSettings {
            universal_fallback: false,
            ..PurgeSettings::default()
        };
        let v = preset_probe("totally-unknown-model-zzz", &cfg);
        assert!(v["data"]["layers"]["exact_or_family"].is_null());
        assert!(
            v["data"]["layers"]["with_universal"].is_null(),
            "兜底关着不应有 wide 命中"
        );
        assert!(v["data"]["effective"].is_null());
        assert_eq!(v["data"]["would_apply"], json!(false));
        assert_eq!(v["data"]["normalized"], json!("totallyunknownmodelzzz"));

        let cfg = PurgeSettings {
            universal_fallback: true,
            ..PurgeSettings::default()
        };
        let v2 = preset_probe("totally-unknown-model-zzz", &cfg);
        assert!(
            v2["data"]["layers"]["exact_or_family"].is_null(),
            "前两档不因兜底开关而变"
        );
        assert_eq!(
            v2["data"]["layers"]["with_universal"]["match"].as_str(),
            Some("universal"),
            "打开兜底后 wide 档必须落 universal"
        );
        assert_eq!(v2["data"]["effective"]["match"].as_str(), Some("universal"));
        assert_eq!(v2["data"]["would_apply"], json!(true));
    }

    /// 固定 id 优先：pinned 命中后 effective 档位是 pinned，且与模型名无关。
    #[test]
    fn wc_probe_pinned_overrides_model_match() {
        // 用一条库内真实 id，确保 `exists=true`（硬编码 id 会在下次目录同步后变假红）
        let id = crate::purge::preset_catalog::BUILTIN_PRESETS
            .iter()
            .find(|p| p.id != presets::UNIVERSAL_PRESET_ID)
            .unwrap()
            .id
            .to_string();
        let cfg = PurgeSettings {
            auto_preset_id: id.clone(),
            ..PurgeSettings::default()
        };
        let v = preset_probe("claude-sonnet-4-6", &cfg);
        assert_eq!(v["data"]["pinned"]["id"], json!(id));
        assert_eq!(v["data"]["pinned"]["exists"], json!(true));
        assert_eq!(
            v["data"]["effective"]["match"].as_str(),
            Some("pinned"),
            "点名固定预设时任何模型都套它"
        );
        assert_eq!(v["data"]["effective"]["id"].as_str(), Some(id.as_str()));
    }

    /// 空模型名：不猜（返回 None 而不是套通用词）。
    #[test]
    fn wc_probe_empty_model_matches_nothing() {
        let cfg = PurgeSettings::default();
        let v = preset_probe("   ", &cfg);
        assert!(v["data"]["effective"].is_null());
        assert_eq!(v["data"]["would_apply"], json!(false));
        assert_eq!(v["data"]["normalized"], json!(""));
    }
}
