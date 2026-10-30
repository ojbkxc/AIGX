/**
 * 供应商图标（UI 资源逐字取自 Agent2API `ui/assets/providers/`，
 * 放在 `public/providers/` 下由构建原样复制到 `/providers/`）。
 *
 * 没有对应图标的 kind 返回 null，卡片回落到纯文字。
 */

const KIND_ICONS: Record<string, string> = {
  // ── 内置 API Key 供应商 ──
  openai: 'openai.png',
  huggingface: 'huggingface.png',
  nvidia: 'nvidia.png',
  siliconflow: 'siliconflow.png',
  volcengine: 'volcengine-ark.png',
  zai: 'glm.png',
  minimax: 'minimax.png',
  ollama: 'ollama-local.png',
  openai_compatible: 'newapi.png',
  deepseek: 'deepseek.png',
  openrouter: 'openrouter.png',
  anthropic: 'anthropic.png',
  dashscope: 'alicode.png',
  lmstudio: 'ollama-local.png',
  // ── 登录态供应商 ──
  cline: 'cline.png',
  workbuddy: 'workbuddy.png',
  xiaohuanxiong: 'raccoon.png',
  catpaw: 'catpaw.png',
  qoder: 'qoder.png',
  accio: 'accio.png',
  zcode: 'zcode.png',
  codearts: 'codearts.png',
  trae: 'trae.png',
  loomy: 'loomy.png',
  kuku: 'kuku.png',
  monkeycode: 'monkeycode.png',
  commandcode: 'commandcode.png',
  antigravity: 'antigravity.png',
  officeace: 'officeace.png',
  orcaterm: 'orcaterm.png',
  autoclaw: 'autoclaw.png',
};

const PRESET_ICONS: Record<string, string> = {
  openai: 'openai.png',
  anthropic: 'anthropic.png',
  openrouter: 'openrouter.png',
  huggingface: 'huggingface.png',
  nvidia: 'nvidia.png',
  'deepseek-anthropic': 'deepseek.png',
  'glm-cn': 'glm-cn.png',
  'zai-anthropic': 'glm.png',
  'siliconflow-intl': 'siliconflow.png',
  'volcengine-ark': 'volcengine-ark.png',
  'alicode-cn': 'alicode.png',
  'alicode-intl': 'alicode-intl.png',
  'alims-intl': 'alims-intl.png',
  'alitp-intl': 'alitp-intl.png',
  'xiaomi-mimo': 'xiaomi-tokenplan.png',
  'minimax-cn': 'minimax-cn.png',
  sensenova: 'sensenova.png',
};

/** provider kind → 图标 URL（无图标返回 null）。 */
export function providerIcon(kind: string): string | null {
  const file = KIND_ICONS[kind];
  return file ? `/providers/${file}` : null;
}

/** 预置模板 key → 图标 URL（无图标返回 null）。 */
export function presetIcon(key: string): string | null {
  const file = PRESET_ICONS[key];
  return file ? `/providers/${file}` : null;
}
