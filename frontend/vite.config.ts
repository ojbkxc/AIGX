import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import { writeFileSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

// tailwindcss @tailwindcss/vite 插件将在依赖安装后的 Phase 2 启用：
// import tailwindcss from '@tailwindcss/vite';
// plugins: [react(), tailwindcss()]

// build 结束后往产物目录写 version.json：后端 update::static_version()
// 靠它判「磁盘上的前端与二进制是否同版」（versionMismatch）。没有这个文件，
// 「手动换过二进制 / 半更新」在生产上永远检测不出来。
function emitStaticVersion(): import('vite').Plugin {
  return {
    name: 'emit-static-version',
    apply: 'build',
    closeBundle() {
      const outDir = resolve(__dirname, '../static');
      mkdirSync(outDir, { recursive: true });
      const manifest = {
        // AIGX_VERSION 由 CI 注入；本地构建没有就写 null（后端把 null 当「判不出」）
        version: process.env.AIGX_VERSION ?? null,
        builtAt: process.env.SOURCE_DATE_EPOCH
          ? Number(process.env.SOURCE_DATE_EPOCH) * 1000
          : Date.now(),
      };
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
    // 代码分割：vendor 与业务代码分离，利于缓存（产物体积变化由 CI 验证）
    rollupOptions: {
      output: {
        manualChunks: {
          'vendor-react': ['react', 'react-dom', 'react-router-dom'],
          'vendor-i18n': ['i18next', 'react-i18next'],
        }
      }
    }
  }
});
