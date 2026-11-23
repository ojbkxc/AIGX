import { describe, expect, it } from 'vitest';
import {
  diffLines,
  diffTokensInLine,
  parsePreviewMessages,
  parseRulesCsv,
  previewHistoryLabel,
  readPreviewHistory,
  writePreviewHistory,
} from './PurgeConsole';

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
