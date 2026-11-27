import { describe, it, expect } from 'vitest';
import { CONFIG_INDEX, matchConfigEntries } from './config-index';

/**
 * 配置项索引 —— 可发现性的判据层。
 *
 * 这个文件的存在理由：用户报「易支付 / github / linuxdo 这些配置我在线上没看见」。
 * 修法不是加菜单（侧栏正在精简），而是让**已有**的搜索与总览能按用户嘴里的词命中。
 * 所以这里钉的不是「组件渲染出来了」，而是「输入用户实际会输入的词，能不能命中到
 * 那条配置，且落地路径真的是承载它的那个 Tab」。
 *
 * 断言必须能区分新旧：`matchConfigEntries` 换成恒返回全部 / 恒返回空，下列用例都会红。
 */

/** t 的替身：本项目 key === 中文原文，英文界面下才返回不同值。 */
const zhT = (key: string): string => key;
const enT = (key: string): string => {
  const en: Record<string, string> = {
    易支付配置: 'Epay configuration',
    'OAuth 登录': 'OAuth login',
    用户分组: 'User groups',
    超时: 'Timeout',
  };
  return en[key] ?? key;
};

describe('config-index — 用户嘴里的词必须能命中', () => {
  it('「易支付」命中易支付配置，且落地在承载它的 billing Tab', () => {
    const hits = matchConfigEntries('易支付', zhT, true);
    const epay = hits.find((e) => e.id === 'epay');
    expect(epay, '搜「易支付」必须命中 epay 条目').toBeTruthy();
    // 不是 /settings —— 设置页里没有易支付（这正是用户「没看见」的由来）
    expect(epay!.path).toBe('/products?tab=billing');
  });

  it('用户报的三家 OAuth 都能命中同一条，且指向 OAuth 配置入口', () => {
    for (const word of ['github', 'GitHub', '谷歌', 'google', 'linuxdo', 'LinuxDO', 'oauth', '第三方登录']) {
      const hits = matchConfigEntries(word, zhT, true);
      const oauth = hits.find((e) => e.id === 'oauth');
      expect(oauth, `搜「${word}」必须命中 OAuth 配置`).toBeTruthy();
      expect(oauth!.path).toBe('/oauth');
    }
  });

  it('英文界面下敲中文原文仍能命中（key===中文原文，但用户可能仍敲中文）', () => {
    const hits = matchConfigEntries('易支付', enT, true);
    expect(hits.some((e) => e.id === 'epay')).toBe(true);
    // 反向：英文界面敲英文展示名也能命中
    const en = matchConfigEntries('Epay configuration', enT, true);
    expect(en.some((e) => e.id === 'epay')).toBe(true);
  });

  it('按落地位置搜也能命中（用户记得「在套餐里」这类线索）', () => {
    const hits = matchConfigEntries('计费设置', zhT, true);
    const ids = hits.map((e) => e.id);
    expect(ids).toContain('epay');
    expect(ids).toContain('user-groups');
  });

  it('空查询返回全部（总览要一屏列全），且不含重复 id', () => {
    const all = matchConfigEntries('', zhT, true);
    expect(all.length).toBe(CONFIG_INDEX.length);
    const ids = all.map((e) => e.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('查无此项时返回空而不是全部（否则「搜不到」会伪装成「都有」）', () => {
    expect(matchConfigEntries('zzz-不存在-zzz', zhT, true)).toEqual([]);
  });

  it('非管理员看不到 adminOnly 条目', () => {
    const hits = matchConfigEntries('易支付', zhT, false);
    expect(hits.some((e) => e.id === 'epay')).toBe(false);
  });
});

describe('config-index — 索引本身的结构约束', () => {
  it('每条都有落地路径与位置说明（总览的「在哪一列」不能空）', () => {
    for (const entry of CONFIG_INDEX) {
      expect(entry.path, `${entry.id} 缺 path`).toMatch(/^\//);
      expect(entry.hint.trim(), `${entry.id} 缺 hint`).not.toBe('');
      expect(entry.aliases.length, `${entry.id} 缺别名`).toBeGreaterThan(0);
    }
  });

  it('id 唯一（React key 与测试锚点的前提）', () => {
    const ids = CONFIG_INDEX.map((e) => e.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('指向存在的 Tab 参数：path 里的 tab 值必须和该页认识的取值一致', () => {
    // 硬编码的取值来源：
    //   /products 的 ProductTab = plans | redemptions | billing（Products.tsx）
    //   /settings 的 SettingsTab 见 Settings.tsx 的 SETTINGS_CATEGORIES
    // 这里只钉我们实际用到的几个，防止改名后路径静默变成「落到默认 Tab」。
    const productsTabs = new Set(['plans', 'redemptions', 'billing']);
    const settingsTabs = new Set([
      'account', 'notifications', 'general', 'display', 'gateway', 'providers',
      'timeout', 'security', 'oauth', 'data', 'feedback', 'system', 'update',
    ]);
    for (const entry of CONFIG_INDEX) {
      const url = new URL(entry.path, 'http://x');
      const tab = url.searchParams.get('tab');
      if (tab == null) continue;
      if (url.pathname === '/products') {
        expect(productsTabs.has(tab), `${entry.id}: /products?tab=${tab} 不是已知 Tab`).toBe(true);
      } else if (url.pathname === '/settings') {
        expect(settingsTabs.has(tab), `${entry.id}: /settings?tab=${tab} 不是已知分类`).toBe(true);
      }
    }
  });
});