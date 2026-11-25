import { describe, expect, it } from 'vitest';
import {
  argumentsView,
  callPhase,
  isSensitiveKey,
  parseToolArguments,
  previewText,
  redactArguments,
  resultLooksFailed,
  usageTotal,
  phaseLabelKey,
} from './tool-calls';

describe('tool-calls — 参数解析不崩', () => {
  it('半截 JSON（流式常态）判非法但保留原文', () => {
    const view = argumentsView('{"path":');
    expect(view.invalid).toBe(true);
    expect(view.text).toBe('{"path":');
    expect(view.redacted).toBe(0);
  });

  it('数组与标量不当对象：判非法', () => {
    expect(parseToolArguments('[1,2]').ok).toBe(false);
    expect(parseToolArguments('42').ok).toBe(false);
    expect(parseToolArguments('').ok).toBe(false);
    expect(parseToolArguments('null').ok).toBe(false);
  });

  it('合法对象保留原文（既有 e2e 按紧凑形态匹配，重排会拆散 token）', () => {
    const raw = '{"path":"README.md"}';
    const view = argumentsView(raw);
    expect(view).toEqual({ text: raw, invalid: false, redacted: 0 });
  });
});

describe('tool-calls — 凭证遮蔽', () => {
  it('标识符后缀不算凭证，密钥本体算', () => {
    expect(isSensitiveKey('api_key')).toBe(true);
    expect(isSensitiveKey('access_token')).toBe(true);
    expect(isSensitiveKey('x-api-key')).toBe(true);
    expect(isSensitiveKey('password')).toBe(true);
    expect(isSensitiveKey('api_key_id')).toBe(false);
    expect(isSensitiveKey('token_count')).toBe(false);
    expect(isSensitiveKey('model')).toBe(false);
  });

  it('嵌套结构里的密钥被替换，其余字段原样', () => {
    const masked = redactArguments({ provider: 'x', auth: { api_key: 'sk-1', api_key_id: 'k-9' } });
    expect(masked.redacted).toBe(1);
    expect(masked.value).toEqual({ provider: 'x', auth: { api_key: '***', api_key_id: 'k-9' } });
  });

  it('含凭证的参数走格式化副本（原文不再展示）', () => {
    const view = argumentsView('{"api_key":"sk-abc","model":"gpt"}');
    expect(view.redacted).toBe(1);
    expect(view.text).toContain('***');
    expect(view.text).not.toContain('sk-abc');
    expect(view.text).toContain('"model": "gpt"');
  });
});

describe('tool-calls — 单条状态', () => {
  it('有结果且像失败 → failed；正常结果 → done', () => {
    expect(callPhase({ result: '{"ok":false,"error":"boom"}' }, 'done')).toBe('failed');
    expect(callPhase({ result: '{"ok":true}' }, 'done')).toBe('done');
    expect(callPhase({ result: 'Error: 上游 500' }, 'done')).toBe('failed');
    expect(callPhase({ result: '共 3 个账号' }, 'done')).toBe('done');
  });

  it('无结果时按消息级状态兜底：流式中 running / 中止 cancelled / 出错 failed', () => {
    expect(callPhase({}, 'streaming')).toBe('running');
    expect(callPhase({ result: '' }, 'streaming')).toBe('running');
    expect(callPhase({}, 'interrupted')).toBe('cancelled');
    expect(callPhase({}, 'error')).toBe('failed');
    expect(callPhase({}, 'done')).toBe('done');
  });

  it('失败判定不被无关文本误伤', () => {
    expect(resultLooksFailed('errorCode 已修复')).toBe(false);
    expect(resultLooksFailed('')).toBe(false);
    expect(resultLooksFailed('{"error":null}')).toBe(false);
  });

  it('阶段文案键都在既有 i18n 集合里（不新增 key：json 不在可写范围）', () => {
    expect(phaseLabelKey('running')).toBe('进行中');
    expect(phaseLabelKey('done')).toBe('完成');
    expect(phaseLabelKey('failed')).toBe('失败');
    expect(phaseLabelKey('cancelled')).toBe('停止');
    // AIGX 的「等确认」只有人工审批一种，复用既有键
    expect(phaseLabelKey('pending')).toBe('人工审批');
  });
});

describe('tool-calls — 长文本与用量', () => {
  it('超限截断并给出被折掉的字符数', () => {
    const text = 'x'.repeat(10);
    expect(previewText(text, 4)).toEqual({ shown: 'xxxx', truncated: true, hidden: 6 });
    expect(previewText(text, 10)).toEqual({ shown: text, truncated: false, hidden: 0 });
    expect(previewText(text, 0).shown).toBe('');
  });

  it('总 token 缺 total 时由 prompt + completion 合成', () => {
    expect(usageTotal({ prompt_tokens: 3, completion_tokens: 4 })).toBe(7);
    expect(usageTotal({ total_tokens: 9, prompt_tokens: 1 })).toBe(9);
    expect(usageTotal({})).toBeUndefined();
    expect(usageTotal(undefined)).toBeUndefined();
  });
});
