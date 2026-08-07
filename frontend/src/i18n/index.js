import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import zh from './zh.json';
import en from './en.json';

// 语言检测：优先 localStorage 的 'i18n_lang'，其次浏览器 navigator.language，默认 zh
function detectLanguage() {
  const saved = localStorage.getItem('i18n_lang');
  if (saved === 'zh' || saved === 'en') return saved;
  const nav = (navigator.language || '').toLowerCase();
  if (nav.startsWith('en')) return 'en';
  return 'zh';
}

i18n
  .use(initReactI18next)
  .init({
    resources: {
      zh: { translation: zh },
      en: { translation: en },
    },
    lng: detectLanguage(),
    fallbackLng: 'zh',
    // 允许 key 中包含点号、括号等特殊字符，按原样查找
    keySeparator: false,
    nsSeparator: false,
    pluralSeparator: false,
    interpolation: {
      escapeValue: false, // React 已防 XSS
    },
    returnNull: false,
    returnEmptyString: true,
  });

export default i18n;