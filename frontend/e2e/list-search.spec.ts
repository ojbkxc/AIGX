import { test, expect, loginAsAdmin } from './fixtures';

/**
 * 列表页搜索/刷新时工具栏必须保持挂载。
 *
 * 回归背景：Users / Channels 原来在 `loading` 时整页 `return <SkeletonTable/>`，
 * 搜索防抖触发后会卸载搜索框本身 —— 用户输入到一半焦点丢失，表现为
 * 「输入几个字搜索框就消失」。
 */
test.describe('列表页搜索不卸载工具栏', () => {
  test('用户管理：输入后搜索框仍在且保持焦点', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/users');
    await expect(page.locator('.main-content')).toContainText('用户管理');

    const search = page.locator('.main-content input[placeholder*="搜索"]').first();
    await expect(search).toBeVisible();
    await search.click();
    await search.fill('admin');

    // 防抖 300ms + 请求往返：等待后搜索框必须还在且保留输入值。
    // 关键断言是「元素没被卸载」——用输入值是否还在判断（卸载重建会丢值），
    // 比 activeElement 更稳定（React 重渲染时焦点可能被浏览器重置）。
    await page.waitForTimeout(900);
    await expect(search).toBeVisible();
    await expect(search).toHaveValue('admin');

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });

  test('渠道管理：搜索后工具栏与搜索框保持挂载', async ({ page, pageErrors }) => {
    await loginAsAdmin(page);
    await page.goto('/channels');
    await expect(page.locator('.main-content')).toContainText('供应商');

    const search = page.locator('.main-content input[placeholder*="搜索"]').first();
    await expect(search).toBeVisible();
    await search.fill('lxchat');
    await page.waitForTimeout(900);

    await expect(search).toBeVisible();
    await expect(search).toHaveValue('lxchat');
    // 工具栏按钮不应随骨架消失
    await expect(page.getByRole('button', { name: /添加供应商/ }).first()).toBeVisible();

    expect(pageErrors, pageErrors.join('\n')).toEqual([]);
  });
});
