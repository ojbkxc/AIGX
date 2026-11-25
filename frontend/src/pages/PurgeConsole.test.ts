import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import type { PurgeSettings } from '../types';
import {
  applyUndo,
  buildActiveSummary,
  batchDropNote,
  buildConfigExport,
  clampNumInput,
  configuredButOff,
  confirmSkillOverwrite,
  collectCapOverflows,
  countEmptyFromRules,
  capError,
  exportPreviewHistory,
  filterRules,
  importOverwriteItems,
  disableRulesAt,
  findBodySnippet,
  parseBatchRules,
  parsePreviewHistoryJson,
  lastRunNote,
  normalizeSettingsArrays,
  normalizeBooleanFields,
  noteRecentPreset,
  readDraftBackup,
  readPresetFilter,
  readRecentPresets,
  retryRateText,
  ruleIssues,
  ruleRowHint,
  rulesToText,
  snapshotReportText,
  statsExportNote,
  sortWarningsBySeverity,
  tidyRules,
  undoSnapshot,
  warningReportText,
  settingsEqual,
  summaryTextWithOff,
  STATS_CSV_HELP,
  swapRules,
  tidySummary,
  writeDraftBackup,
  diffLines,
  diffSettings,
  diffTokensInLine,
  parsePreviewMessages,
  parseRulesCsv,
  parseRulesCsvDetailed,
  parseTemplateJson,
  persistPreviewHistory,
  presetApplyNeedsConfirm,
  previewHistoryClearPrompt,
  previewHistoryLabel,
  readPreviewHistory,
  skillKeywordSample,
  validateImportedSettings,
  writePreviewHistory,
  writePresetFilter,
} from './PurgeConsole';

describe('readRecentPresets / noteRecentPreset', () => {
  const store = new Map<string, string>();
  const set = (k: string, v: string): void => {
    store.set(k, v);
  };
  const remove = (k: string): void => {
    store.delete(k);
  };

  it('记一次使用：去重置顶', () => {
    store.clear();
    let ids = noteRecentPreset([], 'a', set, remove);
    ids = noteRecentPreset(ids, 'b', set, remove);
    ids = noteRecentPreset(ids, 'a', set, remove); // 再用 a → 置顶
    expect(ids).toEqual(['a', 'b']);
    expect(readRecentPresets((k) => store.get(k) ?? null)).toEqual(['a', 'b']);
  });

  it('超过上限截断（保留最近的）', () => {
    store.clear();
    let ids: string[] = [];
    for (const id of ['1', '2', '3', '4', '5', '6', '7']) {
      ids = noteRecentPreset(ids, id, set, remove);
    }
    expect(ids).toHaveLength(5);
    expect(ids[0]).toBe('7');
  });

  it('坏 JSON / 非字符串元素 → 空表（不拿脏值渲染徽标）', () => {
    expect(readRecentPresets(() => 'not-json')).toEqual([]);
    expect(readRecentPresets(() => '{"a":1}')).toEqual([]);
    expect(readRecentPresets(() => '[1,"ok",null,""]')).toEqual(['ok']);
  });

  it('写失败静默（localStorage 满/禁用不影响使用）', () => {
    const boom = (): void => {
      throw new Error('quota');
    };
    expect(() => noteRecentPreset([], 'x', boom, boom)).not.toThrow();
  });
});

describe('readDraftBackup / writeDraftBackup', () => {
  const store = new Map<string, string>();
  const set = (k: string, v: string): void => {
    store.set(k, v);
  };
  const remove = (k: string): void => {
    store.delete(k);
  };
  const sample = { enabled: true } as never;

  it('写入后能读回，带 savedAt', () => {
    store.clear();
    writeDraftBackup(sample, set, remove, 1_000);
    const backup = readDraftBackup((k) => store.get(k) ?? null, 2_000);
    expect(backup?.settings).toEqual(sample);
    expect(backup?.savedAt).toBe(1_000);
  });

  it('超过 24h 视为过期（不拿上次遗留误导恢复）', () => {
    store.clear();
    writeDraftBackup(sample, set, remove, 1_000);
    expect(readDraftBackup((k) => store.get(k) ?? null, 1_000 + 24 * 3600 * 1000 + 1)).toBeNull();
  });

  it('传 null 时移除键（不留无意义条目）', () => {
    store.clear();
    writeDraftBackup(sample, set, remove, 1_000);
    writeDraftBackup(null, set, remove);
    expect(store.has('aigx.purge.draftBackup')).toBe(false);
  });

  it('坏 JSON / 缺字段 → null（不拿脏值渲染恢复条）', () => {
    expect(readDraftBackup(() => 'not-json')).toBeNull();
    expect(readDraftBackup(() => '{"savedAt":1}')).toBeNull();
    expect(readDraftBackup(() => '{"settings":{}}')).toBeNull();
  });

  it('写失败静默（localStorage 满不能反过来打断编辑）', () => {
    const boom = (): void => {
      throw new Error('quota');
    };
    expect(() => writeDraftBackup(sample, boom, boom)).not.toThrow();
  });
});

describe('settingsEqual', () => {
  it('键序不同但内容相同 → 等价（否则会误弹恢复条）', () => {
    expect(settingsEqual({ a: 1, b: 2 } as never, { b: 2, a: 1 } as never)).toBe(true);
  });

  it('数组顺序不同 → 不等价（词表顺序有意义）', () => {
    expect(settingsEqual({ a: [1, 2] } as never, { a: [2, 1] } as never)).toBe(false);
  });

  it('值不同 → 不等价', () => {
    expect(settingsEqual({ a: 1 } as never, { a: 2 } as never)).toBe(false);
  });
});

describe('statsExportNote', () => {
  it('没有下界档 → null（不弹无意义的提示）', () => {
    expect(statsExportNote([{ label: '近 1 小时' }, { label: '近 24 小时', truncated: false }])).toBeNull();
  });

  it('只列出下界的档位（精确档不点名）', () => {
    expect(
      statsExportNote([
        { label: '近 1 小时', truncated: true },
        { label: '近 24 小时', truncated: false },
      ]),
    ).toBe('近 1 小时');
  });

  it('多档都是下界时用 / 连接', () => {
    expect(
      statsExportNote([
        { label: '近 1 小时', truncated: true },
        { label: '近 24 小时', truncated: true },
      ]),
    ).toBe('近 1 小时 / 近 24 小时');
  });

  it('空列表 → null', () => {
    expect(statsExportNote([])).toBeNull();
  });
});

describe('clampNumInput', () => {
  const field = { min: 1, max: 32, label: '重试上限' };

  it('范围内原样返回（不改动、不提示）', () => {
    expect(clampNumInput(field, '5')).toEqual({ corrected: '5', notice: null });
  });

  it('超上限 → 收敛到上限并报 clamped（说明范围而不是悄悄改）', () => {
    expect(clampNumInput(field, '999')).toEqual({
      corrected: '32',
      notice: { kind: 'clamped', value: 32 },
    });
  });

  it('低于下限 → 收敛到下限', () => {
    expect(clampNumInput(field, '0')).toEqual({
      corrected: '1',
      notice: { kind: 'clamped', value: 1 },
    });
  });

  it('小数 → 取整（报 rounded，与范围收紧区分开）', () => {
    expect(clampNumInput(field, '3.7')).toEqual({
      corrected: '3',
      notice: { kind: 'rounded', value: 3 },
    });
  });

  it('空串与非数字不做处理（那是保存前的正常中间态）', () => {
    expect(clampNumInput(field, '')).toEqual({ corrected: '', notice: null });
    expect(clampNumInput(field, '  ')).toEqual({ corrected: '', notice: null });
    expect(clampNumInput(field, 'abc')).toEqual({ corrected: '', notice: null });
  });

  it('首尾空格被清掉但不触发提示', () => {
    expect(clampNumInput(field, '  12  ')).toEqual({ corrected: '12', notice: null });
  });
});

describe('parseBatchRules', () => {
  it('Tab 分隔（Excel 两列直接粘贴）', () => {
    const { rules, skipped } = parseBatchRules('原文A\t改写A\n原文B\t改写B');
    expect(rules).toEqual([
      { from: '原文A', to: '改写A' },
      { from: '原文B', to: '改写B' },
    ]);
    expect(skipped).toBe(0);
  });

  it('认箭头/等号分隔符', () => {
    const { rules } = parseBatchRules('a -> b\nc => d\ne → f\ng ⇒ h');
    expect(rules.map((r) => `${r.from}=${r.to}`)).toEqual(['a=b', 'c=d', 'e=f', 'g=h']);
  });

  it('空行跳过但不计入 skipped', () => {
    const { rules, skipped } = parseBatchRules('\n\na\tb\n   \n');
    expect(rules).toHaveLength(1);
    expect(skipped).toBe(0);
  });

  it('分不出两段的行计入 skipped（如实回报）', () => {
    const { rules, skipped } = parseBatchRules('原文\t改写\n只有一段\n表头\n缺右边\t');
    expect(rules).toHaveLength(1);
    expect(skipped).toBe(3);
  });

  it('与现有规则重复（忽略大小写与首尾空格）的丢弃', () => {
    const { rules } = parseBatchRules('Foo\tbar\nfoo\tbaz', [{ from: ' FOO ', to: 'x' }]);
    expect(rules).toHaveLength(0);
  });

  it('逗号不作为分隔符（改写内容常含逗号，劈开会比不识别更坏）', () => {
    const { rules, skipped } = parseBatchRules('好的, then');
    expect(rules).toHaveLength(0);
    expect(skipped).toBe(1);
  });
});

describe('disableRulesAt', () => {
  const rules = [
    { from: 'a', to: '1', enabled: true },
    { from: 'b', to: '2' }, // enabled 缺省（视为启用）
    { from: 'c', to: '3', enabled: true },
  ];

  it('只停用指定下标，其余行不动', () => {
    const next = disableRulesAt(rules, new Set([0, 2]));
    expect(next[0].enabled).toBe(false);
    expect(next[1]).toEqual({ from: 'b', to: '2' }); // 缺省行不被塞进 enabled:true
    expect(next[2].enabled).toBe(false);
  });

  it('空集合 → 内容等价、但不返回原数组', () => {
    const next = disableRulesAt(rules, new Set());
    expect(next).toEqual(rules);
    expect(next).not.toBe(rules);
  });

  it('不改原数组（避免就地变异掩盖问题）', () => {
    disableRulesAt(rules, new Set([0]));
    expect(rules[0].enabled).toBe(true);
  });

  it('**不删除行**（停用是中间档，删不可逆）', () => {
    const next = disableRulesAt(rules, new Set([1]));
    expect(next).toHaveLength(rules.length);
    expect(next.map((r) => r.from)).toEqual(['a', 'b', 'c']);
  });
});

describe('validateImportedSettings · 导出-导入自洽', () => {
  it('**本页导出的配置能导回来**（字符串数组不该被当成对象数组）', () => {
    // 这就是之前的真实缺陷：`fallback_models: ['glm-5']` 被按「元素必须是对象」
    // 校验 → 判 fatal → 导出的文件自己导不进去。
    const exported = {
      enabled: true,
      rewrite_rules: [{ from: 'a', to: 'b' }],
      synonym_pairs: [{ from: 'x', to: 'y' }],
      structured_templates: [{ name: 't', text: 'body' }],
      fallback_models: ['glm-5', 'gpt-4o'],
      normalize_strip_prefixes: ['Current time'],
      refusal_patterns: ['抱歉'],
      failure_filter_keywords: ['filtered'],
      failure_empty_keywords: ['empty'],
    };
    const verdict = validateImportedSettings(exported);
    expect(verdict.fatal).toEqual([]);
    expect(verdict.warnings).toEqual([]);
  });

  it('启用模板悬空 → 警告（后端会静默回退到 fixed suffix）', () => {
    const verdict = validateImportedSettings({
      active_template: 'missing-one',
      structured_templates: [{ name: 't1', text: 'body' }],
    });
    expect(verdict.fatal).toEqual([]);
    expect(verdict.warnings.some((w) => w.includes('missing-one'))).toBe(true);
    expect(verdict.warnings.some((w) => w.includes('structured_suffix'))).toBe(true);
  });

  it('启用模板命中模板表 → 不警告', () => {
    const verdict = validateImportedSettings({
      active_template: 't1',
      structured_templates: [{ name: 't1', text: 'body' }],
    });
    expect(verdict.warnings).toEqual([]);
  });

  it('启用模板为空/仅空白 → 不警告（用固定后缀是正常状态）', () => {
    for (const v of ['', '   ', undefined]) {
      const verdict = validateImportedSettings({
        active_template: v,
        structured_templates: [{ name: 't1', text: 'body' }],
      });
      expect(verdict.warnings).toEqual([]);
    }
  });

  it('降档目标非法值 → 警告（后端不清洗非空值）', () => {
    const verdict = validateImportedSettings({ reasoning_floor: 'extreme' });
    expect(verdict.fatal).toEqual([]);
    expect(verdict.warnings.some((w) => w.includes('extreme'))).toBe(true);
    expect(verdict.warnings.some((w) => w.includes('low'))).toBe(true);
  });

  it('降档目标合法值（四个档位）→ 不警告', () => {
    for (const floor of ['low', 'medium', 'high', 'minimal']) {
      const verdict = validateImportedSettings({ reasoning_floor: floor });
      expect(verdict.warnings).toEqual([]);
    }
  });

  it('降档目标不是字符串 → 警告', () => {
    const verdict = validateImportedSettings({ reasoning_floor: 42 });
    expect(verdict.warnings.length).toBeGreaterThan(0);
  });

  it('字符串数组里混入非字符串 → 只警告（不整份拒绝）', () => {
    const verdict = validateImportedSettings({ fallback_models: ['ok', 42] });
    expect(verdict.fatal).toEqual([]);
    expect(verdict.warnings.length).toBeGreaterThan(0);
  });

  it('对象数组里混入非对象 → 仍然 fatal（那会渲染报错）', () => {
    const verdict = validateImportedSettings({ rewrite_rules: ['oops'] });
    expect(verdict.fatal.length).toBeGreaterThan(0);
  });

  it('字符串数组本身不是数组 → fatal', () => {
    const verdict = validateImportedSettings({ fallback_models: 'glm-5' });
    expect(verdict.fatal.length).toBeGreaterThan(0);
  });
});

describe('writePreviewHistory · 去重与截断（导入提示的增量靠它算）', () => {
  const store: Record<string, string> = {};
  const set = (k: string, v: string): void => {
    store[k] = v;
  };

  it('新样例置顶、去旧同值', () => {
    expect(writePreviewHistory(['a', 'b'], 'b', set)).toEqual(['b', 'a']);
  });

  it('**超过上限就截断**（导入报增量时要考虑这点）', () => {
    let list: string[] = [];
    for (let i = 0; i < 12; i += 1) list = writePreviewHistory(list, `s${i}`, set);
    expect(list).toHaveLength(8);
    expect(list[0]).toBe('s11'); // 最新的在前
  });

  it('重复样例不增加长度（增量算得出 0）', () => {
    const base = ['a', 'b'];
    const next = writePreviewHistory(base, 'a', set);
    expect(next).toHaveLength(base.length);
    expect(next.filter((x) => !base.includes(x))).toHaveLength(0);
  });
});

describe('parseRulesCsvDetailed · 报出哪一行', () => {
  it('正常两列 CSV（带表头）→ ok，表头被跳过', () => {
    const r = parseRulesCsvDetailed('原文,改写为\n甲,A\n乙,B');
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.rules).toEqual([
      { from: '甲', to: 'A' },
      { from: '乙', to: 'B' },
    ]);
  });

  it('**列数不对时报出是第几行**（只说「格式不对」等于让用户自己找）', () => {
    const r = parseRulesCsvDetailed('甲,A\n乙,B,多余\n丙,C');
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.line).toBe(2);
      expect(r.reason).toContain('2');
      expect(r.reason).toContain('列');
    }
  });

  it('原文为空时也报行号', () => {
    const r = parseRulesCsvDetailed('甲,A\n,B\n丙,C');
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.line).toBe(2);
      expect(r.reason).toContain('原文为空');
    }
  });

  it('空文件 → ok 且零条（不是「格式不对」）', () => {
    const r = parseRulesCsvDetailed('');
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.rules).toEqual([]);
  });

  it('兼容层仍返回 null（既有调用方与测试不受影响）', () => {
    expect(parseRulesCsv('甲,A\n乙,B,多余')).toBeNull();
  });
});

describe('batchDropNote', () => {
  const tr = (key: string, params?: Record<string, string | number>): string =>
    params ? key.replace(/\{\{(\w+)\}\}/g, (_m, k: string) => String(params[k] ?? '')) : key;

  it('没丢行 → null（不弹多余提示）', () => {
    expect(batchDropNote([], tr)).toBeNull();
  });

  it('**报出行号**（只说「跳了 2 行」用户找不到是哪两行）', () => {
    const note = batchDropNote(
      [
        { line: 2, reason: 'no-separator' },
        { line: 5, reason: 'no-separator' },
      ],
      tr,
    );
    expect(note).toContain('2');
    expect(note).toContain('5');
    expect(note).toContain('缺少分隔符');
  });

  it('**重复行也要报**（原先它连计数都不进，彻底静默）', () => {
    const note = batchDropNote([{ line: 3, reason: 'duplicate' }], tr);
    expect(note).toContain('重复');
    expect(note).toContain('3');
  });

  it('两类都有时分开说（合并成一句会看不出各几行）', () => {
    const note = batchDropNote(
      [
        { line: 1, reason: 'no-separator' },
        { line: 4, reason: 'duplicate' },
      ],
      tr,
    );
    expect(note).toContain('缺少分隔符');
    expect(note).toContain('重复');
    expect(note).toContain('；');
  });

  it('超过 5 行只列前 5 个行号并加省略号（列全反而看不出结构）', () => {
    const many = Array.from({ length: 8 }, (_, i) => ({
      line: i + 1,
      reason: 'no-separator' as const,
    }));
    const note = batchDropNote(many, tr);
    expect(note).toContain('8');
    expect(note).toContain('…');
    expect(note).not.toContain('、7');
  });
});

describe('STATS_CSV_HELP', () => {
  it('**列名与顺序必须与后端 STATS_CSV_COLUMNS 一致**（缺一列或换序都会被这测抓到）', () => {
    // 后端定义在 src/purge/mod.rs；这里直接读源码而不是抄一份，避免两处漂移
    const mod = fs.readFileSync(path.resolve(__dirname, '../../../src/purge/mod.rs'), 'utf8');
    const block = mod.slice(mod.indexOf('STATS_CSV_COLUMNS'));
    const cols: string[] = [];
    for (const m of block.matchAll(/"([a-z_]+)",/g)) {
      cols.push(m[1]);
      if (cols.length >= 13) break;
    }
    expect(cols).toHaveLength(13);
    expect(STATS_CSV_HELP.map((h) => h.column)).toEqual(cols);
  });

  it('每一列都有中文说明（不留空标签）', () => {
    for (const item of STATS_CSV_HELP) {
      expect(item.label.trim().length).toBeGreaterThan(0);
    }
  });

  it('lower_bound 的说明要说清「1 表示什么」（否则这列等于没解释）', () => {
    const lower = STATS_CSV_HELP.find((h) => h.column === 'lower_bound');
    expect(lower?.label).toContain('1');
    expect(lower?.label).toContain('少算');
  });
});

describe('previewHistoryClearPrompt', () => {
  const tr = (key: string, params?: Record<string, string | number>): string =>
    params ? key.replace(/\{\{(\w+)\}\}/g, (_m, k: string) => String(params[k] ?? '')) : key;

  it('**报出条数**（只说「确定清空吗」等于让人盲签）', () => {
    const prompt = previewHistoryClearPrompt(3, tr);
    expect(prompt).toContain('3');
    expect(prompt).toContain('无法恢复');
    // 明确告诉用户「不影响已保存的配置」，否则会以为配置也一起没了
    expect(prompt).toContain('不影响已保存的配置');
  });

  it('空历史 → 不弹确认（没什么可清）', () => {
    expect(previewHistoryClearPrompt(0, tr)).toBeNull();
  });

  it('单条也要问（一条也是不可逆的删除）', () => {
    expect(previewHistoryClearPrompt(1, tr)).toContain('1');
  });
});

describe('summaryTextWithOff', () => {
  const tr = (key: string, params?: Record<string, string | number>): string =>
    params ? key.replace(/\{\{(\w+)\}\}/g, (_m, k: string) => String(params[k] ?? '')) : key;

  it('只有生效项 → 一个章节（不硬塞空章节）', () => {
    const text = summaryTextWithOff([{ key: 'system 归一化' }], [], tr);
    expect(text).toContain('## 正在生效');
    expect(text).toContain('- system 归一化');
    expect(text).not.toContain('配了但没启用');
  });

  it('**带上白配项**（贴给别人看时不能漏）', () => {
    const text = summaryTextWithOff(
      [{ key: 'system 归一化' }],
      [{ code: 'dead_zone_off_with_rules', label: '死区词改写', count: 3, anchor: 'dead-zone-table' }],
      tr,
    );
    expect(text).toContain('## 配了但没启用');
    expect(text).toContain('死区词改写（3 项）');
    // 两个章节顺序：先生效、后白配
    expect(text.indexOf('## 正在生效')).toBeLessThan(text.indexOf('## 配了但没启用'));
  });

  it('每一项都占一行（便于贴进工单/群聊）', () => {
    const text = summaryTextWithOff(
      [{ key: 'a' }, { key: 'b' }],
      [{ code: 'x', label: 'L', count: 1, anchor: 'a' }],
      tr,
    );
    const lines = text.split('\n');
    expect(lines.filter((l) => l.startsWith('- '))).toHaveLength(3);
  });
});

describe('configuredButOff', () => {
  const base = {
    dead_zone_rewrite: true,
    synonym_enabled: true,
    cross_source_fallback: true,
    skill_inject: true,
    structured_prompt: true,
    auto_preset: true,
    reduce_reasoning: true,
    reasoning_floor: 'low',
    rewrite_rules: [],
    synonym_pairs: [],
    fallback_models: [],
    skill_dir: '',
    structured_templates: [],
    structured_suffix: '',
    auto_preset_id: '',
  } as Record<string, unknown>;

  it('全部启用 / 全空 → 空数组（不打扰）', () => {
    expect(configuredButOff(base as unknown as PurgeSettings)).toEqual([]);
  });

  it('**关着但配了内容** → 逐项列出并带 count 与锚点', () => {
    const settings = {
      ...base,
      dead_zone_rewrite: false,
      rewrite_rules: [{ from: 'a', to: 'b' }, { from: 'c', to: 'd' }],
      synonym_enabled: false,
      synonym_pairs: [{ from: 'x', to: 'y' }],
      cross_source_fallback: false,
      fallback_models: ['m1', 'm2', 'm3'],
    } as Record<string, unknown>;
    const items = configuredButOff(settings as unknown as PurgeSettings);
    expect(items.map((i) => i.code)).toEqual([
      'dead_zone_off_with_rules',
      'synonym_off_with_pairs',
      'fallback_off_with_models',
    ]);
    expect(items[0].count).toBe(2);
    expect(items[1].count).toBe(1);
    expect(items[2].count).toBe(3);
    expect(items[0].anchor).toBe('dead-zone-table');
  });

  it('开着但配置为空 → 不提（没有可「白配」的东西）', () => {
    const settings = {
      ...base,
      dead_zone_rewrite: false,
      synonym_enabled: false,
      rewrite_rules: [],
      synonym_pairs: [],
    } as Record<string, unknown>;
    expect(configuredButOff(settings as unknown as PurgeSettings)).toEqual([]);
  });

  it('技能注入关着但目录有值 → 提（目录配了就等于打算用）', () => {
    const settings = {
      ...base,
      skill_inject: false,
      skill_dir: '~/.aigx/skills',
    } as unknown as never;
    const items = configuredButOff(settings as unknown as PurgeSettings);
    expect(items.map((i) => i.code)).toContain('skill_inject_off');
    // 只有目录、没有关键词数（`skill_keywords` 在技能快照里，不在 settings）
    expect(items.find((i) => i.code === 'skill_inject_off')?.anchor).toBe('skill-dir');
  });

  it('技能注入关着且目录为空 → 不提（没配东西）', () => {
    const settings = {
      ...base,
      skill_inject: false,
      skill_dir: '   ',
    } as unknown as never;
    expect(configuredButOff(settings as unknown as PurgeSettings)).toEqual([]);
  });

  it('自动套用关着但预设 id 有值 → 提', () => {
    const settings = {
      ...base,
      auto_preset: false,
      auto_preset_id: 'some-preset',
    } as unknown as never;
    expect(configuredButOff(settings as unknown as PurgeSettings).map((i) => i.code)).toContain(
      'auto_preset_off',
    );
  });

  it('自动套用关着且预设 id 为空/空白 → 不提', () => {
    for (const id of ['', '   ']) {
      const settings = {
        ...base,
        auto_preset: false,
        auto_preset_id: id,
      } as unknown as never;
      expect(configuredButOff(settings as unknown as PurgeSettings)).toEqual([]);
    }
  });

  it('降思考关着但 target 是 low → 不提（low 是默认值，没有「白配」）', () => {
    const settings = {
      ...base,
      reduce_reasoning: false,
      reasoning_floor: 'low',
    } as Record<string, unknown>;
    expect(configuredButOff(settings as unknown as PurgeSettings)).toEqual([]);
  });

  it('降思考关着且 target 非 low → 提（用户配了档位却没开开关）', () => {
    const settings = {
      ...base,
      reduce_reasoning: false,
      reasoning_floor: 'medium',
    } as Record<string, unknown>;
    expect(configuredButOff(settings as unknown as PurgeSettings).map((i) => i.code)).toContain('reduce_reasoning_off');
  });

  it('结构化提示关着但有固定后缀 / 模板 → 提', () => {
    const withSuffix = {
      ...base,
      structured_prompt: false,
      structured_suffix: '请给结论',
    } as Record<string, unknown>;
    expect(configuredButOff(withSuffix as unknown as PurgeSettings).map((i) => i.code)).toContain('structured_off');
    const withTpl = {
      ...base,
      structured_prompt: false,
      structured_templates: [{ name: 't', text: 'x' }],
    } as Record<string, unknown>;
    expect(configuredButOff(withTpl as unknown as PurgeSettings).map((i) => i.code)).toContain('structured_off');
  });
});

describe('buildConfigExport', () => {
  const settings = { enabled: true, retry_limit: 8 } as never;
  const meta = { exported_at: '2026-11-25T00:00:00.000Z', pid: 42, upstream: 'e2e' };

  it('带上导出时刻与网关身份（多环境迁移能判断新旧）', () => {
    const parsed = JSON.parse(buildConfigExport(settings, meta)) as {
      version: number;
      exported_at: string;
      runtime: { pid: number; upstream: string };
      settings: unknown;
    };
    expect(parsed.version).toBe(1);
    expect(parsed.exported_at).toBe(meta.exported_at);
    expect(parsed.runtime).toEqual({ pid: 42, upstream: 'e2e' });
    expect(parsed.settings).toEqual(settings);
  });

  it('以换行结尾（与其他导出文件一致，便于 diff）', () => {
    expect(buildConfigExport(settings, meta).endsWith('\n')).toBe(true);
  });

  it('元信息不影响导入：新旧文件都能被校验放行', () => {
    // 新文件（含 exported_at/runtime）
    expect(validateImportedSettings(settings).fatal).toEqual([]);
    // 旧文件（只有 version+settings）同样能过——元信息是线索，不是校验条件
    const old = JSON.parse(buildConfigExport(settings, meta)) as Record<string, unknown>;
    delete old.exported_at;
    delete old.runtime;
    expect(validateImportedSettings(old.settings).fatal).toEqual([]);
  });
});

describe('normalizeBooleanFields', () => {
  it('**字符串 "false" 必须变成 false**（`Boolean("false")` 是 true）', () => {
    expect(normalizeBooleanFields({ enabled: 'false' }).enabled).toBe(false);
    expect(normalizeBooleanFields({ enabled: '0' }).enabled).toBe(false);
  });

  it('认可的真值写法归一为 true', () => {
    for (const v of [true, 1, 'true', 'TRUE', ' yes ', 'on', '1']) {
      expect(normalizeBooleanFields({ enabled: v }).enabled).toBe(true);
    }
  });

  it('布尔值原样保留（不做无谓转换）', () => {
    expect(normalizeBooleanFields({ enabled: true, skill_inject: false })).toMatchObject({
      enabled: true,
      skill_inject: false,
    });
  });

  it('缺省字段不塞值（旧配置缺键时保持 undefined）', () => {
    expect('enabled' in normalizeBooleanFields({})).toBe(false);
  });

  it('非字符串/非数字/非布尔 → false（宁关不开）', () => {
    expect(normalizeBooleanFields({ enabled: null }).enabled).toBe(false);
    expect(normalizeBooleanFields({ enabled: {} }).enabled).toBe(false);
  });

  it('其余字段原样带过', () => {
    const out = normalizeBooleanFields({ enabled: 'true', retry_limit: 7 });
    expect(out.retry_limit).toBe(7);
  });
});

describe('importOverwriteItems', () => {
  const saved = { enabled: true, retry_limit: 8 } as never;
  const draft = { enabled: true, retry_limit: 9 } as never;

  it('有未保存改动 → 返回差异项（调用方据此确认）', () => {
    const items = importOverwriteItems(saved, draft, true);
    expect(items.length).toBeGreaterThan(0);
    // key 是 i18n 模板、字段名在 params.label 里（不是 key 文本）
    expect(items.some((item) => item.params?.label === '重试上限')).toBe(true);
    expect(items.some((item) => item.params?.from === '8' && item.params?.to === '9')).toBe(true);
  });

  it('**不 dirty 时不问**（没有会被覆盖的东西）', () => {
    expect(importOverwriteItems(saved, draft, false)).toEqual([]);
  });

  it('草稿或快照缺失时不问（还没加载完，没有可比对象）', () => {
    expect(importOverwriteItems(null, draft, true)).toEqual([]);
    expect(importOverwriteItems(saved, null, true)).toEqual([]);
  });

  it('dirty 但两值相同 → 空数组（不弹无意义确认）', () => {
    expect(importOverwriteItems(saved, saved, true)).toEqual([]);
  });
});

describe('confirmSkillOverwrite', () => {
  it('确认文案点名具体技能（不笼统问「要覆盖吗」）', () => {
    const asked: string[] = [];
    confirmSkillOverwrite('my-skill', (m) => {
      asked.push(m);
      return true;
    }, (key, params) => `${key}|${params?.id ?? ''}`);
    expect(asked).toHaveLength(1);
    expect(asked[0]).toContain('my-skill');
    expect(asked[0]).toContain('SKILL.md');
  });

  it('用户取消 → 返回 false（调用方据此中止写入）', () => {
    expect(confirmSkillOverwrite('x', () => false)).toBe(false);
  });

  it('用户确认 → 返回 true', () => {
    expect(confirmSkillOverwrite('x', () => true)).toBe(true);
  });
});

describe('swapRules', () => {
  const rules = [{ from: 'a' }, { from: 'b' }, { from: 'c' }];

  it('上移：与上一条互换', () => {
    expect(swapRules(rules, 1, -1).map((r) => r.from)).toEqual(['b', 'a', 'c']);
  });

  it('下移：与下一条互换', () => {
    expect(swapRules(rules, 1, 1).map((r) => r.from)).toEqual(['a', 'c', 'b']);
  });

  it('首行上移 / 末行下移 → 原样返回（不越界、不报错）', () => {
    expect(swapRules(rules, 0, -1).map((r) => r.from)).toEqual(['a', 'b', 'c']);
    expect(swapRules(rules, 2, 1).map((r) => r.from)).toEqual(['a', 'b', 'c']);
  });

  it('返回新数组（不改原数组，避免就地变异掩盖问题）', () => {
    const next = swapRules(rules, 0, 1);
    expect(next).not.toBe(rules);
    expect(rules.map((r) => r.from)).toEqual(['a', 'b', 'c']);
  });
});

describe('ruleRowHint', () => {
  const tr = (k: string): string => k;

  it('空原文优先于重复（报对原因，修法才对）', () => {
    // 两个空行互为重复，但真问题是「没填」
    expect(ruleRowHint({ from: '  ', to: 'x' }, true, tr)).toContain('原文未填');
  });

  it('空改写提示「保存删词、整理删行」两件事区分开', () => {
    const hint = ruleRowHint({ from: 'a', to: ' ' }, false, tr);
    expect(hint).toContain('从消息里删掉');
    expect(hint).toContain('整理');
  });

  it('正常行重复 → 报重复', () => {
    expect(ruleRowHint({ from: 'a', to: 'b' }, true, tr)).toContain('重复');
  });

  it('正常且不重复 → undefined（不留空提示）', () => {
    expect(ruleRowHint({ from: 'a', to: 'b' }, false, tr)).toBeUndefined();
  });
});

describe('lastRunNote', () => {
  const tr = (key: string, params?: Record<string, string | number>): string =>
    params ? key.replace(/\{\{(\w+)\}\}/g, (_m, k: string) => String(params[k] ?? '')) : key;

  it('没有快照 → 说明「没关机过」，而不是留一排 0 让人猜', () => {
    expect(lastRunNote(null, tr)).toContain('还没关机过');
    expect(lastRunNote({ saved_at: 0, uptime_secs: 99 }, tr)).toContain('还没关机过');
  });

  it('有快照 → 给出时刻与那次运行时长（小时档）', () => {
    const note = lastRunNote({ saved_at: 1_780_000_000, uptime_secs: 3 * 3600 + 25 * 60 }, tr);
    expect(note).toContain('上次运行快照');
    expect(note).toContain('3 小时 25 分');
  });

  it('不足一小时走分钟档', () => {
    const note = lastRunNote({ saved_at: 1_780_000_000, uptime_secs: 42 * 60 }, tr);
    expect(note).toContain('42 分钟');
    expect(note).not.toContain('小时');
  });
});

describe('ruleIssues', () => {
  it('空原文 / 空改写 / 重复 各自标记，且判据与 tidyRules 一致', () => {
    const issues = ruleIssues([
      { from: 'a', to: 'x' },
      { from: '   ', to: 'y' }, // emptyFrom
      { from: 'b', to: '  ' }, // emptyTo
      { from: 'A', to: 'z' }, // 与第 0 条重复（trim+忽略大小写）
    ]);
    expect(issues).toEqual([
      { index: 1, reason: 'emptyFrom' },
      { index: 2, reason: 'emptyTo' },
      { index: 3, reason: 'duplicate' },
    ]);
  });

  it('同一个 from 出现三次：只标后面两条（tidyRules 保留最早一条）', () => {
    const issues = ruleIssues([
      { from: 'k', to: '1' },
      { from: 'k', to: '2' },
      { from: 'k', to: '3' },
    ]);
    expect(issues.map((i) => i.index)).toEqual([1, 2]);
  });

  it('干净表 → 空（不误报）', () => {
    expect(ruleIssues([{ from: 'a', to: 'b' }, { from: 'c', to: 'd' }])).toEqual([]);
  });

  it('标记的条数等于 tidyRules 将删除的条数（两把刀必须同口径）', () => {
    const rules = [
      { from: 'a', to: 'b' },
      { from: 'a', to: 'c' },
      { from: '', to: 'd' },
      { from: 'e', to: '' },
      { from: 'f', to: 'g' },
    ];
    const tidy = tidyRules(rules);
    const tidyRemoved =
      tidy.droppedEmptyFrom + tidy.droppedEmptyTo + tidy.droppedDuplicate;
    expect(ruleIssues(rules).length).toBe(tidyRemoved);
  });
});

describe('filterRules', () => {
  const rules = [
    { from: 'Alpha', to: 'x' },
    { from: 'b', to: 'Beta' },
    { from: 'gamma', to: 'y' },
  ];

  it('空关键词返回全部（保持原序与原下标）', () => {
    expect(filterRules(rules, '')).toEqual([
      { rule: rules[0], index: 0 },
      { rule: rules[1], index: 1 },
      { rule: rules[2], index: 2 },
    ]);
    expect(filterRules(rules, '   ').length).toBe(3);
  });

  it('大小写不敏感，原文与改写都匹配', () => {
    expect(filterRules(rules, 'alpha').map((i) => i.index)).toEqual([0]);
    expect(filterRules(rules, 'BETA').map((i) => i.index)).toEqual([1]);
  });

  it('**保留原始下标**（过滤后编辑仍写真实位置）', () => {
    const hit = filterRules(rules, 'gamma')[0];
    expect(hit.index).toBe(2);
    expect(hit.rule).toBe(rules[2]);
  });

  it('无命中 → 空数组', () => {
    expect(filterRules(rules, 'zzz')).toEqual([]);
  });
});

describe('rulesToText', () => {
  it('Tab 分隔、一行一条（能直接粘进 Excel 两列）', () => {
    expect(
      rulesToText([
        { from: '甲', to: 'A' },
        { from: '乙', to: 'B' },
      ]),
    ).toBe('甲\tA\n乙\tB');
  });

  it('输出能被 parseBatchRules 原样吃回去（表间搬运的闭环）', () => {
    const rules = [
      { from: '甲', to: 'A' },
      { from: 'b', to: 'B' },
    ];
    const { rules: back, skipped } = parseBatchRules(rulesToText(rules));
    expect(back).toEqual(rules);
    expect(skipped).toBe(0);
  });

  it('空表 → 空串', () => {
    expect(rulesToText([])).toBe('');
  });

  it('保留空改写（from→空 是「删掉这个词」的有效语义）', () => {
    expect(rulesToText([{ from: 'x', to: '' }])).toBe('x\t');
  });
});

describe('countEmptyFromRules', () => {
  it('只数空原文（trim 后为空），空改写不算', () => {
    const out = countEmptyFromRules({
      rewrite_rules: [{ from: 'a', to: 'b' }, { from: '   ', to: 'c' }, { from: '', to: 'd' }],
      synonym_pairs: [{ from: 'x', to: '' }],
    });
    expect(out).toEqual({ deadZone: 2, synonym: 0 });
  });

  it('缺省 from 也算空（旧数据/手写 JSON 的情况）', () => {
    const out = countEmptyFromRules({ rewrite_rules: [{ to: 'b' }], synonym_pairs: [] });
    expect(out.deadZone).toBe(1);
  });

  it('两张表各自统计', () => {
    const out = countEmptyFromRules({
      rewrite_rules: [{ from: '' }],
      synonym_pairs: [{ from: ' ' }, { from: '' }],
    });
    expect(out).toEqual({ deadZone: 1, synonym: 2 });
  });
});

describe('collectCapOverflows', () => {
  const base = {
    rewrite_rules: [] as unknown[],
    synonym_pairs: [] as unknown[],
    fallback_models: [] as unknown[],
    normalize_strip_prefixes: [] as unknown[],
  };

  it('全部未超限 → 空（不弹无意义确认）', () => {
    expect(collectCapOverflows(base)).toEqual([]);
  });

  it('逐个字段各自报自己的上限（不是统一一个数）', () => {
    const out = collectCapOverflows({
      ...base,
      rewrite_rules: Array(201).fill({}),
      fallback_models: Array(17).fill('m'),
      structured_templates: Array(33).fill({}),
    });
    expect(out.map((o) => `${o.key}:${o.n}/${o.cap}`)).toEqual([
      'rewrite_rules:201/200',
      'fallback_models:17/16',
      'structured_templates:33/32',
    ]);
  });

  it('恰好等于上限 → 不算超限（边界不误报）', () => {
    expect(collectCapOverflows({ ...base, rewrite_rules: Array(200).fill({}) })).toEqual([]);
    expect(collectCapOverflows({ ...base, fallback_models: Array(16).fill('m') })).toEqual([]);
  });

  it('可选字段缺省时按空数组处理（旧后端不返回也不误报）', () => {
    const out = collectCapOverflows({ ...base, rewrite_rules: Array(205).fill({}) });
    expect(out).toHaveLength(1);
  });
});

describe('undoSnapshot / applyUndo', () => {
  const rules = [{ from: 'a', to: 'b' }];

  it('快照拷贝数组（后续追加不会污染快照）', () => {
    const snap = undoSnapshot('dead_zone', rules, '整理词表');
    rules.push({ from: 'c', to: 'd' });
    expect(snap.rules).toHaveLength(1);
    rules.pop();
  });

  it('撤销把快照写回对应表，另一张表不动', () => {
    const draft = {
      rewrite_rules: [{ from: 'x', to: 'y' }],
      synonym_pairs: [{ from: 'p', to: 'q' }],
    } as never;
    const back = applyUndo(draft, undoSnapshot('dead_zone', rules, '整理词表'));
    expect(back.rewrite_rules).toEqual(rules);
    expect(back.synonym_pairs).toEqual([{ from: 'p', to: 'q' }]);

    const back2 = applyUndo(draft, undoSnapshot('synonym', rules, '批量粘贴'));
    expect(back2.synonym_pairs).toEqual(rules);
    expect(back2.rewrite_rules).toEqual([{ from: 'x', to: 'y' }]);
  });
});

describe('normalizeSettingsArrays', () => {
  it('五个数组字段非数组时兜成 []，其余字段原样保留', () => {
    const input = {
      enabled: true,
      rewrite_rules: 'oops',
      synonym_pairs: null,
      structured_templates: { a: 1 },
      normalize_strip_prefixes: undefined,
      fallback_models: 42,
    } as never;
    const out = normalizeSettingsArrays(input);
    expect(out.rewrite_rules).toEqual([]);
    expect(out.synonym_pairs).toEqual([]);
    expect(out.structured_templates).toEqual([]);
    expect(out.normalize_strip_prefixes).toEqual([]);
    expect(out.fallback_models).toEqual([]);
    expect(out.enabled).toBe(true);
  });

  it('已是数组时**保持原引用内容**（不复制、不改内容）', () => {
    const rules = [{ from: 'a', to: 'b' }];
    const out = normalizeSettingsArrays({ rewrite_rules: rules } as never);
    expect(out.rewrite_rules).toEqual(rules);
    expect(out.rewrite_rules[0]).toBe(rules[0]);
  });
});

describe('retryRateText', () => {
  it('正常数字 → 两位小数百分比', () => {
    expect(retryRateText(200, 3)).toBe('1.50');
    expect(retryRateText(0, 0)).toBe('0.00');
  });

  it('responses/retries 不是有效数字 → null（显示 — 而不是 0.00%）', () => {
    // 关键：`undefined > 0` 是 false，旧写法会算出 '0.00' —— 把「没数据」
    // 显示成「0% 重试」，正好是最容易被误读的那个结论
    expect(retryRateText(undefined, 3)).toBeNull();
    expect(retryRateText('oops', 3)).toBeNull();
    expect(retryRateText(200, null)).toBeNull();
    expect(retryRateText(Number.NaN, 1)).toBeNull();
  });
});

describe('snapshotReportText', () => {
  const tr = (k: string): string => k;
  const base = {
    runtime: { pid: 42, uptime_secs: 90, upstream: 'relay-a' },
    activeItems: ['死区词改写（3 条启用）'],
    warnings: [],
    stats: { responses: 12, retries: 1 },
    translate: tr,
  };

  it('四段齐全且顺序固定：运行 → 生效项 → 体检 → 统计', () => {
    const text = snapshotReportText({ ...base, baselineAt: 1_780_000_000 });
    const order = ['## 运行', '## 当前生效项', '## 配置体检', '## 统计'].map((h) => text.indexOf(h));
    expect(order.every((i) => i >= 0)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  it('运行段带 PID/运行时长/上游，有基线时多一行', () => {
    const withBaseline = snapshotReportText({ ...base, baselineAt: 1_780_000_000 });
    expect(withBaseline).toContain('PID 42');
    expect(withBaseline).toContain('90s');
    expect(withBaseline).toContain('relay-a');
    expect(withBaseline).toContain('统计基线');
    const noBaseline = snapshotReportText(base);
    expect(noBaseline).not.toContain('统计基线');
  });

  it('无告警 → 体检段写「通过」；有告警 → 带级别前缀', () => {
    expect(snapshotReportText(base)).toContain('通过：没有「开了但不生效」或自相矛盾的项目');
    const withWarn = snapshotReportText({
      ...base,
      warnings: [{ message: '死区词表为空', severity: 'warn' }],
    });
    expect(withWarn).toContain('- [warn] 死区词表为空');
  });

  it('空生效项/空统计 → 写「（无）」而不是留空段', () => {
    const text = snapshotReportText({ ...base, activeItems: [], stats: {} });
    expect(text).toContain('- （无）');
  });

  it('统计按后端键序输出（不排序，避免与页面卡片顺序错位）', () => {
    const text = snapshotReportText({ ...base, stats: { responses: 5, retries: 2, refusals: 1 } });
    expect(text.indexOf('- responses: 5')).toBeLessThan(text.indexOf('- retries: 2'));
    expect(text.indexOf('- retries: 2')).toBeLessThan(text.indexOf('- refusals: 1'));
  });
});

describe('warningReportText', () => {
  const tr = (k: string): string => k;

  it('按严重级分组，同级保持原序', () => {
    const text = warningReportText(
      [
        { message: 'info-1', severity: 'info' },
        { message: 'error-1', severity: 'error' },
        { message: 'error-2', severity: 'error' },
        { message: 'warn-1', severity: 'warn' },
      ],
      tr,
    );
    expect(text).toBe('## 不生效\n- error-1\n- error-2\n## 需确认\n- warn-1\n## 提示\n- info-1');
  });

  it('未声明 severity 按 error 分组（与后端兜底同立场）', () => {
    const text = warningReportText([{ message: 'legacy' }], tr);
    expect(text).toContain('## 不生效');
    expect(text).toContain('- legacy');
  });

  it('无告警 → 给「已通过」结论，而不是空串', () => {
    const text = warningReportText([], tr);
    expect(text).not.toBe('');
    expect(text).toContain('配置体检通过');
  });

  it('只有某一级时不输出空分组标题', () => {
    const text = warningReportText([{ message: 'only-warn', severity: 'warn' }], tr);
    expect(text).toBe('## 需确认\n- only-warn');
  });
});

describe('findBodySnippet', () => {
  const body = 'A'.repeat(100) + 'NeedleHere' + 'B'.repeat(100);

  it('大小写不敏感定位，但返回原文切片（不改作者写法）', () => {
    const s = findBodySnippet(body, 'needlehere');
    expect(s?.hit).toBe('NeedleHere');
  });

  it('取前后上下文并加省略号标记截断', () => {
    const s = findBodySnippet(body, 'needlehere', 10);
    expect(s?.before).toBe('…' + 'A'.repeat(10));
    expect(s?.after).toBe('B'.repeat(10) + '…');
  });

  it('命中在开头/结尾时不加多余省略号', () => {
    expect(findBodySnippet('needle tail', 'needle')?.before).toBe('');
    expect(findBodySnippet('head needle', 'needle')?.after).toBe('');
  });

  it('空检索词 / 空正文 / 未命中 → null', () => {
    expect(findBodySnippet(body, '   ')).toBeNull();
    expect(findBodySnippet('', 'x')).toBeNull();
    expect(findBodySnippet(body, 'zzz-not-there')).toBeNull();
  });
});

describe('diffSettings', () => {
  const base = {
    enabled: true,
    system_normalize: false,
    dead_zone_rewrite: false,
    synonym_enabled: false,
    reduce_reasoning: false,
    cross_source_fallback: false,
    skill_inject: false,
    structured_prompt: false,
    reasoning_floor: 'low',
    retry_limit: 8,
    fallback_trigger_attempt: 3,
    fallback_models: [],
    rewrite_rules: [],
    synonym_pairs: [],
    normalize_strip_prefixes: [],
    skill_dir: '',
    skill_inject_limit: 4000,
    structured_suffix: '',
    structured_templates: [],
    active_template: '',
    auto_preset: false,
    auto_preset_id: '',
    upstream_label: '',
  };

  it('完全相同 → 空数组', () => {
    expect(diffSettings(base, { ...base })).toEqual([]);
  });

  it('布尔/数字/字符串各报一条，带 from→to', () => {
    const items = diffSettings(base, {
      ...base,
      enabled: false,
      retry_limit: 12,
      reasoning_floor: 'medium',
    });
    expect(items).toHaveLength(3);
    const joined = items.map((i) => JSON.stringify(i.params)).join();
    expect(joined).toContain('"from":"开"');
    expect(joined).toContain('"to":"12"');
    expect(joined).toContain('"to":"medium"');
  });

  it('词表按条数+启用数报告（含停用行不计入启用）', () => {
    const items = diffSettings(base, {
      ...base,
      rewrite_rules: [
        { from: 'a', to: 'x' },
        { from: 'b', to: 'y', enabled: false },
      ],
    });
    expect(items).toHaveLength(1);
    expect(items[0].params).toMatchObject({ fromN: 0, fromE: 0, toN: 2, toE: 1 });
  });

  it('模板按名称集合比较（改名算变化）', () => {
    const saved = { ...base, structured_templates: [{ name: 'A', text: 't' }] };
    const draft = { ...base, structured_templates: [{ name: 'B', text: 't' }] };
    expect(diffSettings(saved, draft)).toHaveLength(1);
    // 仅改正文、名称不变 → 不报（模板正文编辑不在这条口径里）
    expect(
      diffSettings(saved, { ...base, structured_templates: [{ name: 'A', text: 'changed' }] }),
    ).toEqual([]);
  });

  it('异源模型逐个比较', () => {
    const items = diffSettings(
      { ...base, fallback_models: ['a', 'b'] },
      { ...base, fallback_models: ['a', 'c'] },
    );
    expect(items).toHaveLength(1);
    expect(items[0].params).toEqual({ from: 'a,b', to: 'a,c' });
  });
});

describe('tidyRules / tidySummary', () => {
  const rule = (from: string, to: string, enabled?: boolean) => ({ from, to, enabled });

  it('删空 from / 空 to / 重复（保留最早），并分别计数', () => {
    const result = tidyRules([
      rule('a', 'x'),
      rule('  ', 'y'), // 空 from
      rule('b', '   '), // 空 to
      rule('A', 'z'), // 与 a 重复（大小写不敏感）
      rule('c', 'w'),
    ]);
    expect(result.kept.map((r) => r.from)).toEqual(['a', 'c']);
    expect(result.droppedEmptyFrom).toBe(1);
    expect(result.droppedEmptyTo).toBe(1);
    expect(result.droppedDuplicate).toBe(1);
    expect(tidySummary(result).removed).toBe(3);
  });

  it('干净表：removed = 0', () => {
    const summary = tidySummary(tidyRules([rule('a', 'x'), rule('b', 'y')]));
    expect(summary.removed).toBe(0);
  });

  it('不改动入参数组', () => {
    const input = [rule('a', 'x'), rule('a', 'y')];
    tidyRules(input);
    expect(input.length).toBe(2);
  });
});

describe('exportPreviewHistory / parsePreviewHistoryJson', () => {
  it('导出再导回等价（保留原始空白）', () => {
    const samples = ['第一行\n第二行', '单行样例'];
    const back = parsePreviewHistoryJson(exportPreviewHistory(samples));
    expect(back).toEqual(samples);
  });

  it('接受裸数组与单字符串', () => {
    expect(parsePreviewHistoryJson('["a","b"]')).toEqual(['a', 'b']);
    expect(parsePreviewHistoryJson('"only"')).toEqual(['only']);
  });

  it('去重保序、跳过空与非字符串、截到 8 条', () => {
    const many = Array.from({ length: 12 }, (_, i) => `s${i}`);
    const parsed = parsePreviewHistoryJson(JSON.stringify({ samples: ['a', 'a', '', 123, ...many] }));
    expect(parsed?.[0]).toBe('a');
    expect(parsed?.length).toBe(8);
    expect(parsed?.filter((s) => s === 'a').length).toBe(1);
  });

  it('坏 JSON / 结构不符 → null（与「空文件」区分）', () => {
    expect(parsePreviewHistoryJson('not json')).toBeNull();
    expect(parsePreviewHistoryJson('{"foo":1}')).toBeNull();
    expect(parsePreviewHistoryJson('{"samples":[]}')).toEqual([]);
  });

  it('超长单条被跳过（与写入路径同一上限）', () => {
    const long = 'x'.repeat(5000);
    expect(parsePreviewHistoryJson(JSON.stringify({ samples: [long, 'ok'] }))).toEqual(['ok']);
  });
});

describe('readPresetFilter / writePresetFilter', () => {
  const store = new Map<string, string>();
  const set = (k: string, v: string): void => {
    store.set(k, v);
  };
  const remove = (k: string): void => {
    store.delete(k);
  };

  it('读回写入的筛选', () => {
    store.clear();
    writePresetFilter({ q: 'glm', vendor: 'zhipu' }, set, remove);
    expect(readPresetFilter((k) => store.get(k) ?? null)).toEqual({
      q: 'glm',
      vendor: 'zhipu',
    });
  });

  it('两项都空 → 移除键，不留垃圾', () => {
    store.clear();
    writePresetFilter({ q: 'x', vendor: 'y' }, set, remove);
    writePresetFilter({ q: '', vendor: '' }, set, remove);
    expect(store.has('aigx.purge.presetFilter')).toBe(false);
    expect(readPresetFilter((k) => store.get(k) ?? null)).toEqual({ q: '', vendor: '' });
  });

  it('坏 JSON / 类型不符 → 空筛选（不拿脏值请求后端）', () => {
    expect(readPresetFilter(() => 'not-json')).toEqual({ q: '', vendor: '' });
    expect(readPresetFilter(() => '[]')).toEqual({ q: '', vendor: '' });
    expect(readPresetFilter(() => '{"q":123,"vendor":null}')).toEqual({ q: '', vendor: '' });
  });

  it('超长搜索词读回时截断（与写入同上限）', () => {
    const long = 'a'.repeat(500);
    store.clear();
    writePresetFilter({ q: long, vendor: '' }, set, remove);
    const back = readPresetFilter((k) => store.get(k) ?? null);
    expect(back.q.length).toBe(200);
  });
});

describe('sortWarningsBySeverity', () => {
  it('Error 在前，Info 在后；同级保持原序（稳定）', () => {
    const items = [
      { code: 'a', severity: 'info' },
      { code: 'b', severity: 'error' },
      { code: 'c', severity: 'warn' },
      { code: 'd', severity: 'error' },
      { code: 'e', severity: 'info' },
    ];
    expect(sortWarningsBySeverity(items).map((i) => i.code)).toEqual([
      'b',
      'd',
      'c',
      'a',
      'e',
    ]);
  });

  it('未声明 severity 按 error 处理（与后端兜底同立场）', () => {
    const items = [{ code: 'x' }, { code: 'y', severity: 'info' }];
    expect(sortWarningsBySeverity(items).map((i) => i.code)).toEqual(['x', 'y']);
  });

  it('不改动入参数组', () => {
    const items = [
      { code: 'a', severity: 'info' },
      { code: 'b', severity: 'error' },
    ];
    sortWarningsBySeverity(items);
    expect(items.map((i) => i.code)).toEqual(['a', 'b']);
  });
});

describe('buildActiveSummary', () => {
  const base = {
    enabled: true,
    system_normalize: false,
    dead_zone_rewrite: false,
    synonym_enabled: false,
    reduce_reasoning: false,
    cross_source_fallback: false,
    skill_inject: false,
    structured_prompt: false,
    reasoning_floor: 'low',
    retry_limit: 8,
    fallback_trigger_attempt: 3,
    fallback_models: [],
    rewrite_rules: [],
    synonym_pairs: [],
    normalize_strip_prefixes: [],
    skill_dir: '',
    skill_inject_limit: 4000,
    structured_suffix: '',
    upstream_label: '',
  };

  it('总开关关闭 → 只出一条「全关」', () => {
    const items = buildActiveSummary({ ...base, enabled: false, system_normalize: true });
    expect(items).toHaveLength(1);
    expect(items[0].key).toContain('总开关关闭');
  });

  it('词表只算启用行（停用行不计入条数）', () => {
    const items = buildActiveSummary({
      ...base,
      dead_zone_rewrite: true,
      rewrite_rules: [
        { from: 'a', to: 'b', enabled: true },
        { from: 'c', to: 'd', enabled: false },
        { from: 'e', to: 'f' },
      ],
    });
    const item = items.find((i) => i.key.includes('死区词改写'));
    expect(item?.params?.n).toBe(2);
  });

  it('开关开着但无启用行 → 明确说出来，不显示 0 条', () => {
    const items = buildActiveSummary({
      ...base,
      synonym_enabled: true,
      synonym_pairs: [{ from: 'a', to: 'b', enabled: false }],
    });
    expect(items.some((i) => i.key.includes('开关开着但无启用行'))).toBe(true);
  });

  it('模板/预设/异源各自带上关键参数', () => {
    const items = buildActiveSummary({
      ...base,
      cross_source_fallback: true,
      fallback_models: ['m1', 'm2'],
      structured_prompt: true,
      active_template: 'T1',
      auto_preset: true,
      auto_preset_id: 'p-9',
      reduce_reasoning: true,
      reasoning_floor: 'medium',
    });
    expect(items.find((i) => i.key.includes('异源递进'))?.params).toEqual({ n: 2, attempt: 3 });
    expect(items.find((i) => i.key.includes('结构化提示'))?.params).toEqual({ name: 'T1' });
    expect(items.find((i) => i.key.includes('按模型套预设'))?.params).toEqual({ id: 'p-9' });
    expect(items.find((i) => i.key.includes('降思考'))?.params).toEqual({ floor: 'medium' });
  });

  it('未选模板 → 回退默认后缀文案', () => {
    const items = buildActiveSummary({ ...base, structured_prompt: true });
    expect(items.some((i) => i.key.includes('默认后缀'))).toBe(true);
  });

  it('除总开关关闭外，每条都带 anchor（否则点了不跳）', () => {
    const items = buildActiveSummary({
      ...base,
      system_normalize: true,
      dead_zone_rewrite: true,
      rewrite_rules: [{ from: 'a', to: 'b' }],
      synonym_enabled: true,
      synonym_pairs: [{ from: 'c', to: 'd' }],
      reduce_reasoning: true,
      cross_source_fallback: true,
      fallback_models: ['m'],
      skill_inject: true,
      structured_prompt: true,
      auto_preset: true,
    });
    expect(items.length).toBeGreaterThan(5);
    for (const item of items) {
      expect(item.anchor, `缺 anchor：${item.key}`).toBeTruthy();
    }
  });

  it('总开关关闭那条不带 anchor（无处可跳）', () => {
    const items = buildActiveSummary({ ...base, enabled: false });
    expect(items[0].anchor).toBeUndefined();
  });
});

describe('validateImportedSettings', () => {
  const base = {
    rewrite_rules: [{ from: 'a', to: 'b' }],
    synonym_pairs: [],
    retry_limit: 8,
  };

  it('结构正常 → 无 fatal 无 warning', () => {
    const v = validateImportedSettings(base);
    expect(v.fatal).toEqual([]);
    expect(v.warnings).toEqual([]);
  });

  it('根节点不是对象 → fatal', () => {
    expect(validateImportedSettings(null).fatal.length).toBe(1);
    expect(validateImportedSettings('x').fatal.length).toBe(1);
    expect(validateImportedSettings([]).fatal.length).toBe(1);
  });

  it('数组字段不是数组 → fatal（渲染期会 .map 抛错白屏）', () => {
    const v = validateImportedSettings({ ...base, synonym_pairs: 'oops' });
    expect(v.fatal.some((m) => m.includes('synonym_pairs'))).toBe(true);
  });

  it('规则行 from 不是字符串 → fatal 且点名第几条', () => {
    const v = validateImportedSettings({
      ...base,
      rewrite_rules: [{ from: 1, to: 'b' }],
    });
    expect(v.fatal.some((m) => m.includes('第 1 条'))).toBe(true);
  });

  it('数字越界/非数字 → warning（不阻断导入）', () => {
    const v = validateImportedSettings({ ...base, retry_limit: 99, skill_inject_limit: 'x' });
    expect(v.fatal).toEqual([]);
    expect(v.warnings.length).toBe(2);
    expect(v.warnings.join()).toContain('99');
  });

  it('模板缺名 / 布尔非布尔 → warning', () => {
    const v = validateImportedSettings({
      ...base,
      structured_templates: [{ name: '', text: 't' }],
      enabled: 'yes',
    });
    expect(v.fatal).toEqual([]);
    expect(v.warnings.some((m) => m.includes('没有名称'))).toBe(true);
    expect(v.warnings.some((m) => m.includes('enabled'))).toBe(true);
  });

  it('缺省字段不算问题（旧导出文件必须能导回）', () => {
    const v = validateImportedSettings({ rewrite_rules: [] });
    expect(v.fatal).toEqual([]);
    expect(v.warnings).toEqual([]);
  });
});

describe('skillKeywordSample', () => {
  it('关键词原样嵌入（子串匹配，多余包装不影响命中）', () => {
    const sample = skillKeywordSample('troubleshooting');
    expect(sample).toContain('troubleshooting');
    expect(sample.trim()).not.toBe('troubleshooting');
  });

  it('首尾空白剔除；空关键词给空串（不产生伪装样例）', () => {
    expect(skillKeywordSample('  net  ')).toContain(' net '.trim());
    expect(skillKeywordSample('   ')).toBe('');
  });
});

describe('persistPreviewHistory', () => {
  const store = new Map<string, string>();
  const set = (k: string, v: string): void => {
    store.set(k, v);
  };
  const remove = (k: string): void => {
    store.delete(k);
  };

  it('非空 → 写 JSON；为空 → 移除 key（不留空数组）', () => {
    persistPreviewHistory(['a', 'b'], set, remove);
    expect(JSON.parse(store.get('aigx.purge.previewHistory') ?? '[]')).toEqual(['a', 'b']);

    persistPreviewHistory([], set, remove);
    expect(store.has('aigx.purge.previewHistory')).toBe(false);
  });
});

describe('capError', () => {
  it('未超限 / 空 / undefined → 无提示', () => {
    expect(capError([], 3)).toBeUndefined();
    expect(capError(undefined, 3)).toBeUndefined();
    expect(capError(['a', 'b'], 3)).toBeUndefined();
    expect(capError(['a', 'b', 'c'], 3)).toBeUndefined();
  });

  it('超限 → 提示含实际条数与上限', () => {
    const msg = capError(['a', 'b', 'c', 'd'], 3);
    expect(msg).toContain('4');
    expect(msg).toContain('3');
    expect(msg).toContain('只保留前 3 项');
  });

  it('可带字段名前缀', () => {
    expect(capError(['a', 'b'], 1, '前缀')).toContain('前缀：');
  });
});

describe('presetApplyNeedsConfirm', () => {
  const existing = [
    { name: 'A', text: 'text-a' },
    { name: 'B', text: 'text-b' },
  ];

  it('无同名 → 不需要确认（纯新增）', () => {
    expect(presetApplyNeedsConfirm(existing, 'C', 'whatever')).toBe(false);
  });

  it('同名同正文 → 不需要确认（幂等重复点击）', () => {
    expect(presetApplyNeedsConfirm(existing, 'A', 'text-a')).toBe(false);
  });

  it('同名不同正文 → 需要确认（会覆盖用户手改）', () => {
    expect(presetApplyNeedsConfirm(existing, 'A', 'edited')).toBe(true);
  });

  it('空/未定义模板表 → 不需要确认', () => {
    expect(presetApplyNeedsConfirm(undefined, 'A', 'x')).toBe(false);
    expect(presetApplyNeedsConfirm([], 'A', 'x')).toBe(false);
  });
});

describe('parseTemplateJson', () => {
  it('单对象 / 数组 / 配置级 {structured_templates} 三形态', () => {
    expect(parseTemplateJson('{"name":"a","text":"T"}')).toEqual({
      templates: [{ name: 'a', text: 'T' }], skipped: 0,
    });
    expect(parseTemplateJson('[{"name":"a","text":"T"},{"name":"b","text":""}]')).toEqual({
      templates: [{ name: 'a', text: 'T' }, { name: 'b', text: '' }], skipped: 0,
    });
    expect(parseTemplateJson('{"structured_templates":[{"name":"c","text":"X"}]}'))
      .toEqual({ templates: [{ name: 'c', text: 'X' }], skipped: 0 });
  });

  it('坏 JSON → not-json；无有效条目 → no-valid', () => {
    expect(parseTemplateJson('not json')).toEqual({ error: 'not-json' });
    expect(parseTemplateJson('[]')).toEqual({ error: 'no-valid' });
    expect(parseTemplateJson('{"name":123,"text":"T"}')).toEqual({ error: 'no-valid' });
    expect(parseTemplateJson('{"name":"  ","text":"T"}')).toEqual({ error: 'no-valid' });
  });

  it('非法条目跳过计数：非对象/空名/超长名/重名（大小写不敏感）', () => {
    const raw = JSON.stringify([
      { name: 'ok', text: 'T' },
      42,
      { text: 'no-name' },
      { name: 'x'.repeat(70), text: 'T' },
      { name: 'OK', text: 'dup' },
    ]);
    expect(parseTemplateJson(raw)).toEqual({
      templates: [{ name: 'ok', text: 'T' }], skipped: 4,
    });
  });

  it('name 前后空白剥离', () => {
    expect(parseTemplateJson('{"name":"  a  ","text":"T"}')).toEqual({
      templates: [{ name: 'a', text: 'T' }], skipped: 0,
    });
  });
});

describe('preview history', () => {
  const store = new Map<string, string>();
  const get = (k: string): string | null => store.get(k) ?? null;
  const set = (k: string, v: string): void => { store.set(k, v); };

  it('readPreviewHistory：空/坏 JSON/非字符串数组 → 空表', () => {
    expect(readPreviewHistory(get)).toEqual([]);
    store.set('aigx.purge.previewHistory', '{bad json');
    expect(readPreviewHistory(get)).toEqual([]);
    store.set('aigx.purge.previewHistory', '[1, null, "ok"]');
    expect(readPreviewHistory(get)).toEqual(['ok']);
    store.delete('aigx.purge.previewHistory');
  });

  it('write：新样例置顶、同值去重、截断 8 条、落 localStorage', () => {
    let items: string[] = [];
    for (let i = 1; i <= 10; i += 1) {
      items = writePreviewHistory(items, `sample-${i}`, set);
    }
    expect(items).toHaveLength(8);
    expect(items[0]).toBe('sample-10');
    expect(items.includes('sample-1')).toBe(false);
    // 同值回填：不重复，且提到最前
    items = writePreviewHistory(items, 'sample-3', set);
    expect(items[0]).toBe('sample-3');
    expect(items.filter((x) => x === 'sample-3')).toHaveLength(1);
    // 持久化形状：写进 localStorage
    expect(JSON.parse(store.get('aigx.purge.previewHistory') ?? '')[0]).toBe('sample-3');
    store.clear();
  });

  it('空白样例不入库', () => {
    const items = writePreviewHistory(['a'], '   ', set);
    expect(items).toEqual(['a']);
    store.clear();
  });

  it('label：首行截 46 字符；角色前缀形态加 ~ 前缀', () => {
    expect(previewHistoryLabel('短样例')).toBe('短样例');
    expect(previewHistoryLabel(`${'长'.repeat(60)}`)).toContain('…');
    expect(previewHistoryLabel(`${'长'.repeat(60)}`).length).toBeLessThanOrEqual(48);
    expect(previewHistoryLabel('user: 黑客在攻击')).toBe('~ user: 黑客在攻击');
  });
});

describe('diffLines', () => {
  it('无变化 → 全 ctx 行', () => {
    expect(diffLines('a\nb\nc', 'a\nb\nc')).toEqual([
      { kind: 'ctx', text: 'a' },
      { kind: 'ctx', text: 'b' },
      { kind: 'ctx', text: 'c' },
    ]);
  });

  it('行内改词 → 折叠为 mod 行（成对 del/add）', () => {
    const rows = diffLines('黑客在攻击', '安全研究人员在攻击');
    expect(rows).toEqual([
      { kind: 'mod', del: '黑客在攻击', add: '安全研究人员在攻击' },
    ]);
  });

  it('纯增行 / 纯删行', () => {
    expect(diffLines('a', 'a\nb')).toEqual([
      { kind: 'ctx', text: 'a' },
      { kind: 'add', text: 'b' },
    ]);
    expect(diffLines('a\nb', 'a')).toEqual([
      { kind: 'ctx', text: 'a' },
      { kind: 'del', text: 'b' },
    ]);
  });

  it('中间插入不误报上下文重排（LCS 对齐）', () => {
    const rows = diffLines('第一行\n第三行', '第一行\n第二行\n第三行');
    expect(rows).toEqual([
      { kind: 'ctx', text: '第一行' },
      { kind: 'add', text: '第二行' },
      { kind: 'ctx', text: '第三行' },
    ]);
  });

  it('超大输入 → 整红整绿退化（不追精确 diff）', () => {
    const big = Array.from({ length: 500 }, (_, i) => `line-${i}`).join('\n');
    const big2 = Array.from({ length: 500 }, (_, i) => `line2-${i}`).join('\n');
    const rows = diffLines(big, big2);
    expect(rows).toHaveLength(1000);
    expect(rows.filter((r) => r.kind === 'del')).toHaveLength(500);
    expect(rows.filter((r) => r.kind === 'add')).toHaveLength(500);
  });
});

describe('diffTokensInLine', () => {
  it('相同行 → keep1 全文', () => {
    expect(diffTokensInLine('abc', 'abc')).toEqual({
      keep1: 'abc', del: '', keep2: '', add: '', keep3: '',
    });
  });

  it('词级：替换段染色、前后缀保留', () => {
    const w = diffTokensInLine('黑客在攻击系统', '安全研究人员在攻击系统');
    expect(w.keep1).toBe('');
    expect(w.keep3).toBe('在攻击系统');
    // 中段：黑客被删、安全研究人员被增（贪心序列保持）
    expect(w.del).toContain('黑客');
    expect(w.add).toContain('安全研究人员');
  });

  it('纯追加（前缀保留，add 尾部）', () => {
    const w = diffTokensInLine('报告正文', '报告正文\n\n<skill name="x">');
    expect(w.keep1).toBe('报告正文');
    expect(w.add.length).toBeGreaterThan(0);
    expect(w.del).toBe('');
  });

  it('纯删除', () => {
    const w = diffTokensInLine('AB CD EF', 'AB EF');
    // 前缀按词推进：'AB' + 空格都保留在前缀里
    expect(w.keep1.startsWith('AB')).toBe(true);
    expect(w.keep3).toBe('EF');
    expect(w.del).toContain('CD');
    expect(w.add).toBe('');
  });
});

describe('parsePreviewMessages', () => {
  it('裸文本 → 单条 user（现状不变）', () => {
    expect(parsePreviewMessages('你好\n_world')).toEqual([
      { role: 'user', content: '你好\n_world' },
    ]);
  });

  it('角色前缀行拆多条；无前缀行并入当前消息', () => {
    const src = 'system: 你是助手\n注意安全\nuser: 黑客在攻击';
    expect(parsePreviewMessages(src)).toEqual([
      { role: 'system', content: '你是助手\n注意安全' },
      { role: 'user', content: '黑客在攻击' },
    ]);
  });

  it('前缀大小写不限；assistant 也支持', () => {
    const src = 'SYSTEM: a\nUser: b\nASSISTANT: c';
    expect(parsePreviewMessages(src)).toEqual([
      { role: 'system', content: 'a' },
      { role: 'user', content: 'b' },
      { role: 'assistant', content: 'c' },
    ]);
  });

  it('冒号后无空白不算前缀（普通文本）', () => {
    expect(parsePreviewMessages('system:无空格')).toEqual([
      { role: 'user', content: 'system:无空格' },
    ]);
  });

  it('空前缀行产生的空消息被丢弃；全空白输入 → 空数组', () => {
    expect(parsePreviewMessages('user: \n  \n')).toEqual([]);
    expect(parsePreviewMessages('   \n\n')).toEqual([]);
  });

  it('CRLF 与首尾空白行 OK', () => {
    expect(parsePreviewMessages('user: a\r\nuser: b\r\n')).toEqual([
      { role: 'user', content: 'a' },
      { role: 'user', content: 'b' },
    ]);
  });
});

describe('parseRulesCsv', () => {
  it('解析两列基础形态（含表头）', () => {
    expect(parseRulesCsv('from,to\n黑客,安全研究人员\n爆破,口令测试\n')).toEqual([
      { from: '黑客', to: '安全研究人员' },
      { from: '爆破', to: '口令测试' },
    ]);
  });

  it('无表头直接数据行也接受', () => {
    expect(parseRulesCsv('a,b')).toEqual([{ from: 'a', to: 'b' }]);
  });

  it('中文表头（原文/改写为、敏感词/同义词）被丢弃', () => {
    expect(parseRulesCsv('原文,改写为\n黑,白\n')).toEqual([{ from: '黑', to: '白' }]);
    expect(parseRulesCsv('敏感词,同义词\nx,y\n')).toEqual([{ from: 'x', to: 'y' }]);
  });

  it('首行只命中一个表头词 → 当数据行保留', () => {
    expect(parseRulesCsv('from,to,extra\na,b,c\n')).toBeNull();
  });

  it('RFC 4180：引号内逗号/换行/双写引号', () => {
    const csv = 'from,to\n"含,逗号","含""引号"\n"跨\n行",b\n';
    expect(parseRulesCsv(csv)).toEqual([
      { from: '含,逗号', to: '含"引号' },
      { from: '跨\n行', to: 'b' },
    ]);
  });

  it('CRLF 与 BOM 都能处理', () => {
    expect(parseRulesCsv('﻿from,to\r\na,b\r\n')).toEqual([{ from: 'a', to: 'b' }]);
  });

  it('空行跳过、from 空白整单拒绝、列数错整单拒绝', () => {
    expect(parseRulesCsv('from,to\n\na,b\n\n\n')).toEqual([{ from: 'a', to: 'b' }]);
    expect(parseRulesCsv('from,to\n ,b\n')).toBeNull();
    expect(parseRulesCsv('a,b,c\n')).toBeNull();
    expect(parseRulesCsv('only-one\n')).toBeNull();
  });

  it('空内容返回空数组（调用方按「无规则行」提示）', () => {
    expect(parseRulesCsv('')).toEqual([]);
    expect(parseRulesCsv('from,to\n')).toEqual([]);
  });

  it('单元格trim：前后空白剔除', () => {
    expect(parseRulesCsv(' a , b \n')).toEqual([{ from: 'a', to: 'b' }]);
  });
});

