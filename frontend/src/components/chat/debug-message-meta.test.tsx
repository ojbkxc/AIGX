/**
 * 按消息下标存的元数据重映射（debug-message-meta）—— 复现删除后的错位。
 *
 * 原缺陷：删除第 1 条（0-based idx=1）后只 `delete(1)`，第 2 条的下标变成 1，
 * 但它记录的耗时仍挂在 2 上 → 界面上「响应耗时」整体错位一格，最后一条
 * 干脆没有标记。这些断言能区分新旧：旧实现下第 2 条读不到任何值。
 */
import { describe, expect, it } from 'vitest';
import { clearMeta, removeMetaAt, truncateMeta } from './debug-message-meta';

describe('removeMetaAt', () => {
  it('删除中间一条后，其后条目整体左移一位（旧实现只 delete，键不变）', () => {
    const meta = new Map<number, number>([[0, 100], [1, 200], [2, 300]]);
    const after = removeMetaAt(meta, 1);
    // 消息 [m0][m1][m2] → [m0][m2]：m2 的新下标是 1，耗时 300 必须跟过去
    expect(after.get(0)).toBe(100);
    expect(after.get(1)).toBe(300);
    expect(after.has(2)).toBe(false);
  });

  it('删除最后一条：只丢它自己，前面的不动', () => {
    const after = removeMetaAt(new Map([[0, 1], [1, 2]]), 1);
    expect([...after.entries()]).toEqual([[0, 1]]);
  });

  it('删除不存在的下标：原样返回（值不变）', () => {
    const after = removeMetaAt(new Map([[0, 1]]), 5);
    expect([...after.entries()]).toEqual([[0, 1]]);
  });

  it('不修改入参（React 复用旧引用会不重渲染）', () => {
    const meta = new Map<number, number>([[0, 1], [1, 2], [2, 3]]);
    removeMetaAt(meta, 0);
    expect([...meta.entries()]).toEqual([[0, 1], [1, 2], [2, 3]]);
  });

  it('连续删除两条：剩余键连续无洞（值跟随原消息一起左移）', () => {
    let meta = new Map<number, number>([[0, 10], [1, 11], [2, 12], [3, 13]]);
    meta = removeMetaAt(meta, 1);   // 剩 m0=10, m2=12, m3=13 → 键 0,1,2
    meta = removeMetaAt(meta, 1);   // 删掉 m2 → 剩 m0=10, m3=13 → 键 0,1
    expect([...meta.entries()]).toEqual([[0, 10], [1, 13]]);
  });
});

describe('truncateMeta', () => {
  it('截尾丢弃 >= len 的条目（编辑重发后新回复不继承旧耗时）', () => {
    const after = truncateMeta(new Map([[0, 1], [1, 2], [2, 3]]), 2);
    expect([...after.entries()]).toEqual([[0, 1], [1, 2]]);
  });

  it('len 为 0 时全清', () => {
    expect(truncateMeta(new Map([[0, 1], [1, 2]]), 0).size).toBe(0);
  });

  it('len 大于现有条目数时原样保留', () => {
    const after = truncateMeta(new Map([[0, 1]]), 9);
    expect([...after.entries()]).toEqual([[0, 1]]);
  });
});

describe('clearMeta', () => {
  it('返回空 Map（清空对话后首条消息不继承上一轮耗时）', () => {
    expect(clearMeta().size).toBe(0);
  });
});
