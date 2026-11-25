import { describe, expect, it } from 'vitest';
import { kindOfRisk, needsApproval, normalizeRisk, riskLabelKey, toolSpecIndex } from './tool-meta';

describe('tool-meta — 风险归一到后端值域', () => {
  it('大小写与连字符容错', () => {
    expect(normalizeRisk('readonly')).toBe('readonly');
    expect(normalizeRisk('ReadOnly')).toBe('readonly');
    expect(normalizeRisk('read_only')).toBe('readonly');
    expect(normalizeRisk('low')).toBe('low');
    expect(normalizeRisk('HIGH')).toBe('high');
    expect(normalizeRisk('dangerous')).toBe('high');
  });

  it('未知 risk 不当只读（缺省不能等于放行）', () => {
    expect(normalizeRisk(undefined)).toBe('unknown');
    expect(normalizeRisk('weird')).toBe('unknown');
    expect(kindOfRisk('unknown')).toBe('unknown');
  });

  it('risk → A2A kind：readonly 只读，low/high 改动；run 无数据源', () => {
    expect(kindOfRisk('readonly')).toBe('read');
    expect(kindOfRisk('low')).toBe('write');
    expect(kindOfRisk('high')).toBe('write');
  });

  it('审批判据只用于展示，但高/低危都算需要', () => {
    expect(needsApproval('high')).toBe(true);
    expect(needsApproval('low')).toBe(true);
    expect(needsApproval('readonly')).toBe(false);
    expect(needsApproval('unknown')).toBe(false);
  });

  it('徽标文案键落在既有键上', () => {
    expect(riskLabelKey('readonly')).toBe('只读');
    expect(riskLabelKey('low')).toBe('低危');
    expect(riskLabelKey('high')).toBe('高危');
  });
});

describe('tool-meta — 规格索引', () => {
  const specs = [
    { name: 'list_accounts', label: '列出账号', risk: 'readonly' },
    { name: 'create_key', label: '创建密钥', risk: 'high' },
  ];

  it('按 name 查到后端下发的中文名与风险', () => {
    const index = toolSpecIndex(specs);
    expect(index.label('create_key')).toBe('创建密钥');
    expect(index.risk('create_key')).toBe('high');
    expect(index.kind('list_accounts')).toBe('read');
  });

  it('未登记的工具回退工具 id，不编造中文名', () => {
    const index = toolSpecIndex(specs);
    expect(index.label('nope')).toBe('nope');
    expect(index.risk('nope')).toBe('unknown');
    expect(index.label(undefined)).toBe('');
  });

  it('配置缺失时是空索引（标签退化回 id）', () => {
    const index = toolSpecIndex(undefined);
    expect(index.label('any_tool')).toBe('any_tool');
    expect(index.risk('any_tool')).toBe('unknown');
    expect(index.description('any_tool')).toBe('');
  });
});
