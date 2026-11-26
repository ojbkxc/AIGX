import i18n, { type InitOptions } from 'i18next';
import { initReactI18next } from 'react-i18next';

export type AppLanguage = 'zh' | 'en' | 'zh-Hant';

/** 支持的语言清单（侧边栏/设置页切换器用）
 *
 * 业主 2026-11-27 裁定：**只做中文与英文**。ja / ko / pt-BR 不提供（不再保留
 * 空占位包——选了会静默 fallback 回中文，属「看起来能切但没生效」的缺陷；
 * 也不收未复核的机翻）。zh-Hant 是中文繁体，已补齐 5876 键，但暂留在切换器
 * 外等质量确认后再上架。 */
export const SUPPORTED_LANGS: Array<{ code: AppLanguage; label: string }> = [
  { code: 'zh', label: '简体中文' },
  { code: 'en', label: 'English' },
];

// 语言检测：优先 localStorage 的 'i18n_lang'，其次浏览器 navigator.language，默认 zh
function detectLanguage(): AppLanguage {
  const saved = localStorage.getItem('i18n_lang');
  if (saved && SUPPORTED_LANGS.some((l) => l.code === saved)) {
    return saved as AppLanguage;
  }
  const nav = (navigator.language || '').toLowerCase();
  if (nav.startsWith('en')) return 'en';
  if (nav.startsWith('zh-hant') || nav.startsWith('zh-tw') || nav.startsWith('zh-hk')) return 'zh-Hant';
  // 其余语言（ja/ko/pt-BR 等）业主裁定不提供 → 回落中文
  return 'zh';
}

// ── 语言包全部按需加载 ─────────────────────────────
// 所有语言包（含 zh）都走动态 import，被 Vite 拆成独立 chunk，不进主包。
// 英文用户首屏只加载 en.json（≈461KB），不再被 zh.json（≈439KB）拖累；
// 中文用户启动时按需加载 zh，繁体中文同理。
const loadedLangs = new Set<AppLanguage>();

/** 各语言包的动态 import 映射（含 zh，与 en 同款加载模式） */
const bundleImports: Record<AppLanguage, () => Promise<unknown>> = {
  zh: () => import('./zh.json'),
  en: () => import('./en.json'),
  'zh-Hant': () => import('./zh-Hant.json'),
};

/** 按需加载某语言资源包（已加载则直接返回） */
async function loadBundle(lang: AppLanguage): Promise<void> {
  if (loadedLangs.has(lang)) return;
  const loader = bundleImports[lang];
  if (!loader) return;
  const mod = await loader();
  const bundle = ((mod as { default?: Record<string, string> }).default ?? (mod as Record<string, string>));
  i18n.addResourceBundle(lang, 'translation', bundle, true, true);
  loadedLangs.add(lang);
}

const initialLang = detectLanguage();

const initOptions: InitOptions = {
  // 不预置任何语言包：全部由 loadBundle 按需 addResourceBundle。
  // zh / en 的 key 集合完全一致（脚本 i18n:check 校验），故英文用户缺键时
  // 直接回落到「key 本身」（本项目的 key 就是中文原文），无需再加载 zh。
  resources: {},
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
 * - 先把初始语言的语言包动态 import 进来再 resolve，避免首帧闪 key/中文；
 * - 中文用户启动即加载 zh，英文用户只加载 en（不预载 zh）。
 */
export const i18nReady: Promise<void> = (async () => {
  await i18n.use(initReactI18next).init(initOptions);
  // 语言切换时按需补包（用户手动切换 / geo-lang 端点在初始化后改写都会走到这里）
  i18n.on('languageChanged', (lng: string) => {
    if (!lng) return;
    const code = lng.split('-')[0].toLowerCase();
    const match = SUPPORTED_LANGS.find(
      (l) => l.code === lng || l.code.toLowerCase().startsWith(code)
    );
    if (!match) return;
    const alreadyLoaded = loadedLangs.has(match.code);
    void loadBundle(match.code).then(() => {
      // 资源包是「切语言之后」才补齐的 → 再切一次触发 react-i18next 重渲染。
      if (!alreadyLoaded) void i18n.changeLanguage(lng);
    });
  });
  await loadBundle(initialLang);
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
  localStorage.setItem('i18n_lang', lang);
  // 先确保资源包到位再切语言，避免目标语言首帧回退中文
  void loadBundle(lang).then(() => i18n.changeLanguage(lang));
  return lang;
}

