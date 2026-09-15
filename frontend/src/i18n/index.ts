import i18n, { type InitOptions } from 'i18next';
import { initReactI18next } from 'react-i18next';
import zh from './zh.json';
import en from './en.json';

export type AppLanguage = 'zh' | 'en';

// 语言检测：优先 localStorage 的 'i18n_lang'，其次浏览器 navigator.language，默认 zh
function detectLanguage(): AppLanguage {
  const saved = localStorage.getItem('i18n_lang');
  if (saved === 'zh' || saved === 'en') return saved;
  const nav = (navigator.language || '').toLowerCase();
  if (nav.startsWith('en')) return 'en';
  return 'zh';
}

const initOptions: InitOptions = {
  resources: {
    zh: { translation: zh },
    en: { translation: en },
  },
  lng: detectLanguage(),
  fallbackLng: 'zh',
  // 允许 key 中包含点号、括号等特殊字符，按原样查找
  keySeparator: false,
  nsSeparator: false,
  interpolation: {
    escapeValue: false, // React 已防 XSS
  },
  returnNull: false,
  returnEmptyString: true,
};

void i18n.use(initReactI18next).init(initOptions);

// ── 按 IP 自动选择语言 ──
// 用户未手动选过语言（localStorage 无记录）时，请求后端 geo-lang 端点：
// - 中国大陆 IP → zh，其他 IP → en
// - 端点返回 null（无代理头/IPv6）或请求失败 → 保持 navigator.language 推断结果
// - 不写 localStorage：只有用户手动切换（setLanguage）才持久化
if (!localStorage.getItem('i18n_lang')) {
  fetch('/api/public/geo-lang')
    .then((res) => (res.ok ? res.json() : null))
    .then((body) => {
      const lang = body?.data?.lang ?? body?.lang;
      if (lang === 'zh' || lang === 'en') {
        void i18n.changeLanguage(lang);
      }
    })
    .catch(() => {
      /* 网络失败静默回退，保持现有语言 */
    });
}

export default i18n;

/**
 * 切换界面语言并持久化到 localStorage。
 * 供 Sidebar / 设置页语言切换器调用。
 */
export function setLanguage(lang: AppLanguage): AppLanguage {
  const next: AppLanguage = lang === 'en' ? 'en' : 'zh';
  localStorage.setItem('i18n_lang', next);
  void i18n.changeLanguage(next);
  return next;
}

/**
 * 当前界面语言（'zh' | 'en'）。
 */
export function getLanguage(): AppLanguage {
  return i18n.language && i18n.language.startsWith('en') ? 'en' : 'zh';
}
