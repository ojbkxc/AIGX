import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 添加账号四段 + 重复提供商检测（#22 前端 / #23 后端支撑）。
 *
 * ┌─────────────────────────────────────────────────────────────────────┐
 * │ mock 形状契约锚点（防「前端假定形状」假绿——逐字段出处）：           │
 * │  账号列表 GET /api/account-sessions                                │
 * │    → {success, data: [...], total}（static-server.mjs:143-152      │
 * │      ACCOUNT_SESSIONS / 后端 src/api/admin/account_sessions.rs）   │
 * │  供应商 GET /api/account-sessions/providers                        │
 * │    → {success, data: [{kind,label,category?,auth?,default_base_url?│
 * │      }], total}（static-server.mjs:709-711 ACCOUNT_PROVIDERS）     │
 * │  渠道 GET /api/channels → {success, data: [{id,name,…}]}           │
 * │    （static-server.mjs:339-341）                                   │
 * │  桌面来源 GET /api/account-sessions/desktop/sources                │
 * │    → {success, data: [{id,label,path,encrypted,supported,          │
 * │      unsupportedReason}]}（desktop_import.rs:165-176 投影；白名单   │
 * │      DESKTOP_SOURCES :180-210：cline 明文 supported、其余 encrypted │
 * │      501；非 Windows 全 unsupported）                              │
 * │  桌面导入 POST /api/account-sessions/desktop-import                │
 * │    → body 只有 {source_id, name?}（desktop_import.rs:216-221）     │
 * │  创建 POST /api/account-sessions（payload 形状 = 对话框 submit）    │
 * └─────────────────────────────────────────────────────────────────────┘
 *
 * 全部新端点 page.route 层 mock，零 static-server.mjs 改动。
 * 注意：desktop POST 必须 mock——本地 Windows 上真端点会读真实用户文件。
 */

/** 账号列表桩（含一条同地址账号，供重复检测命中） */
const SESSIONS = {
  success: true,
  data: [
    {
      id: 'acc-dup-1', channel_id: 'ch-dup', channel_name: '我的中转 账号池',
      name: 'dup-1', provider: 'openai_compatible', base_url: 'https://dup.example.com/v1',
      status: 'enabled', configured_status: 'enabled',
      priority: 0, weight: 1, max_concurrency: 0, models: [], extra_headers: {},
    },
  ],
  total: 1,
};

const PROVIDERS = {
  success: true,
  data: [
    { kind: 'chatglm', label: '智谱清言', category: 'web', auth: 'cookie', default_base_url: 'https://chatglm.cn' },
    { kind: 'autoclaw-intl', label: 'AutoClaw 国际版', category: 'agent', auth: {}, default_base_url: 'https://autoglm-api.autoglm.ai' },
  ],
  total: 2,
};

const CHANNELS = {
  success: true,
  // channel_type 必须叫这个名：页面按 c.channel_type === 'account' 过滤
  // （AccountSessions.tsx:559），写成 type 会被滤空 → findDuplicate 拿不到渠道名
  data: [{ id: 'ch-dup', name: '我的中转 账号池', channel_type: 'account', status: 'enabled' }],
};

/** 桌面来源桩（形状 = desktop_import.rs:165-176；cline 明文 supported） */
const DESKTOP_SOURCES = {
  success: true,
  data: [
    { id: 'autoclaw', label: 'AutoClaw 客户端登录态', path: '%APPDATA%/AutoClaw/auth.json', encrypted: true, supported: false, unsupportedReason: '该来源的 DPAPI/safeStorage 解密链是上游插件私有知识，暂不支持' },
    { id: 'box-agent', label: 'BoxAgent 客户端登录态', path: '~/.box-agent/config/auth.json', encrypted: true, supported: false, unsupportedReason: '该来源的 DPAPI/safeStorage 解密链是上游插件私有知识，暂不支持' },
    { id: 'meituan-catpaw', label: '美团猫爪客户端登录态', path: '~/.meituan-catpaw/auth.json', encrypted: true, supported: false, unsupportedReason: '该来源的 DPAPI/safeStorage 解密链是上游插件私有知识，暂不支持' },
    { id: 'cline', label: 'Cline 客户端登录态', path: '~/.cline/config/auth.json', encrypted: false, supported: true, unsupportedReason: null },
  ],
};

test.describe('添加账号 · file/desktop/guest/oauth 四段 + 重复检测', () => {
  test.beforeEach(async ({ page }) => {
    await page.route('**/api/account-sessions/providers', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(PROVIDERS) }));
    await page.route('**/api/channels**', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(CHANNELS) }));
    await loginAsAdmin(page);
  });

  test('desktop 段：来源清单渲染（supported/灰态）+ 导入请求只有 source_id/name', async ({ page, pageErrors }) => {
    await page.route(/\/api\/account-sessions\/desktop-import(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { id: 'acc-new' } }) }));
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS) }));
    await page.route('**/api/account-sessions/desktop/sources', (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(DESKTOP_SOURCES) }));

    await page.goto('/channels?tab=accounts');
    await page.getByRole('button', { name: '添加账号' }).first().click();
    // 桌面卡在「已建 API」组（新建自定义提供商旁）
    await page.getByRole('button', { name: /桌面客户端登录态/ }).first().click();
    await expect(page.locator('h4', { hasText: '桌面客户端登录态导入' })).toBeVisible();

    // 4 来源：cline 可导入，encrypted 三家灰态带原因
    const rows = page.locator('.add-desktop-row');
    await expect(rows).toHaveCount(4);
    const clineRow = rows.filter({ hasText: 'Cline' });
    await expect(clineRow.getByRole('button', { name: '导入' })).toBeEnabled();
    const encRow = rows.filter({ hasText: 'AutoClaw 客户端登录态' });
    await expect(encRow.getByRole('button', { name: '不支持' })).toBeDisabled();
    await expect(encRow).toContainText('上游插件私有知识');

    // 导入请求体：只有 source_id + name（无路径参数——安全红线）
    const imported: Array<Record<string, unknown>> = [];
    await page.route('**/api/account-sessions/desktop-import', async (r) => {
      imported.push(r.request().postDataJSON() as Record<string, unknown>);
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { id: 'acc-new' } }) });
    });
    await page.getByLabel('备注名（可选，作用于本次导入的所有账号）').fill('e2e-桌面');
    await clineRow.getByRole('button', { name: '导入' }).click();
    await expect.poll(() => imported.length).toBe(1);
    expect(imported[0]).toEqual({ source_id: 'cline', name: 'e2e-桌面' });

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('chatglm：guest/oauth/file 三方法齐备；file 读文件填入粘贴域并提交', async ({ page, pageErrors }) => {
    const created: Array<Record<string, unknown>> = [];
    // guest/oauth 块的提交走 submitPaste → POST login/paste（非 create 端点）
    await page.route(/\/api\/account-sessions\/login\/paste(\?|$)/, (r) => {
      created.push(r.request().postDataJSON() as Record<string, unknown>);
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { accountId: 'acc-new' } }) });
    });
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS) }));

    await page.goto('/channels?tab=accounts');
    await page.getByRole('button', { name: '添加账号' }).first().click();
    // 搜索命中 chatglm 卡（Web 档——档位对齐桩 static-server.mjs:135）
    await page.getByPlaceholder('搜索提供商…').fill('智谱');
    await page.locator('.add-card', { hasText: '智谱清言' }).first().click();

    // 三方法齐备（file 由对话框对 paste 家统一追加；oauth 只属 autoclaw-intl）
    const tabs = page.locator('.add-method-row .ui-tabs button, .add-method-row button');
    const tabText = await tabs.allInnerTexts();
    expect(tabText.join('|')).toContain('访客模式');
    expect(tabText.join('|')).toContain('凭证文件');
    expect(tabText.join('|')).not.toContain('网页授权');

    // guest 块：说明 + 粘贴域 + 提交（submitPaste → POST login/paste，
    // body = {provider, channel_id, secret, name}——AddAccountDialog.tsx:567-599、
    // api/index.ts:826、后端 account_login.rs:173）
    await page.locator('.add-method-row button', { hasText: '访客模式' }).click();
    await expect(page.locator('h4', { hasText: '访客模式' })).toBeVisible();
    await expect(page.locator('.add-block', { hasText: '访客模式' })).toContainText('没有积分体系');
    await page.getByLabel('accessToken').fill('eyJ-guest-token');
    await page.getByRole('button', { name: '校验并添加' }).click();
    await expect.poll(() => created.length).toBe(1);
    expect(created[0].secret).toBe('eyJ-guest-token');
    expect(created[0].provider).toBe('chatglm');

    // file 块：选择 JSON 文件 → 内容进粘贴域 → 提交 body 带文件内容。
    // 上一次 guest 提交成功会关闭弹窗（submitPaste 成功路径 onClose）→ 重开
    await page.getByRole('button', { name: '添加账号' }).first().click();
    await page.getByPlaceholder('搜索提供商…').fill('智谱');
    await page.locator('.add-card', { hasText: '智谱清言' }).first().click();
    await page.locator('.add-method-row button', { hasText: '凭证文件' }).click();
    await expect(page.locator('h4', { hasText: '导入凭证文件' })).toBeVisible();
    const fileContent = JSON.stringify({ chatglm_token: 'eyJ-from-file' });
    // 「选择文件」是 label 包裹的隐藏 input（非 button role）；两个 file input
    // （JSON 凭据 / Cookies SQLite #41）用 data-testid 区分，防止命中 2 个
    await page.locator('input[data-testid="file-input-json"]').setInputFiles({
      name: 'cred.json', mimeType: 'application/json', buffer: Buffer.from(fileContent),
    });
    await expect(page.getByLabel('accessToken')).toHaveValue(fileContent);
    await page.getByRole('button', { name: '校验并添加' }).click();
    await expect.poll(() => created.length).toBe(2);
    expect(created[1].secret).toBe(fileContent);

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('oauth 段：autoclaw-intl 指引 + 授权页按钮 + 粘贴提交', async ({ page, pageErrors }) => {
    const created: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/account-sessions\/login\/paste(\?|$)/, (r) => {
      created.push(r.request().postDataJSON() as Record<string, unknown>);
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { accountId: 'acc-new' } }) });
    });
    await page.route(/\/api\/account-sessions(\?|$)/, (r) =>
      r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS) }));

    await page.goto('/channels?tab=accounts');
    await page.getByRole('button', { name: '添加账号' }).first().click();
    await page.getByPlaceholder('搜索提供商…').fill('AutoClaw 国际');
    await page.locator('.add-card', { hasText: 'AutoClaw 国际版' }).first().click();

    await page.locator('.add-method-row button', { hasText: '网页授权' }).click();
    await expect(page.locator('h4', { hasText: '网页授权' })).toBeVisible();
    // 指引如实声明：网页弹窗无法过滑块（桌面桥），授权后回填 accessToken
    await expect(page.locator('.add-block', { hasText: '网页授权' })).toContainText('滑块');
    const [popup] = await Promise.all([
      page.waitForEvent('popup').catch(() => null),
      page.getByRole('button', { name: '打开 AutoClaw 国际版登录页' }).click(),
    ]);
    if (popup) await popup.close();
    await page.getByLabel('token（必填）').fill('eyJ-oauth-token');
    await page.getByRole('button', { name: '校验并添加' }).click();
    await expect.poll(() => created.length).toBe(1);
    expect(created[0].secret).toBe('eyJ-oauth-token');
    expect(created[0].provider).toBe('autoclaw-intl');
    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('重复检测：同协议同地址已有账号 → 警告条 → 仍要新建放行', async ({ page, pageErrors }) => {
    const created: Array<Record<string, unknown>> = [];
    await page.route(/\/api\/account-sessions(\?|$)/, (r) => {
      if (r.request().method() === 'POST') {
        created.push(r.request().postDataJSON() as Record<string, unknown>);
        return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ success: true, data: { id: 'acc-new' } }) });
      }
      return r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(SESSIONS) });
    });

    await page.goto('/channels?tab=accounts');
    await page.getByRole('button', { name: '添加账号' }).first().click();
    // 「新建自定义提供商」卡（已建 API 组第一张）
    await page.locator('.add-card', { hasText: '新建自定义提供商' }).first().click();
    await page.getByLabel('渠道名称（可选）').fill('我的中转');
    // 上游地址/凭证在折叠的「账号设置」details 里：先展开再填（closed details
    // 内容对 Playwright 不可见——CI :214 的 not visible 即此）
    await page.locator('.add-advanced > summary').click();
    await page.getByLabel('上游地址（可选，留空用 provider 默认）').fill('https://dup.example.com/v1');
    await page.getByLabel('API Key').fill('sk-test');

    // 首次提交 → 警告条 + 拦截（POST 未发出）
    await page.locator('.add-modal-form').getByRole('button', { name: '添加账号' }).last().click();
    await expect(page.locator('.add-dup-warn')).toContainText('我的中转 账号池');
    await expect(page.locator('.add-dup-warn')).toContainText('不会重复建池');
    expect(created.length).toBe(0);

    // 「仍要新建」→ 放行；POST body provider/协议 + 地址正确（并入同池语义不变）
    await page.locator('.add-dup-warn').getByRole('button', { name: '仍要新建' }).click();
    await page.locator('.add-modal-form').getByRole('button', { name: '添加账号' }).last().click();
    await expect.poll(() => created.length).toBe(1);
    expect(created[0].provider).toBe('openai_compatible');
    expect(created[0].base_url).toBe('https://dup.example.com/v1');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
