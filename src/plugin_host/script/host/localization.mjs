// Ported from Agent2API v3.5.0-beta.4 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/host/localization.mjs
const languageKeys = {
  "zh-CN": ["zh-CN", "zh-Hans", "zh"],
  "zh-Hans": ["zh-Hans", "zh-CN", "zh"],
  "zh-Hant": ["zh-Hant", "zh-TW", "zh-HK"],
  "pt-BR": ["pt-BR", "pt"],
}

export function localizedText(value, translations, locale = "zh-CN") {
  const keys = languageKeys[locale] || [locale, locale.split("-")[0]]
  if (translations && typeof translations === "object" && !Array.isArray(translations)) {
    for (const key of keys) {
      const text = translations[key]
      if (typeof text === "string" && text.trim()) return text
    }
  }
  return typeof value === "string" ? value : ""
}
