// @vitest-environment node
/**
 * 预设库的「命中率」与「虚拟滚动」判据（纯逻辑）。
 *
 * 两条最容易出错的点：
 * 1. **把逐条徽标当成命中率**：后端列表里的 `model_match` 是展示口径，把
 *    「12/53 条命中 exact」说成「命中率 23%」是假话——真正决定会不会前插的是
 *    `preset_for_model`（透出为 `data.auto_preset`）。判据钉住「有写路径结论时
 *    只认它、没有时不编百分比」。
 * 2. **虚拟滚动的边界**：条数少于窗口、滚动超出总高、行高为 0、可视高为 0
 *    这四种退化都必须返回全量而不是空窗口（否则第一屏空白，比不虚拟化更糟）。
 */
import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  PRESET_ROW_HEIGHT,
  coverageLine,
  presetCoverage,
  virtualWindow,
} from './purge-presets';

describe('virtualWindow', () => {
  it('条数不足一屏 → 返回全量、无占位（与不虚拟化逐像素一致）', () => {
    const w = virtualWindow(3, 0, 360);
    expect(w.start).toBe(0);
    expect(w.end).toBe(3);
    expect(w.padTop).toBe(0);
    expect(w.padBottom).toBe(0);
  });

  it('滚到中部 → 窗口前后各留 overscan，占位高度合计 = 总高 − 窗口高', () => {
    const count = 100;
    const w = virtualWindow(count, 31 * PRESET_ROW_HEIGHT, 360, PRESET_ROW_HEIGHT, 4);
    expect(w.start).toBe(27);
    expect(w.end).toBeGreaterThan(27);
    expect(w.padTop).toBe(w.start * PRESET_ROW_HEIGHT);
    expect(w.padBottom).toBe((count - w.end) * PRESET_ROW_HEIGHT);
    // 占位 + 窗口 = 全量，否则滚动条长度会漂
    expect(w.padTop + (w.end - w.start) * PRESET_ROW_HEIGHT + w.padBottom).toBe(
      count * PRESET_ROW_HEIGHT,
    );
  });

  it('滚动超出总高 → 回夹（不出现 start 越过尾行的空窗口）', () => {
    const w = virtualWindow(10, 999999, 360);
    expect(w.start).toBeLessThan(10);
    expect(w.end).toBe(10);
    expect(w.padBottom).toBe(0);
  });

  it('负滚动位置（macOS 橡皮筋）→ 视为 0', () => {
    const w = virtualWindow(20, -50, 360);
    expect(w.start).toBe(0);
    expect(w.padTop).toBe(0);
  });

  it('空列表 / 行高 0 / 可视高 0 → 空窗口（不抛错）', () => {
    expect(virtualWindow(0, 0, 360)).toEqual({ start: 0, end: 0, padTop: 0, padBottom: 0 });
    expect(virtualWindow(10, 0, 360, 0)).toEqual({ start: 0, end: 0, padTop: 0, padBottom: 0 });
    // 可视高 0（列表还没挂）：至少给一行，否则首帧空白
    expect(virtualWindow(10, 0, 0).end).toBeGreaterThanOrEqual(1);
  });

  it('行高常量与 CSS 里的列表行高对齐（改一处不改另一处就会错位）', () => {
    const css = fs.readFileSync(
      path.resolve(__dirname, '..', 'pages', 'PurgeConsole.css'),
      'utf8',
    );
    // CSS 里 `.purge-preset-item` 的 min-height 必须等于 PRESET_ROW_HEIGHT，
    // 否则占位高度是错的：滚动条长度对、但行会跳。
    const block = css.slice(css.indexOf('.purge-preset-item {'));
    const m = /min-height:\s*(\d+)px/.exec(block.slice(0, 400));
    expect(m, '.purge-preset-item 需要写 min-height（虚拟滚动靠它固定行高）').not.toBeNull();
    expect(Number(m![1])).toBe(PRESET_ROW_HEIGHT);
  });
});

describe('presetCoverage + coverageLine', () => {
  const items = [
    { model_match: 'exact' as const },
    { model_match: 'vendor' as const },
    { model_match: 'universal' as const },
    { model_match: 'mismatch' as const },
    { model_match: null },
  ];

  it('有写路径结论 → 只认它（逐条徽标只做统计，不进文案）', () => {
    const c = presetCoverage(items, { match: 'family', preset_id: 'p1', applied: true });
    expect(c.decisive).toBe(true);
    expect(c.applied).toBe(true);
    expect(c.presetId).toBe('p1');
    expect(c.counts.exact).toBe(1);
    expect(c.counts.vendor).toBe(1);
    expect(c.counts.universal).toBe(1);
    expect(c.counts.total).toBe(5);
    const line = coverageLine(c, 'glm-5', true, true);
    expect(line.tone).toBe('on');
    expect(line.params?.id).toBe('p1');
  });

  it('没有写路径结论 → decisive=false，且文案里**不出现百分比/命中率**', () => {
    const c = presetCoverage(items, null);
    expect(c.decisive).toBe(false);
    const line = coverageLine(c, 'glm-5', true, true);
    expect(line.key).not.toMatch(/%|命中率/);
    expect(line.tone).toBe('warn');
    expect(line.params?.n).toBe(2); // exact + vendor
  });

  it('总开关关闭 → 先说总开关（否则用户会去改模型名）', () => {
    const line = coverageLine(presetCoverage(items, { match: 'pinned', preset_id: 'x', applied: false }), 'm', false, true);
    expect(line.key).toContain('总开关关闭');
    expect(line.tone).toBe('off');
  });

  it('自动套用关闭 → 报这一条，不报「没命中」', () => {
    const line = coverageLine(presetCoverage(items, { match: null, preset_id: null, applied: false }), 'm', true, false);
    expect(line.key).toContain('按模型自动套用');
    expect(line.tone).toBe('off');
  });

  it('命中了但 applied=false → 报「正文为空」（不是「没命中」）', () => {
    const line = coverageLine(
      presetCoverage(items, { match: 'exact', preset_id: 'p-empty', applied: false }),
      'm',
      true,
      true,
    );
    expect(line.key).toContain('正文为空');
    expect(line.params?.id).toBe('p-empty');
    expect(line.tone).toBe('warn');
  });

  it('完全没命中 → 报「没有可套的预设」并回显模型名', () => {
    const line = coverageLine(
      presetCoverage(items, { match: null, preset_id: null, applied: false }),
      'unknown-model',
      true,
      true,
    );
    expect(line.key).toContain('没有可套的预设');
    expect(line.params?.model).toBe('unknown-model');
  });

  it('模型名为空 → 用 — 占位（不留空串给句子中间留个洞）', () => {
    const line = coverageLine(
      presetCoverage([], { match: null, preset_id: null, applied: false }),
      '   ',
      true,
      true,
    );
    expect(line.params?.model).toBe('—');
  });
});