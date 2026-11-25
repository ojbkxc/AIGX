/**
 * 调试面报文详情（debug-traffic-view）—— 复现乱序覆盖与大报文渲染。
 *
 * 原缺陷 1：`openDetail` 没有「响应是否还是当前选中项」的判据，快速点 A→B
 * 时 A 的迟到响应会把 B 的弹层内容覆盖成 A 的报文（id 与正文不匹配）。
 * 原缺陷 2：整段 body 直接塞进 `<pre>`，无预算无折叠。
 */
import { describe, expect, it } from 'vitest';
import { boundedText, DETAIL_TEXT_BUDGET, isStaleResponse } from './debug-traffic-view';

describe('boundedText', () => {
  it('预算内逐字不变（正常大小的报文零视觉变化）', () => {
    const s = '{"a":1,"b":"中文"}';
    expect(boundedText(s, 100)).toEqual({ text: s, truncated: false, total: s.length });
  });

  it('超预算时只给前段，并如实报总长', () => {
    const s = 'x'.repeat(50);
    const r = boundedText(s, 10);
    expect(r.text).toBe('x'.repeat(10));
    expect(r.truncated).toBe(true);
    expect(r.total).toBe(50);
  });

  it('null/undefined 视为空串（不炸，也不渲染 "null"）', () => {
    expect(boundedText(null, 10)).toEqual({ text: '', truncated: false, total: 0 });
    expect(boundedText(undefined, 10).text).toBe('');
  });

  it('不把代理对切一半（emoji 截半会渲染成乱码方块）', () => {
    // 😀 = 两个 UTF-16 code unit（0xD83D 0xDE00）
    const s = `a😀b`;
    const r = boundedText(s, 2); // 切点正好落在代理对中间
    expect(r.text).toBe('a');
    expect(r.truncated).toBe(true);
  });

  it('budget <= 0 视为不截断（Infinity 展开路径）', () => {
    const s = 'y'.repeat(30);
    expect(boundedText(s, Infinity).truncated).toBe(false);
    expect(boundedText(s, Infinity).text).toBe(s);
  });

  it('默认预算是 20000 字符', () => {
    expect(DETAIL_TEXT_BUDGET).toBe(20_000);
    const big = 'z'.repeat(DETAIL_TEXT_BUDGET + 1);
    expect(boundedText(big).truncated).toBe(true);
    expect(boundedText('z'.repeat(DETAIL_TEXT_BUDGET)).truncated).toBe(false);
  });
});

describe('isStaleResponse', () => {
  it('序号一致 = 当前响应，可以采用', () => {
    expect(isStaleResponse(3, 3)).toBe(false);
  });

  it('序号更大（用户又点了别的条目）= 旧响应必须丢弃', () => {
    // A 发起时 mine=3，用户点了 B（current=4），A 的响应迟到
    expect(isStaleResponse(4, 3)).toBe(true);
  });

  it('关闭弹层后迟到（current 已被 closeDetail +1）= 丢弃', () => {
    expect(isStaleResponse(5, 4)).toBe(true);
  });
});
