/**
 * `lib/purge-ai-bridge` 的单测：破甲 → AI 运维交接、AI → 破甲回跳判据。
 *
 * 每条断言都对准一个**已发生过的失败形态**（见各 describe 的说明），
 * 不做「函数能跑就算过」的自指断言。
 */
import { describe, expect, it, vi } from 'vitest';
import {
  PURGE_AI_HANDOFF_KEY,
  PURGE_AI_HANDOFF_TTL_MS,
  PURGE_AI_PROMPT_MAX_CHARS,
  buildPurgeAiPrompt,
  consumePurgeAiHandoff,
  readPurgeAiHandoff,
  touchedPurge,
  writePurgeAiHandoff,
  type PurgeAiStorage,
} from './purge-ai-bridge';

/** 可注入的内存存储（与 sessionStorage 同面；不碰 jsdom 的真实实现）。 */
function memStorage(initial: Record<string, string> = {}): PurgeAiStorage & {
  dump: () => Record<string, string>;
  failNextSet: () => void;
} {
  const map = new Map<string, string>(Object.entries(initial));
  let failSet = false;
  return {
    get: (k) => map.get(k) ?? null,
    set: (k, v) => {
      if (failSet) throw new Error('QuotaExceededError');
      map.set(k, v);
    },
    remove: (k) => {
      map.delete(k);
    },
    dump: () => Object.fromEntries(map),
    failNextSet: () => {
      failSet = true;
    },
  };
}

describe('writePurgeAiHandoff / readPurgeAiHandoff', () => {
  it('写入后能读回同一条正文，并带上写入时刻', () => {
    const store = memStorage();
    expect(writePurgeAiHandoff('  看下破甲配置  ', store, 1000)).toBe(true);
    const item = readPurgeAiHandoff(store, 1000);
    expect(item?.prompt).toBe('看下破甲配置');
    expect(item?.at).toBe(1000);
    expect(item?.v).toBe(1);
  });

  it('空正文不写（避免把空串塞进输入框）', () => {
    const store = memStorage();
    expect(writePurgeAiHandoff('   \n ', store)).toBe(false);
    expect(store.dump()[PURGE_AI_HANDOFF_KEY]).toBeUndefined();
  });

  it('超长正文截断到上限（「一句话」不该是六页）', () => {
    const store = memStorage();
    const long = 'x'.repeat(PURGE_AI_PROMPT_MAX_CHARS + 500);
    writePurgeAiHandoff(long, store);
    expect(readPurgeAiHandoff(store, Date.now())?.prompt.length).toBe(PURGE_AI_PROMPT_MAX_CHARS);
  });

  it('存储抛错（隐私模式/配额满）时返回 false，不把异常抛给点击处理', () => {
    const store = memStorage();
    store.failNextSet();
    expect(() => writePurgeAiHandoff('x', store)).not.toThrow();
    expect(writePurgeAiHandoff('x', store)).toBe(false);
  });

  it('过期的交接不读、并顺手清掉（浏览器会话恢复会带回上周的值）', () => {
    const store = memStorage();
    writePurgeAiHandoff('旧摘要', store, 1_000);
    expect(readPurgeAiHandoff(store, 1_000 + PURGE_AI_HANDOFF_TTL_MS + 1)).toBeNull();
    expect(store.dump()[PURGE_AI_HANDOFF_KEY]).toBeUndefined();
  });

  it('结构不符/坏 JSON 一律清键返回 null（旧版本遗留值不能当正文填进去）', () => {
    expect(readPurgeAiHandoff(memStorage({ [PURGE_AI_HANDOFF_KEY]: '{not json' }))).toBeNull();
    const wrongVersion = memStorage({
      [PURGE_AI_HANDOFF_KEY]: JSON.stringify({ v: 2, prompt: 'x', at: Date.now() }),
    });
    expect(readPurgeAiHandoff(wrongVersion)).toBeNull();
    expect(wrongVersion.dump()[PURGE_AI_HANDOFF_KEY]).toBeUndefined();
    const emptyPrompt = memStorage({
      [PURGE_AI_HANDOFF_KEY]: JSON.stringify({ v: 1, prompt: '  ', at: Date.now() }),
    });
    expect(readPurgeAiHandoff(emptyPrompt)).toBeNull();
  });

  it('get 本身抛错（禁用存储）时静默返回 null', () => {
    const store: PurgeAiStorage = {
      get: () => {
        throw new Error('SecurityError');
      },
      set: vi.fn(),
      remove: vi.fn(),
    };
    expect(readPurgeAiHandoff(store)).toBeNull();
  });
});

describe('consumePurgeAiHandoff', () => {
  it('取一次就清键：离开 /agent 再回来不会被重复填同一个摘要', () => {
    const store = memStorage();
    writePurgeAiHandoff('摘要 A', store, 500);
    expect(consumePurgeAiHandoff(store, 500)).toBe('摘要 A');
    expect(consumePurgeAiHandoff(store, 500)).toBeNull();
    expect(store.dump()[PURGE_AI_HANDOFF_KEY]).toBeUndefined();
  });

  it('过期时也不返回正文（同 read 的 TTL 判据）', () => {
    const store = memStorage();
    writePurgeAiHandoff('摘要 A', store, 500);
    expect(consumePurgeAiHandoff(store, 500 + PURGE_AI_HANDOFF_TTL_MS + 1)).toBeNull();
  });
});

describe('buildPurgeAiPrompt', () => {
  const base = {
    activeItems: ['system 归一化', '死区词改写（12 条启用）'],
    warnings: [{ message: '异源递进已开启，但异源模型列表为空', severity: 'error' as const }],
  };

  it('带上生效项与体检结论（AI 不必为「现在开了什么」多跑一轮只读）', () => {
    const text = buildPurgeAiPrompt(base);
    expect(text).toContain('system 归一化');
    expect(text).toContain('死区词改写（12 条启用）');
    expect(text).toContain('[error] 异源递进已开启，但异源模型列表为空');
  });

  it('写死「整份替换」与工具名：缺这两句模型会只传一个字段把词表清零', () => {
    const text = buildPurgeAiPrompt(base);
    expect(text).toContain('整份替换');
    expect(text).toContain('aigx_config_view{key:"purge"}');
    expect(text).toContain('aigx_config_set{key:"purge"}');
    // 第一跳必须是体检工具（否则模型要自己从二十多个字段推「哪些开了却不生效」）
    expect(text).toContain('aigx_purge_check');
  });

  it('无生效项/无告警时给出明确结论，而不是空白段', () => {
    const text = buildPurgeAiPrompt({ activeItems: [], warnings: [] });
    expect(text).toContain('（无生效项）');
    expect(text).toContain('通过：没有「开了但不生效」或自相矛盾的项目');
  });

  it('预设命中率段：只报计数，不把 196 行模型名单抄进输入框', () => {
    const text = buildPurgeAiPrompt({
      ...base,
      presetHit: { models_enabled: 196, exact: 62, family: 92, universal: 42, none: 0, would_apply: 196 },
    });
    expect(text).toContain('启用模型 196 个');
    expect(text).toContain('通用兜底 42');
    expect(text).toContain('真正会前插正文 196 个');
  });

  it('自定义 ask 覆盖默认那句（同一函数服务两种意图）', () => {
    const text = buildPurgeAiPrompt({ ...base, ask: '只告诉我怎么关掉通用兜底' });
    expect(text).toContain('只告诉我怎么关掉通用兜底');
    expect(text).not.toContain('请先体检（aigx_purge_check）再动手');
  });

  it('超上限时截断并指向只读工具（正文不静默丢）', () => {
    const many = Array.from({ length: 200 }, (_, i) => `- 生效项 ${i} ${'x'.repeat(40)}`);
    const text = buildPurgeAiPrompt({ activeItems: many, warnings: [] });
    expect(text.length).toBeLessThanOrEqual(PURGE_AI_PROMPT_MAX_CHARS + 60);
    expect(text).toContain('已截断');
  });
});

describe('touchedPurge', () => {
  it('诊断工具名直接命中', () => {
    expect(touchedPurge([{ name: 'aigx_purge_check' }])).toBe(true);
    expect(touchedPurge([{ name: 'aigx_purge_preset_probe', args: '{"model":"glm-5"}' }])).toBe(true);
  });

  it('aigx_config_set 只有 key 真的是 purge 才算（防止正文提到这个词误判）', () => {
    expect(touchedPurge([{ name: 'aigx_config_set', args: '{"key":"purge","value":{}}' }])).toBe(true);
    expect(
      touchedPurge([
        { name: 'aigx_config_set', args: '{"key":"gatewayPrompt","value":{"prompt":"别动 purge 配置"}}' },
      ]),
    ).toBe(false);
  });

  it('坏 JSON / 非对象参数不猜（当作没碰过）', () => {
    expect(touchedPurge([{ name: 'aigx_config_set', args: '{oops' }])).toBe(false);
    expect(touchedPurge([{ name: 'aigx_config_set', args: 'null' }])).toBe(false);
    expect(touchedPurge([{ name: 'aigx_config_set' }])).toBe(false);
  });

  it('只读别的配置 / 无关工具不算碰过破甲', () => {
    expect(touchedPurge([{ name: 'aigx_config_view', args: '{"key":"purge"}' }])).toBe(false);
    expect(touchedPurge([{ name: 'aigx_models_view' }])).toBe(false);
    expect(touchedPurge([])).toBe(false);
  });
});