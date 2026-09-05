/** @type {import('tailwindcss').Config} */
export default {
  darkMode: ['selector', '[data-theme="dark"]'],
  content: ['./index.html', './src/**/*.{js,jsx,ts,tsx}'],
  theme: {
    extend: {
      fontFamily: {
        sans: ['Inter', '-apple-system', 'BlinkMacSystemFont', 'Segoe UI', 'Roboto', 'sans-serif'],
        display: ['Outfit', 'Inter', 'sans-serif'],
        mono: ['JetBrains Mono', 'Consolas', 'Courier New', 'monospace'],
      },
      colors: {
        // 桥接 App.css 玻璃拟态设计系统的 CSS 变量
        // 让 Tailwind 类与现有主题变量共存，渐进迁移不破坏现有页面
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
        card: '14px',
        control: '8px',
      },
      backdropBlur: {
        glass: '20px',
      },
      transitionTimingFunction: {
        smooth: 'cubic-bezier(0.16, 1, 0.3, 1)',
      },
    },
  },
  plugins: [],
};
