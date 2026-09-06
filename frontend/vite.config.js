import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// tailwindcss @tailwindcss/vite 插件将在依赖安装后的 Phase 2 启用：
// import tailwindcss from '@tailwindcss/vite';
// plugins: [react(), tailwindcss()]

export default defineConfig({
  plugins: [react()],
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
