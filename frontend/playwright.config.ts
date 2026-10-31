import { defineConfig, devices } from '@playwright/test';

// E2E 跑在**入库的构建产物** static/ 上，用内置静态服务器 + API 桩，
// 无需真实后端/数据库，可离线且可并发。
//
// 本地：npm run test:e2e
// CI：见 .github/workflows/ci.yml 的 e2e job（需先 npm run build）
export default defineConfig({
  testDir: './e2e',
  // 同一 static 目录 + 单端口，串行更稳定；用例本身很轻
  workers: 1,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : [['list']],
  use: {
    baseURL: `http://127.0.0.1:${process.env.E2E_PORT || 4173}`,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    viewport: { width: 1440, height: 900 },
    locale: 'zh-CN',
  },
  projects: [
    // 允许用环境变量指向系统 Chrome（本地未跑 npx playwright install 时省事）；
    // CI 中不设置该变量，使用 playwright 自带浏览器以保证一致性。
    {
      name: 'desktop',
      use: {
        ...devices['Desktop Chrome'],
        ...(process.env.E2E_CHROME_PATH
          ? { launchOptions: { executablePath: process.env.E2E_CHROME_PATH } }
          : {}),
      },
    },
    // 移动端单独一个 project：抽屉/汉堡按钮的层级问题只在窄屏暴露
    {
      name: 'mobile',
      use: {
        ...devices['Pixel 5'],
        viewport: { width: 390, height: 844 },
        ...(process.env.E2E_CHROME_PATH
          ? { launchOptions: { executablePath: process.env.E2E_CHROME_PATH } }
          : {}),
      },
    },
  ],
  webServer: {
    command: 'node e2e/static-server.mjs',
    url: `http://127.0.0.1:${process.env.E2E_PORT || 4173}/index.html`,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
