import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import { writeFileSync, mkdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// tailwindcss @tailwindcss/vite 插件将在依赖安装后的 Phase 2 启用：
// import tailwindcss from '@tailwindcss/vite';
// plugins: [react(), tailwindcss()]

// build 结束后往产物目录写 version.json：后端 update::static_version()
// 靠它判「磁盘上的前端与二进制是否同版」（versionMismatch）。没有这个文件，
// 「手动换过二进制 / 半更新」在生产上永远检测不出来。
// 输出必须逐字节确定（无时间戳），否则 CI 的 static 同步闸门永远过不了。
// 当前构建版本：`AIGX_VERSION` 优先，否则读仓库根 `VERSION`。
// 两处都拿不到时返回 null —— 宁可没有版本号，也不要写一个假的。
function readVersion(): string | null {
  const fromEnv = process.env.AIGX_VERSION?.trim();
  if (fromEnv) return fromEnv;
  try {
    const raw = readFileSync(resolve(__dirname, '../VERSION'), 'utf8').trim();
    return raw || null;
  } catch {
    return null;
  }
}

function emitStaticVersion(): import('vite').Plugin {
  return {
    name: 'emit-static-version',
    apply: 'build',
    closeBundle() {
      const outDir = resolve(__dirname, '../static');
      mkdirSync(outDir, { recursive: true });
      const version = readVersion();
      // 内容必须确定性：不写时间戳（否则 CI 的 static 同步闸门永远红）
      const manifest = { version };
      writeFileSync(
        resolve(outDir, 'version.json'),
        JSON.stringify(manifest, null, 2) + '\n',
        'utf8',
      );
    },
  };
}

export default defineConfig({
  plugins: [react(), emitStaticVersion()],
  // 把版本号烧进 bundle：`useVersionGuard` 用它和部署产物里的 version.json 比对，
  // 检测「浏览器还跑着旧 JS、后端已升级」。
  define: {
    __APP_VERSION__: JSON.stringify(readVersion() ?? ''),
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
    globals: true,
    // 固定 NODE_ENV=test：宿主 shell 若已导出 NODE_ENV=production，
    // React 会加载生产构建，@testing-library 的 act() 直接抛错。
    env: { NODE_ENV: 'test' },
  },
  // 模块解析优先级：TS > JS（页面迁移期间保证 .tsx 优先于同名 .jsx 被加载）
  resolve: {
    extensions: ['.mjs', '.mts', '.ts', '.tsx', '.js', '.jsx', '.json'],
    // Vite 不读 tsconfig paths，需在此显式映射 @ -> src
    alias: {
      '@': resolve(__dirname, 'src'),
    },
  },
  server: {
    port: 3000,
    proxy: {
      '/api': 'http://127.0.0.1:8080',
      '/v1': 'http://127.0.0.1:8080',
    }
  },
  build: {
    outDir: '../static',
    emptyOutDir: true,
    // 浏览器基线：ES2020 为目标（可选链/空值合并等原生可用，无需降级即可去掉多余 polyfill）
    target: 'es2020',
    // 显式声明压缩器与关闭 sourcemap：生产产物不带 .map，避免泄露源码、减小体积
    minify: 'esbuild',
    sourcemap: false,
    // 代码分割：vendor 与业务代码分离，利于缓存（产物体积变化由 CI 验证）
    rollupOptions: {
      output: {
        // 用函数式分组把同一包的**所有子模块**归并到一个 vendor chunk。
        // 对象式只能匹配 `lucide-react` 入口，图标子路径（dist/esm/icons/*）
        // 仍会被 Rollup 拆成几十个 1-2KB 的小 chunk；函数式按路径前缀可整组收拢。
        // 只对我们关心的包分组，其它模块返回 undefined 交回 Rollup 默认策略。
        manualChunks(id: string): string | undefined {
          if (!id.includes('node_modules')) return undefined;
          // lucide 图标：逐图标子模块按路径前缀整组归入 vendor-icons
          if (id.includes('/lucide-react/')) return 'vendor-icons';
          // @tanstack/react-query 及其 @tanstack/query-core 同组
          if (id.includes('/@tanstack/')) return 'vendor-query';
          // i18n 运行时连同其转译期依赖一起分组，避免它们落回主 chunk 反向依赖形成循环
          if (
            id.includes('/i18next/') ||
            id.includes('/react-i18next/') ||
            id.includes('/@babel/runtime/') ||
            id.includes('/html-parse-stringify/')
          ) {
            return 'vendor-i18n';
          }
          // react 系连同 scheduler、@remix-run（react-router 的依赖）一起分组；
          // 这些依赖若落回主 chunk，会让主 chunk ⇄ vendor-react 形成循环依赖告警。
          if (
            id.includes('/node_modules/react/') ||
            id.includes('/node_modules/react-dom/') ||
            id.includes('/node_modules/react-router/') ||
            id.includes('/node_modules/react-router-dom/') ||
            id.includes('/node_modules/scheduler/') ||
            id.includes('/node_modules/@remix-run/')
          ) {
            return 'vendor-react';
          }
          return undefined;
        },
      }
    }
  }
});
