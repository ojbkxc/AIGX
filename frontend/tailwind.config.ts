import type { Config } from 'tailwindcss';

export default {
  // 与 lib/theme.ts 的实际机制对齐：主题通过 <html data-theme="dark"> 切换，
  // 而不是 .dark 类。'selector' 是 Tailwind 3.4+ 的推荐写法（等价旧 'class'，
  // 但允许自定义选择器）。
  darkMode: ['selector', '[data-theme="dark"]'],
  content: ['./index.html', './src/**/*.{js,jsx,ts,tsx}'],
  theme: {
    extend: {
      fontFamily: {
        sans: ['-apple-system', 'BlinkMacSystemFont', 'Inter', 'Vazirmatn', 'ui-sans-serif', 'system-ui', 'Segoe UI', 'Roboto', 'Ubuntu', 'Cantarell', 'Noto Sans', 'sans-serif'],
        mono: ['JetBrainsMono', 'SF Mono', 'Consolas', 'monospace'],
      },
      colors: {
        // 桥接 App.css 设计系统的 CSS 变量
        glass: {
          bg: 'var(--bg-color)',
          card: 'var(--card-bg)',
          border: 'var(--border-color)',
          text: 'var(--text-main)',
          muted: 'var(--text-muted)',
          accent: 'var(--accent-color)',
          'input-bg': 'var(--input-bg)',
          'input-border': 'var(--input-border)',
        },
      },
      borderRadius: {
        card: '12px',
        control: '8px',
      },
      boxShadow: {
        card: 'var(--card-shadow)',
        elevated: 'var(--shadow)',
      },
      transitionTimingFunction: {
        smooth: 'cubic-bezier(0.16, 1, 0.3, 1)',
      },
    },
  },
  plugins: [],
} satisfies Config;
