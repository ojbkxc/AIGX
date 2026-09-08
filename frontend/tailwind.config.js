/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
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
};
