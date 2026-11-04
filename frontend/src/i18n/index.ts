import i18n, { type InitOptions } from 'i18next';
import { initReactI18next } from 'react-i18next';
import zh from './zh.json';

export type AppLanguage = 'zh' | 'en';

// 语言检测：优先 localStorage 的 'i18n_lang'，其次浏览器 navigator.language，默认 zh
function detectLanguage(): AppLanguage {
  const saved = localStorage.getItem('i18n_lang');
  if (saved === 'zh' || saved === 'en') return saved;
  const nav = (navigator.language || '').toLowerCase();
  if (nav.startsWith('en')) return 'en';
  return 'zh';
}

// ── 语言包按需加载 ─────────────────────────────────────────────
// 只有「中文」随首屏同步打包（zh.json 静态 import，同时是 fallbackLng）；
// 英文包走动态 import，被 Vite 拆成独立 chunk，非英文用户不再下载 ~118KB。
// 代价：切到英文时多一次 chunk 请求 —— 由 i18nReady / loadBundle 保证在
// 挂载前（或切换前）完成，避免英文界面首帧回退中文。
const loadedLangs = new Set<AppLanguage>(['zh']);

/** 按需加载某语言资源包（已加载则直接返回） */
async function loadBundle(lang: AppLanguage): Promise<void> {
  if (loadedLangs.has(lang)) return;
  const mod = await import('./en.json');
  const bundle = (mod.default ?? (mod as unknown)) as Record<string, string>;
  i18n.addResourceBundle(lang, 'translation', bundle, true, true);
  loadedLangs.add(lang);
}

const initialLang = detectLanguage();

const initOptions: InitOptions = {
  resources: {
    // 默认语言（也是 fallback）同步内置；英文包见 loadBundle。
    zh: { translation: zh },
  },
  lng: initialLang,
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

/**
 * i18n 就绪 Promise：main.tsx 必须 await 后再挂载 React。
 * - 默认（中文）用户：init 完成即可，无额外请求；
 * - 英文用户：先把英文包 import 进来再 resolve，避免首帧闪中文。
 */
export const i18nReady: Promise<void> = (async () => {
  await i18n.use(initReactI18next).init(initOptions);
  // 语言切换时按需补包（用户手动切换 / geo-lang 端点在初始化后改写都会走到这里）
  i18n.on('languageChanged', (lng: string) => {
    if (!lng || !lng.startsWith('en')) return;
    const alreadyLoaded = loadedLangs.has('en');
    void loadBundle('en').then(() => {
      // 资源包是「切语言之后」才补齐的 → 再切一次触发 react-i18next 重渲染。
      // alreadyLoaded 时无需重切（不会形成死循环：loadedLangs 已含 en）。
      if (!alreadyLoaded) void i18n.changeLanguage(lng);
    });
  });
  if (initialLang === 'en') await loadBundle('en');
  await i18n.changeLanguage(initialLang);
})();

export default i18n;

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
        // 切到 en 时 languageChanged 监听会补英文包，无需在此等待
        void i18n.changeLanguage(lang);
      }
    })
    .catch(() => {
      /* 网络失败静默回退，保持现有语言 */
    });
}

/**
 * 切换界面语言并持久化到 localStorage。
 * 供 Sidebar / 设置页语言切换器调用。
 */
export function setLanguage(lang: AppLanguage): AppLanguage {
  const next: AppLanguage = lang === 'en' ? 'en' : 'zh';
  localStorage.setItem('i18n_lang', next);
  // 先确保资源包到位再切语言，避免目标语言首帧回退中文
  void loadBundle(next).then(() => i18n.changeLanguage(next));
  return next;
}

/**
 * 当前界面语言（'zh' | 'en'）。
 */
export function getLanguage(): AppLanguage {
  return i18n.language && i18n.language.startsWith('en') ? 'en' : 'zh';
}
