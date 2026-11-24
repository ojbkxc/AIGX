import { describe, expect, it } from 'vitest';
import {
  activeSummaryText,
  buildActiveSummary,
  clampNumInput,
  capError,
  exportPreviewHistory,
  findBodySnippet,
  parseBatchRules,
  parsePreviewHistoryJson,
  noteRecentPreset,
  readDraftBackup,
  readPresetFilter,
  readRecentPresets,
  retryRateText,
  snapshotReportText,
  statsExportNote,
  sortWarningsBySeverity,
  tidyRules,
  warningReportText,
  settingsEqual,
  tidySummary,
  writeDraftBackup,
  diffLines,
  diffSettings,
  diffTokensInLine,
  parsePreviewMessages,
  parseRulesCsv,
  parseTemplateJson,
  persistPreviewHistory,
  presetApplyNeedsConfirm,
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

describe('activeSummaryText', () => {
  it('每行一条、带 `- ` 前缀，参数已插值（贴出去可直接读）', () => {
    const items = [
      { key: '死区词改写（{{n}} 条启用）', params: { n: 3 } },
      { key: 'system 归一化' },
    ];
    const text = activeSummaryText(items, (key, params) =>
      params ? key.replace('{{n}}', String(params.n)) : key,
    );
    expect(text).toBe('- 死区词改写（3 条启用）\n- system 归一化');
  });

  it('空清单 → 空串（不产出只有装饰的文本）', () => {
    expect(activeSummaryText([], (k) => k)).toBe('');
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
