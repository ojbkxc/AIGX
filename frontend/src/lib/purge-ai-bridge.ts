/**
 * 破甲控制台 ↔ AI 运维（`/purge` ↔ `/agent`）的**双向跳转**：一句话让 AI 调破甲。
 *
 * ── 为什么需要这一层 ──────────────────────────────────────────────
 * 两条链路的工具面早就通着（`aigx_config_view{key:"purge"}` /
 * `aigx_config_set{key:"purge"}` / `aigx_purge_check` / `aigx_purge_preset_probe`），
 * 但用户到不了入口：他得先记住「AI 运维能改破甲」，再自己把配置摘要抄进对话框。
 * 抄一版二十多个字段的配置给 AI 是本页最贵的动作，而它的内容**已经**在
 * `GET /api/purge` 的返回里。
 *
 * ── 为什么用 sessionStorage 而不是路由 query ────────────────────────
 * `/agent` 现在**不读任何初始消息**（无 `?prompt=` 机制），而为此改路由要动
 * `App.tsx` 的 Route 定义与两页的挂载语义。sessionStorage 是「同一标签页内的
 * 一次性交接」的天然载体：
 *
 * - **按标签页隔离**：两个标签页各点各的，不会互相覆盖；
 * - **自带生命周期**：关标签页即消失，不会像 localStorage 那样在下次开机
 *   把上周的破甲快照塞进输入框；
 * - **一次性**：`consume` 读完即删，刷新 `/agent` 不会重复注入。
 *
 * 再加一层 `TTL_MS`：标签页恢复（浏览器「恢复上次会话」）会把 sessionStorage
 * 一起带回来，半小时前的配置摘要已经不代表现状了，宁可不填也不能填错的。
 *
 * ── 为什么单独一个 lib 文件而不是写在页面里 ─────────────────────────
 * 两个页面都要用同一个键与同一套解析。写在 `PurgeConsole.tsx` 里再让
 * `Agent.tsx` import，会把 24 万字节的破甲页静态拖进 /agent 的 chunk
 * （`App.tsx` 里两页都是 `lazyPage`，静态互相引用等于把懒加载拆掉一半）。
 */

/** sessionStorage 键（带版本号：结构变了下一次写入自然失效，不用写迁移）。 */
export const PURGE_AI_HANDOFF_KEY = 'aigx_purge_ai_handoff_v1';

/** 交接有效期：超过这个时长就不填（浏览器会话恢复会把旧值带回来）。 */
export const PURGE_AI_HANDOFF_TTL_MS = 30 * 60 * 1000;

/** 注入输入框的正文上限（字符）：再长就不是「一句话」了，用户还得自己删。 */
export const PURGE_AI_PROMPT_MAX_CHARS = 2400;

/** sessionStorage 的最小读写面（测试可注入，与 `PurgeConsole` 的 ls*Stable 同形态）。 */
export interface PurgeAiStorage {
  get: (key: string) => string | null;
  set: (key: string, value: string) => void;
  remove: (key: string) => void;
}

/** 交接内容（落盘形状即此，`v` 用于结构演进）。 */
export interface PurgeAiHandoff {
  v: 1;
  prompt: string;
  /** 写入时刻（unix ms）：TTL 判据 */
  at: number;
}

/**
 * 写交接（点「问 AI」时调用）。
 *
 * 存储满/被禁用（隐私模式）时静默失败并返回 `false`：调用方据此走「复制到剪贴板」
 * 的降级路径，而不是让按钮看起来点了没反应。
 */
export function writePurgeAiHandoff(
  prompt: string,
  storage: PurgeAiStorage,
  now: number = Date.now(),
): boolean {
  const text = prompt.trim();
  if (!text) return false;
  const payload: PurgeAiHandoff = {
    v: 1,
    prompt: text.slice(0, PURGE_AI_PROMPT_MAX_CHARS),
    at: now,
  };
  try {
    storage.set(PURGE_AI_HANDOFF_KEY, JSON.stringify(payload));
    return true;
  } catch {
    return false;
  }
}

/**
 * 读交接但**不删**（页面首帧判断「是不是从破甲页进来的」用）。
 *
 * 解析失败/结构不符/过期一律返回 `null`。过期项顺手删掉——留着只会在下一次
 * 判断里再被读一次，而它已经没有意义了。
 */
export function readPurgeAiHandoff(
  storage: PurgeAiStorage,
  now: number = Date.now(),
): PurgeAiHandoff | null {
  let raw: string | null = null;
  try {
    raw = storage.get(PURGE_AI_HANDOFF_KEY);
  } catch {
    return null; // 禁用存储：当作没有交接
  }
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    storage.remove(PURGE_AI_HANDOFF_KEY);
    return null;
  }
  if (!parsed || typeof parsed !== 'object') {
    storage.remove(PURGE_AI_HANDOFF_KEY);
    return null;
  }
  const row = parsed as Record<string, unknown>;
  const prompt = typeof row.prompt === 'string' ? row.prompt : '';
  const at = typeof row.at === 'number' ? row.at : 0;
  if (row.v !== 1 || !prompt.trim()) {
    storage.remove(PURGE_AI_HANDOFF_KEY);
    return null;
  }
  if (!Number.isFinite(at) || now - at > PURGE_AI_HANDOFF_TTL_MS) {
    storage.remove(PURGE_AI_HANDOFF_KEY);
    return null;
  }
  return { v: 1, prompt, at };
}

/**
 * 取走交接正文并清掉键（`/agent` 挂载时调用）。
 *
 * **一次性的意义**：`/agent` 挂载在每次路由切换都会重新跑 effect，若不清键，
 * 用户离开再回来会被再填一次同一个摘要。
 */
export function consumePurgeAiHandoff(
  storage: PurgeAiStorage,
  now: number = Date.now(),
): string | null {
  const item = readPurgeAiHandoff(storage, now);
  if (!item) return null;
  storage.remove(PURGE_AI_HANDOFF_KEY);
  return item.prompt;
}

/** `buildPurgeAiPrompt` 的入参：只吃**已经算好**的片段，不在本文件里二次推导。 */
export interface PurgeAiPromptInput {
  /** 生效项摘要（`buildActiveSummary` 的文本形态，一行一项） */
  activeItems: readonly string[];
  /** 体检告警：`message` 面向人，`severity` 用于排序/标注 */
  warnings: readonly { message: string; severity?: string }[];
  /** 预设命中读数（可缺：模型视图取不到时本段省略） */
  presetHit?: {
    models_enabled: number;
    exact: number;
    family: number;
    universal: number;
    none: number;
    would_apply: number;
  } | null;
  /** 技能扫描（可缺） */
  skills?: { count: number; inject: boolean } | null;
  /** 末尾那句「要 AI 做什么」：默认是体检 + 最小改动方案 */
  ask?: string;
}

/**
 * 拼出带进 AI 运维输入框的那段话。
 *
 * 三条硬约束（都来自「AI 改破甲」这一步的真实翻车方式）：
 * 1. **必须自带现状**：AI 会先 `aigx_config_view{key:"purge"}` 读一遍，但如果输入框
 *    里已经有摘要，它就不必为「现在到底开了什么」多跑一轮；
 * 2. **必须说清是整份替换**：`PUT /api/purge` 缺字段回落出厂默认——提示里点明
 *    「改完要整份写回」，否则模型只传一个字段就把两张词表清零；
 * 3. **必须要求先体检再改**：把 `aigx_purge_check` 的名字写进提示，让工具选择
 *    有明确的第一跳（模型不必自己从二十多个字段里推「哪些开了却不生效」）。
 */
export function buildPurgeAiPrompt(input: PurgeAiPromptInput): string {
  const lines: string[] = [];
  lines.push('【破甲控制台现状】（来自控制台当前生效配置）');
  if (input.activeItems.length === 0) {
    lines.push('- （无生效项）');
  } else {
    for (const item of input.activeItems) lines.push(`- ${item}`);
  }

  lines.push('');
  lines.push('【配置体检】');
  if (input.warnings.length === 0) {
    lines.push('- 通过：没有「开了但不生效」或自相矛盾的项目');
  } else {
    for (const w of input.warnings) {
      lines.push(`- [${w.severity ?? 'error'}] ${w.message}`);
    }
  }

  if (input.presetHit) {
    const h = input.presetHit;
    lines.push('');
    lines.push('【按模型套预设的命中率】');
    lines.push(
      `- 启用模型 ${h.models_enabled} 个：同名命中 ${h.exact} · 同族命中 ${h.family}` +
        ` · 通用兜底 ${h.universal} · 无预设可套 ${h.none}；真正会前插正文 ${h.would_apply} 个`,
    );
  }

  if (input.skills) {
    lines.push('');
    lines.push('【技能目录】');
    lines.push(
      `- 扫描到 ${input.skills.count} 个 SKILL.md；技能注入开关=${input.skills.inject ? '开' : '关'}`,
    );
  }

  lines.push('');
  lines.push(
    input.ask ??
      '请先体检（aigx_purge_check）再动手：指出这套配置里哪些项「开了却不生效」或互相矛盾，' +
        '给出最小改动方案（改哪个字段、改成什么、为什么）。' +
        '确认要改时用 aigx_config_view{key:"purge"} 读整份配置、改完后整份写回 aigx_config_set{key:"purge"}' +
        '（PUT 是整份替换，只传单个字段会把词表清零），写完再体检一次复验。',
  );

  const text = lines.join('\n');
  return text.length > PURGE_AI_PROMPT_MAX_CHARS
    ? `${text.slice(0, PURGE_AI_PROMPT_MAX_CHARS)}\n…（已截断：完整配置请用 aigx_config_view{key:"purge"} 读）`
    : text;
}

/** Agent 侧工具轨迹的最小形状（避免本文件依赖 `tool-steps.ts` 的类型）。 */
export interface PurgeToolTraceItem {
  name: string;
  args?: string;
}

/** 破甲相关的工具名（`aigx_config_set` 另按参数里的 `"purge"` 判）。 */
const PURGE_TOOL_NAMES = new Set([
  'aigx_purge_check',
  'aigx_purge_preset_probe',
]);

/**
 * 这一轮对话有没有碰过破甲 —— 决定 `/agent` 里要不要显示「去破甲控制台看结果」。
 *
 * 为什么按轨迹判而不是按「AI 说了什么」：模型复述自己不准确（它可能说「已调整
 * 破甲」而其实只读了一次）。轨迹里的工具名与参数是**实际发生过**的那一半。
 *
 * `aigx_config_set` 的参数是 JSON 文本，里面 `"key":"purge"` 才算——把 `key`
 * 取出再比，避免 `purge` 出现在别的字段值里（比如某个模板正文提到这个词）造成误判。
 */
export function touchedPurge(items: readonly PurgeToolTraceItem[]): boolean {
  for (const item of items) {
    if (PURGE_TOOL_NAMES.has(item.name)) return true;
    if (item.name !== 'aigx_config_set' || !item.args) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(item.args);
    } catch {
      continue;
    }
    if (!parsed || typeof parsed !== 'object') continue;
    const key = (parsed as Record<string, unknown>).key;
    if (typeof key === 'string' && key === 'purge') return true;
  }
  return false;
}