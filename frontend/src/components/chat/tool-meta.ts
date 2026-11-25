/**
 * 工具元信息：名称 → 中文标签 / 风险等级 / A2A「kind」的映射。
 *
 * ── 判据来源只有一个 ─────────────────────────────────────────
 * AIGX 后端 `/api/agent/config`（= `/api/agent/settings` 同源）的 `tools[]`
 * 给每只工具 `{name, label, risk}`，`risk` 的值域由 `agent/tools.rs` 的
 * `RiskLevel::as_str()` 决定（`readonly` / `low` / `high`）。前端**不得**另写
 * 一张中文名表：改名 = 用户看到的审批卡与工具卡对不上后端实际执行的工具。
 *
 * ── 与 A2A 的 kind 三态 ─────────────────────────────────────
 * A2A 的工具定义带 `kind: 'read' | 'write' | 'run'`，决定确认卡要不要弹：
 *   read  只读 → 读完直接回给模型；
 *   write 改动 → 先渲染确认卡；
 *   run   真打上游 / 消耗额度（模型测试）→ 同 write，且卡上写明代价。
 * AIGX 的风险位只有三档，**没有**「消耗额度」这一维（模型测试在 AIGX 是
 * 管理页动作，不在 Agent 工具集里）。因此这里只能派生 read / write，
 * `run` 保留常量与判据位（`kindOfTool` 遇 `risk === 'run'` 时认）但当前永远
 * 取不到 —— 记为「A2A 有而 AIGX 缺数据源」。
 */

/** 后端 `RiskLevel::as_str()` 的值域 + 前端兜底档（未知 risk 不当只读） */
export type ToolRisk = 'readonly' | 'low' | 'high' | 'unknown';

/** A2A 的工具语义三态（`run` 当前无数据源） */
export type ToolKind = 'read' | 'write' | 'run' | 'unknown';

/** 后端下发的一条工具规格（`agent_settings_json` 的 `tools[]`） */
export interface AgentToolSpec {
  name: string;
  label: string;
  description?: string;
  risk?: string;
}

/** 规格索引：审批卡 / 工具卡共用（按 name 查一次，不在渲染里线性 find） */
export interface ToolSpecIndex {
  label: (name: string | undefined | null) => string;
  risk: (name: string | undefined | null) => ToolRisk;
  kind: (name: string | undefined | null) => ToolKind;
  description: (name: string | undefined | null) => string;
}

const EMPTY_INDEX: ToolSpecIndex = {
  label: (name) => String(name ?? ''),
  risk: () => 'unknown',
  kind: () => 'unknown',
  description: () => '',
};

/** `risk` 字符串归一（大小写与连字符容错：`read_only` / `ReadOnly` 都收） */
export function normalizeRisk(raw: unknown): ToolRisk {
  const text = typeof raw === 'string' ? raw.trim().toLowerCase().replace(/[-_\s]/g, '') : '';
  if (text === 'readonly') return 'readonly';
  if (text === 'low' || text === 'medium' || text === 'high' || text === 'dangerous') {
    return text === 'low' || text === 'medium' ? 'low' : 'high';
  }
  return 'unknown';
}

/** 风险 → A2A kind（`run` 不由 risk 推导：见文件头） */
export function kindOfRisk(risk: ToolRisk): ToolKind {
  if (risk === 'readonly') return 'read';
  if (risk === 'low' || risk === 'high') return 'write';
  return 'unknown';
}

/** 风险徽标文案的 i18n key（三档都有既有键；未知档走中性文案） */
export function riskLabelKey(risk: ToolRisk): string {
  if (risk === 'readonly') return '只读';
  if (risk === 'low') return '低危';
  if (risk === 'high') return '高危';
  return '未知';
}

/**
 * 是否必须人工审批。
 *
 * 判据只在后端（`agent/runner.rs` 按 role + risk 决定），前端这份只用于
 * **展示**（审批卡的「高危」角标、工具卡的写入色）。拿它当放行判据等于
 * 在浏览器里做权限判断 —— 不会被信任，也不该被信任。
 */
export function needsApproval(risk: ToolRisk): boolean {
  return risk === 'high' || risk === 'low';
}

/** 由后端 `tools[]` 建索引（缺配置时返回空索引：标签退化回工具 id，不编造中文名） */
export function toolSpecIndex(specs: readonly AgentToolSpec[] | undefined | null): ToolSpecIndex {
  if (!Array.isArray(specs) || !specs.length) return EMPTY_INDEX;
  const table = new Map<string, AgentToolSpec>();
  for (const spec of specs) {
    const name = typeof spec?.name === 'string' ? spec.name.trim() : '';
    if (!name) continue;
    table.set(name, spec);
  }
  if (!table.size) return EMPTY_INDEX;
  const lookup = (name: string | undefined | null): AgentToolSpec | undefined => {
    const key = typeof name === 'string' ? name.trim() : '';
    return key ? table.get(key) : undefined;
  };
  return {
    label: (name) => {
      const spec = lookup(name);
      const label = typeof spec?.label === 'string' ? spec.label.trim() : '';
      return label || String(name ?? '');
    },
    risk: (name) => normalizeRisk(lookup(name)?.risk),
    kind: (name) => kindOfRisk(normalizeRisk(lookup(name)?.risk)),
    description: (name) => {
      const text = lookup(name)?.description;
      return typeof text === 'string' ? text : '';
    },
  };
}
