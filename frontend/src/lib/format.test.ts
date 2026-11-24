import { describe, expect, it, afterEach } from 'vitest';
import { fmtCost, toCsv } from './format';
import { setQuotaPerYuan } from './quotaRate';

describe('fmtCost（分 → 元，统一两位小数）', () => {
  it('按 200 额度 = 1 元折算 + 千分位两位', () => {
    expect(fmtCost(246900)).toBe('¥1,234.50');
    expect(fmtCost(0)).toBe('¥0.00');
    expect(fmtCost(200)).toBe('¥1.00');
    expect(fmtCost(1)).toBe('¥0.01');
    expect(fmtCost(246913578)).toBe('¥1,234,567.89');
  });

  it('负数额号在币号外（-¥1.50）', () => {
    expect(fmtCost(-300)).toBe('-¥1.50');
    expect(fmtCost(-246900)).toBe('-¥1,234.50');
  });

  it('null / undefined → —', () => {
    expect(fmtCost(null)).toBe('—');
    expect(fmtCost(undefined)).toBe('—');
  });

  it('英文档同样千分位两位小数', () => {
    expect(fmtCost(246900, 'en')).toBe('¥1,234.50');
  });

  it('随兑换倍率动态折算', () => {
    // 后台把倍率改成 1 元 = 100 额度后，同样的额度数应折成两倍金额
    setQuotaPerYuan(100);
    expect(fmtCost(123450)).toBe('¥1,234.50');
    expect(fmtCost(200)).toBe('¥2.00');
    setQuotaPerYuan(200);
  });
});

describe('toCsv 转义（RFC 4180）', () => {
  it('裸 CR 进引号分支（否则一行会被拆成两行）', () => {
    const csv = toCsv([{ a: 'x\ry', b: '2' }]);
    // 单元格必须被引号包住；不加引号时 CSV 里就是 "x" + CR + "y,2"
    expect(csv).toBe('a,b\n"x\ry",2');
  });

  it('Windows CRLF 同样进引号分支', () => {
    const csv = toCsv([{ a: '第一行\r\n第二行', b: '2' }]);
    expect(csv).toBe('a,b\n"第一行\r\n第二行",2');
  });

  it('正例对照：普通值与逗号/引号/换行的既有口径不变', () => {
    expect(toCsv([{ a: 'plain', b: 1 }])).toBe('a,b\nplain,1');
    expect(toCsv([{ a: 'has,comma' }])).toBe('a\n"has,comma"');
    expect(toCsv([{ a: 'say "hi"' }])).toBe('a\n"say ""hi"""');
    expect(toCsv([{ a: 'line\nbreak' }])).toBe('a\n"line\nbreak"');
    expect(toCsv([{ a: null, b: undefined }])).toBe('a,b\n,');
  });
});

afterEach(() => {
  // 全局倍率是模块级状态，用例之间复位，避免顺序依赖
  setQuotaPerYuan(200);
});
