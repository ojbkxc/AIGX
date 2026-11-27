// OAuth 配置面板的**判据层**测试（纯函数）。
//
// 为什么测这些函数而不是只测渲染：这里三处判据写错都会造成线上事故，且
// 渲染层看不见——
// 1. secret 回读是掩码，"留空 = 不修改" 必须靠「不发送字段」实现；一旦改成发送空串，
//    后端 `handle_update_oauth_config` 会把空串真的写进配置（settings.rs:991-995），
//    该 provider 的登录立刻失效，且原 secret 不可恢复。
// 2. ready 判据三家不同（github/linuxdo 不看 redirect_uri、google 看），界面把它当同一口径
//    就会显示「已启用」而登录必失败。
// 3. 回调地址长/短两套形态的识别错一位，用户就会拿错的串去平台登记，
//    得到 `redirect_uri is not associated with this application`。
import { describe, expect, it } from 'vitest';
import {
  STATE_PLACEHOLDER,
  authorizeUrlPreview,
  authorizeUrlPreviewWarnings,
  buildOauthUpdate,
  classifyCallback,
  deriveSiteOrigin,
  emptyDraft,
  isMaskedSecret,
  longCallbackUrl,
  readiness,
  readyRequiresRedirectUri,
  secretConfigured,
  shortCallbackUrl,
} from './oauth';
import type { OauthProviderConfig } from '@/types/oauth';

function cfg(patch: Partial<OauthProviderConfig> = {}): OauthProviderConfig {
  return {
    client_id: 'cid',
    client_secret: '...',
    redirect_uri: 'https://api.example.com/api/auth/github/callback',
    ready: true,
    ...patch,
  };
}

describe('buildOauthUpdate —— 省略=不改，空串=清空', () => {
  it('秘钥留空时**不发送** client_secret（这是「不修改」的唯一正确实现）', () => {
    const update = buildOauthUpdate(
      { client_id: 'cid2', client_secret: '', redirect_uri: cfg().redirect_uri },
      cfg(),
    );
    expect(update).not.toBeNull();
    expect(update).toHaveProperty('client_id', 'cid2');
    expect(Object.prototype.hasOwnProperty.call(update, 'client_secret')).toBe(false);
  });

  it('什么都没改 → 返回 null（界面据此禁用保存按钮）', () => {
    expect(buildOauthUpdate(emptyDraft(cfg()), cfg())).toBeNull();
  });

  it('只改 redirect_uri → 载荷里只有 redirect_uri', () => {
    const update = buildOauthUpdate(
      {
        client_id: 'cid',
        client_secret: '',
        redirect_uri: 'https://api.example.com/oauth/github',
      },
      cfg(),
    );
    expect(update).toEqual({ redirect_uri: 'https://api.example.com/oauth/github' });
  });

  it('输入新秘钥 → 发送新值', () => {
    const update = buildOauthUpdate(
      { client_id: 'cid', client_secret: 's3cret', redirect_uri: cfg().redirect_uri },
      cfg(),
    );
    expect(update).toEqual({ client_secret: 's3cret' });
  });

  it('显式清除 → 发送空串（后端据此真的清空）', () => {
    const update = buildOauthUpdate(
      { client_id: 'cid', client_secret: '', redirect_uri: cfg().redirect_uri },
      cfg(),
      { clearSecret: true },
    );
    expect(update).toEqual({ client_secret: '' });
  });

  it('未配置过秘钥时点清除 → 不产生载荷（否则会写一条无意义的空串）', () => {
    const update = buildOauthUpdate(
      { client_id: 'cid', client_secret: '', redirect_uri: cfg().redirect_uri },
      cfg({ client_secret: '' }),
      { clearSecret: true },
    );
    expect(update).toBeNull();
  });

  it('前端输入首尾空格被裁掉后再比对（避免仅空格差异触发保存）', () => {
    const update = buildOauthUpdate(
      { client_id: ' cid ', client_secret: '', redirect_uri: ` ${cfg().redirect_uri} ` },
      cfg(),
    );
    expect(update).toBeNull();
  });
});

describe('掩码判定', () => {
  it('`...` 与含 `***` 都算掩码（与后端 is_masked_secret 同语义）', () => {
    expect(isMaskedSecret('...')).toBe(true);
    expect(isMaskedSecret(' ... ')).toBe(true);
    expect(isMaskedSecret('ab***cd')).toBe(true);
    expect(isMaskedSecret('real-secret')).toBe(false);
  });

  it('服务端回读非空即视为「已配置」（读不到原值，只能这样判断）', () => {
    expect(secretConfigured('...')).toBe(true);
    expect(secretConfigured('')).toBe(false);
    expect(secretConfigured('   ')).toBe(false);
  });
});

describe('readiness —— 三家判据不同，如实反映', () => {
  it('google 的 ready 判据包含 redirect_uri（后端 google.rs:35-39）', () => {
    expect(readyRequiresRedirectUri('google')).toBe(true);
    expect(readyRequiresRedirectUri('github')).toBe(false);
    expect(readyRequiresRedirectUri('linuxdo')).toBe(false);

    const r = readiness(
      'google',
      { client_id: 'cid', client_secret: '', redirect_uri: '' },
      true,
    );
    expect(r.ready).toBe(false);
    expect(r.missing.join('|')).toContain('redirect_uri');
  });

  it('github：id+secret 齐就 ready，但 redirect_uri 为空要给告警而不是判未就绪', () => {
    const r = readiness(
      'github',
      { client_id: 'cid', client_secret: '', redirect_uri: '' },
      true,
    );
    // 后端 ready() 只看 id+secret —— 界面必须与之一致，否则与后端结论打架
    expect(r.ready).toBe(true);
    expect(r.missing).toEqual([]);
    expect(r.warnings.join('|')).toContain('redirect_uri');
  });

  it('缺 client_id / client_secret 逐条点名（redirect_uri 对 linuxdo 只告警，不判未就绪）', () => {
    const r = readiness('linuxdo', { client_id: '', client_secret: '', redirect_uri: '' }, false);
    expect(r.ready).toBe(false);
    expect(r.missing).toEqual(['client_id 为空', 'client_secret 为空']);
    expect(r.warnings.join('|')).toContain('redirect_uri');
  });

  it('只输入了新秘钥（服务端尚未保存）也算已配置 —— 否则用户刚填完仍显示未启用', () => {
    const r = readiness(
      'github',
      { client_id: 'cid', client_secret: 'new', redirect_uri: 'https://a.example/oauth/github' },
      false,
    );
    expect(r.ready).toBe(true);
    expect(r.warnings).toEqual([]);
  });

  it('redirect_uri 不带协议 / 不是两套回调之一 → 告警', () => {
    const r = readiness(
      'github',
      { client_id: 'cid', client_secret: '', redirect_uri: 'api.example.com/whatever' },
      true,
    );
    const joined = r.warnings.join('|');
    expect(joined).toContain('http://');
    expect(joined).toContain('既不是 /api/auth/<provider>/callback 也不是 /oauth/<provider>');
  });
});

describe('classifyCallback —— 两套回调形态识别', () => {
  it('长 / 短 / 其它 / 空', () => {
    expect(classifyCallback('https://api.x.com/api/auth/github/callback', 'github')).toBe('long');
    expect(classifyCallback('https://api.x.com/oauth/github', 'github')).toBe('short');
    expect(classifyCallback('https://api.x.com/auth/callback', 'github')).toBe('other');
    expect(classifyCallback('   ', 'github')).toBe('empty');
  });

  it('容忍结尾斜杠与 query（平台侧常带回参数）', () => {
    expect(classifyCallback('https://api.x.com/oauth/github/', 'github')).toBe('short');
    expect(classifyCallback('https://api.x.com/api/auth/google/callback?x=1', 'google')).toBe(
      'long',
    );
  });

  it('provider 必须对上：拿 github 的回调去配 linuxdo 判为 other', () => {
    expect(classifyCallback('https://api.x.com/oauth/github', 'linuxdo')).toBe('other');
  });
});

describe('回调候选地址拼接', () => {
  it('去掉站点尾斜杠再拼，避免 //', () => {
    expect(longCallbackUrl('https://api.x.com/', 'google')).toBe(
      'https://api.x.com/api/auth/google/callback',
    );
    expect(longCallbackUrl('https://api.x.com///', 'google')).toBe(
      'https://api.x.com/api/auth/google/callback',
    );
    expect(shortCallbackUrl('https://api.x.com/', 'linuxdo')).toBe('https://api.x.com/oauth/linuxdo');
  });
});

describe('deriveSiteOrigin —— 站点根必须从配置里反推，不能用当前源', () => {
  it('长/短两种形态都能反推出根', () => {
    expect(deriveSiteOrigin('https://api.lxseek.com/api/auth/github/callback', 'github')).toBe(
      'https://api.lxseek.com',
    );
    expect(deriveSiteOrigin('https://api.lxseek.com/oauth/github', 'github')).toBe(
      'https://api.lxseek.com',
    );
    // 子路径部署（控制台挂在 /aigx 下）也能反推
    expect(deriveSiteOrigin('https://x.com/aigx/oauth/google', 'google')).toBe(
      'https://x.com/aigx',
    );
  });

  it('容忍结尾斜杠 / query，并保持根不带斜杠', () => {
    expect(deriveSiteOrigin('https://api.x.com/oauth/github/', 'github')).toBe('https://api.x.com');
    expect(deriveSiteOrigin('https://api.x.com/oauth/github?x=1', 'github')).toBe(
      'https://api.x.com',
    );
  });

  it('不是两套回调之一 / 为空 → null（交给下一级来源，不瞎猜）', () => {
    expect(deriveSiteOrigin('', 'github')).toBeNull();
    expect(deriveSiteOrigin('https://api.x.com/auth/callback', 'github')).toBeNull();
    // provider 对不上也算认不出
    expect(deriveSiteOrigin('https://api.x.com/oauth/github', 'linuxdo')).toBeNull();
  });
});

describe('authorizeUrlPreview —— 必须逐字节等于后端会发出的串', () => {
  it('github 与后端 format! 模板一致（auth.rs:1457-1462）', () => {
    const url = authorizeUrlPreview('github', {
      client_id: 'Ov23',
      client_secret: '',
      redirect_uri: 'https://api.x.com/api/auth/github/callback',
    });
    expect(url).toBe(
      `https://github.com/login/oauth/authorize?client_id=Ov23&redirect_uri=https://api.x.com/api/auth/github/callback&scope=user:email&state=${STATE_PLACEHOLDER}`,
    );
  });

  it('linuxdo 与后端一致（auth.rs:1501，response_type 在最前）', () => {
    const url = authorizeUrlPreview('linuxdo', {
      client_id: 'Wx',
      client_secret: '',
      redirect_uri: 'https://api.x.com/oauth/linuxdo',
    });
    expect(url).toBe(
      `https://connect.linux.do/oauth2/authorize?response_type=code&client_id=Wx&redirect_uri=https://api.x.com/oauth/linuxdo&state=${STATE_PLACEHOLDER}`,
    );
  });

  it('google 带默认 scope（google.rs:47-53）', () => {
    const url = authorizeUrlPreview('google', {
      client_id: 'gid',
      client_secret: '',
      redirect_uri: 'https://api.x.com/api/auth/google/callback',
    });
    expect(url).toBe(
      `https://accounts.google.com/o/oauth2/v2/auth?client_id=gid&redirect_uri=https://api.x.com/api/auth/google/callback&response_type=code&scope=openid email profile&state=${STATE_PLACEHOLDER}`,
    );
  });

  it('redirect_uri **不做百分号编码**（后端就是原样插值，预览若「好看」就失去诊断意义）', () => {
    const url = authorizeUrlPreview('github', {
      client_id: 'c',
      client_secret: '',
      redirect_uri: 'https://a.example/cb?x=1&y=2',
    });
    expect(url).toContain('redirect_uri=https://a.example/cb?x=1&y=2&scope=');
  });
});

describe('authorizeUrlPreviewWarnings —— 会拆坏 query 的字符', () => {
  it('& / 空格 / ? / # 各自点名', () => {
    expect(authorizeUrlPreviewWarnings('https://a/b?x=1')).toHaveLength(1); // 只有 ? 一条
    expect(authorizeUrlPreviewWarnings('https://a/b&c')).toHaveLength(1);
    expect(authorizeUrlPreviewWarnings('https://a/b c')).toHaveLength(1);
    expect(authorizeUrlPreviewWarnings('https://a/b#frag')).toHaveLength(1);
  });

  it('干净地址零告警（否则告警会变成噪声）', () => {
    expect(authorizeUrlPreviewWarnings('https://api.x.com/oauth/github')).toEqual([]);
    expect(authorizeUrlPreviewWarnings('')).toEqual([]);
  });
});
