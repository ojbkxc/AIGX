/**
 * 配置体检告警的「怎么修」（纯函数层，执行动作由页面接线）。
 *
 * 背景（用户原话）：「每条告警给一个『怎么修』的动作，不是只显示文案」。
 * 之前 `config_warning_items` 只有 `message`，每条都配一个泛泛的「去修复」
 * 按钮——跳到输入框后面还得自己判断该填什么。
 *
 * 纪律：
 * 1. **只给能自动做完的动作**：要么改草稿里的具体字段（可见、可撤销），要么
 *    跳到具体控件并把该展开的组展开。不做「猜一个值填进去」之外的臆造。
 * 2. **不吞掉告警**：点完动作后告警**仍然显示**，只是加一行「已按建议修改，
 *    保存后消失」。因为真正判定告警的是后端（save 之后重新体检），前端把行
 *    藏起来等于谎报「已经好了」。
 * 3. code 全集与后端 `config_warning_items` 的 10 个 push 点一一对应
 *    （`src/purge/mod.rs:1642-1801`）。后端新增 code 时这里要同步：
 *    未登记的 code 落「跳转到对应锚点」的兜底动作，不会变成死按钮。
 */
import type { PurgeSettings } from '../types';

/** 告警可执行动作的种类：改草稿 / 纯跳转。 */
export type HealthFixKind = 'draft' | 'jump';

export interface HealthFixSpec {
  /** 按钮文案（i18n key = 中文原文） */
  label: string;
  kind: HealthFixKind;
  /** kind=jump 时的跳转锚点 */
  anchor?: string;
}

/** 告警 code → 修复动作（后端 code 注释里的「机器码 → 前端修复入口约定」）。 */
const HEALTH_FIXES: Record<string, HealthFixSpec[]> = {
  // 异源递进开着但候选为空 → 直接填候选（模型名从「模型可用列表」里挑），
  // 或至少把用户送到输入框（没有模型列表时）。
  fallback_empty: [
    { label: '填入推荐异源', kind: 'draft' },
    { label: '去异源模型框', kind: 'jump', anchor: 'fallback-models' },
  ],
  // 触发轮次 > 重试上限 → 永远到不了：按重试上限对齐（可再手改）
  fallback_after_retry: [{ label: '按重试上限对齐轮次', kind: 'draft' }],
  // 重复项 → 去重（大小写不敏感），保留最早出现的一条
  fallback_duplicate: [{ label: '去重', kind: 'draft' }],
  // 技能目录为空 → 新建技能（导入技能包的入口就在这个组里）
  skill_dir_empty: [
    { label: '新建技能（导入技能包）', kind: 'jump', anchor: 'skill-import' },
    { label: '看技能目录说明', kind: 'jump', anchor: 'skill-dir' },
  ],
  // 单字符关键词 → 只能人改 SKILL.md 的 description，页面给快照与定位
  skill_keyword_short: [{ label: '看这几个技能', kind: 'jump', anchor: 'skill-snapshot' }],
  // 链式二次改写 → 跳到死区词表并打开「只看异常」
  chained_rewrite: [{ label: '只看异常行', kind: 'draft' }],
  // 整表被单条停用 → 一键全启用（保留内容，只翻 enabled）
  dead_zone_all_disabled: [{ label: '全部启用', kind: 'draft' }],
  synonym_all_disabled: [{ label: '全部启用', kind: 'draft' }],
  // 结构化提示开着但生效文本为空 → 填一段中性后缀（可再改），或去选模板
  structured_empty: [
    { label: '填入推荐后缀', kind: 'draft' },
    { label: '去选模板', kind: 'jump', anchor: 'templates' },
  ],
  // 固定预设 id 不存在 → 清空固定值改按模型匹配（后端 message 里已给最接近 id）
  preset_missing: [{ label: '清空固定预设（改按模型匹配）', kind: 'draft' }],
};

/**
 * 取某条告警的修复动作。
 *
 * 未登记 code（后端新增而前端未跟上）→ 给「跳转到对应锚点」的兜底，
 * 锚点由调用方按 `FIX_TARGETS` 传进来；连锚点都没有就返回空数组
 * （渲染层不画按钮，而不是画一个点了没反应的死按钮）。
 */
export function healthFixSpecs(code: string, fallbackAnchor?: string): HealthFixSpec[] {
  const known = HEALTH_FIXES[code];
  if (known && known.length > 0) return known;
  if (fallbackAnchor) return [{ label: '去修复', kind: 'jump', anchor: fallbackAnchor }];
  return [];
}

/** 建议的结构化后缀（中性、不改变语义方向；用户可再改）。 */
export const RECOMMENDED_STRUCTURED_SUFFIX =
  '请直接给出可执行的答案，不要复述问题、不要附加免责声明。';

/** 推荐异源模型的候选数上限（后端 `normalize` 硬上限 16，这里给 3 条够用）。 */
export const RECOMMEND_FALLBACK_COUNT = 3;

/**
 * 从「模型可用列表」里挑推荐异源模型（纯函数）。
 *
 * 口径：优先覆盖**不同厂商**（`owned_by`），一个厂商只取一条——异源递进的意义
 * 是「换个源」，同一家的第二条模型解决的是限流而不是内容层失败。
 * 排除当前默认模型（它已经是主模型）与空串；结果去重（大小写不敏感）。
 */
export function recommendFallbackModels(
  models: Array<{ id: string; owned_by?: string }>,
  options: { exclude?: string; limit?: number } = {},
): string[] {
  const limit = Math.max(1, options.limit ?? RECOMMEND_FALLBACK_COUNT);
  const exclude = (options.exclude ?? '').trim().toLowerCase();
  const seenVendor = new Set<string>();
  const seenModel = new Set<string>();
  const out: string[] = [];
  for (const model of models) {
    const id = (model.id ?? '').trim();
    if (!id) continue;
    const lower = id.toLowerCase();
    if (lower === exclude) continue;
    if (seenModel.has(lower)) continue;
    const vendor = (model.owned_by ?? '').trim().toLowerCase() || `_${lower}`;
    if (seenVendor.has(vendor)) continue;
    seenVendor.add(vendor);
    seenModel.add(lower);
    out.push(id);
    if (out.length >= limit) break;
  }
  return out;
}

/** 异源模型去重（大小写不敏感，保留最早出现的一条；保序）。 */
export function dedupeFallbackModels(models: readonly string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of models) {
    const id = raw.trim();
    if (!id) continue;
    const key = id.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(id);
  }
  return out;
}

/**
 * 点在草稿上的修复动作（纯函数：入参草稿 → 出参补丁）。
 *
 * 返回 `null` = 这条 code 不支持自动改（或当前状态已无法自动修），调用方据此
 * 给出「改不动，去看字段」的提示，而不是假装点了一下就好了。
 */
export function applyHealthFix(
  code: string,
  settings: PurgeSettings,
  context: { recommendedModels: readonly string[]; defaultModel?: string },
): Partial<PurgeSettings> | { __jump: string } | null {
  switch (code) {
    case 'fallback_empty': {
      // 调用方可以直接给「推荐模型名」（内置推荐），也可以给「模型可用列表」
      // 由这里按厂商挑；两者都没有就放弃，避免凭空造一个不存在的模型名。
      const pick =
        context.recommendedModels.length > 0
          ? [...context.recommendedModels].slice(0, RECOMMEND_FALLBACK_COUNT)
          : [];
      if (pick.length === 0) return null;
      return { fallback_models: pick };
    }
    case 'fallback_after_retry': {
      const next = Math.min(32, Math.max(1, Math.trunc(settings.retry_limit)));
      if (settings.fallback_trigger_attempt === next) return null;
      return { fallback_trigger_attempt: next };
    }
    case 'fallback_duplicate': {
      const deduped = dedupeFallbackModels(settings.fallback_models ?? []);
      if (deduped.length === (settings.fallback_models ?? []).length) return null;
      return { fallback_models: deduped };
    }
    case 'structured_empty': {
      // 与后端 `active_structured_text()` 同口径：选中的模板正文优先，否则用后缀
      const activeName = (settings.active_template ?? '').trim();
      const activeTemplate = activeName
        ? (settings.structured_templates ?? []).find((tpl) => tpl.name.trim() === activeName)
        : undefined;
      if ((activeTemplate?.text ?? settings.structured_suffix ?? '').trim()) return null;
      return { structured_suffix: RECOMMENDED_STRUCTURED_SUFFIX };
    }
    case 'preset_missing': {
      if (!(settings.auto_preset_id ?? '').trim()) return null;
      return { auto_preset_id: '' };
    }
    case 'dead_zone_all_disabled': {
      if ((settings.rewrite_rules ?? []).length === 0) return null;
      return { rewrite_rules: settings.rewrite_rules.map((rule) => ({ ...rule, enabled: true })) };
    }
    case 'synonym_all_disabled': {
      if ((settings.synonym_pairs ?? []).length === 0) return null;
      return { synonym_pairs: settings.synonym_pairs.map((rule) => ({ ...rule, enabled: true })) };
    }
    case 'chained_rewrite':
      // 只打开筛选，真正改什么由人判断（链式改写是否「符合预期」没有客观答案）
      return { __jump: 'dead-zone-issues' };
    case 'skill_dir_empty':
    case 'skill_keyword_short':
      return { __jump: code === 'skill_dir_empty' ? 'skill-import' : 'skill-snapshot' };
    default:
      return null;
  }
}

/** 告警是否已经有「点过修复」的记录（按 code；同一 code 多条时共用）。 */
export function markFixed(codes: readonly string[], code: string): string[] {
  return codes.includes(code) ? [...codes] : [...codes, code];
}