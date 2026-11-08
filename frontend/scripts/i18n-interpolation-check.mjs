#!/usr/bin/env node
/**
 * i18n 插值体检闸门（#57）——键值花括号形态一致性。
 *
 * 背景（#45/#56 两次事故）：i18next 默认只插值 `{{x}}`，键/值写成 `{x}` 时
 * 界面渲染字面量 `{n}`（中英文都坏）。i18n:check 只查键集存在性，
 * 占位符**形态**漂移对它不可见——本脚本补上这道闸。
 *
 * 检查：扫 src/i18n/*.json 全部语言包，逐键比较键侧与值侧的占位符形态——
 *   - 键侧用双花括号 `{{x}}`（i18next 插值约定）而值侧只有单花括号 `{x}`
 *     → 英文/中文界面渲染字面量，exit 1。
 *   - 反向（键侧单括号、值侧双括号）→ 同样报（形态漂移的另一半）。
 * 正则用 lookaround（verifier 口径）：`(?<!\{)\{(\w+)\}(?!\})`，先剥掉
 * `{{x}}` 再数单括号，避免双括号被误判成单。
 *
 * 白名单：键侧本身写单花括号、走动态拼接/文档占位体系的 7 个键
 * （CUSTOM_BASE_HINT 长版×3 + hintKey 短版×3 + 列对齐 label×1，判定见
 * 89f50547 的 #56 结论）——这些键的 `{base}`/`{name}` 是文档占位语义，
 * 单括号是既有约定，不视为病灶。
 *
 * 用法：node scripts/i18n-interpolation-check.mjs（零依赖；CI 与本地同跑）
 *   --check（默认）发现即 exit 1 并列键名与语言包。
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const i18nDir = path.join(root, 'src', 'i18n');
const PACKS = ['zh.json', 'en.json', 'zh-Hant.json', 'ja.json', 'ko.json', 'pt-BR.json'];

/** 白名单：键侧本身写单花括号的动态拼接/文档占位键。
 *  - CUSTOM_BASE_HINT 长版×3（AddAccountDialog:61-63，t(值) 无 options，
 *    {base} 为文档占位语义——89f50547/#56 判定）
 *  （「{{name}}」列的对齐与 hintKey 短版×3 已随 S1/#57 孤儿清理删除，
 *    白名单同步收敛——0fa531cc 即删调用点，键无消费） */
const WHITELIST = new Set([
  'OpenAI 兼容填到 /v1，按 {base}/chat/completions 转发',
  'Anthropic 协议填根地址，不要带 /v1，按 {base}/v1/messages 转发',
  'Gemini 协议填到 /v1beta，按 {base}/models 转发',
]);

/** 剥掉 {{x}} 后再抓单括号 {x}（lookaround 口径） */
const DOUBLE_RE = /\{\{(\w+)\}\}/g;
const SINGLE_RE = /(?<!\{)\{(\w+)\}(?!\})/g;
const doubles = (s) => [...String(s).matchAll(DOUBLE_RE)].map((m) => m[1]).sort();
const singles = (s) => [...String(s).matchAll(SINGLE_RE)].map((m) => m[1]).sort();

const problems = [];
const bothSingle = new Set();

for (const pack of PACKS) {
  const p = path.join(i18nDir, pack);
  if (!fs.existsSync(p)) {
    problems.push(`[missing] 语言包不存在: ${pack}`);
    continue;
  }
  const dict = JSON.parse(fs.readFileSync(p, 'utf-8'));
  for (const [key, value] of Object.entries(dict)) {
    if (typeof value !== 'string' || WHITELIST.has(key)) continue;
    const keyD = doubles(key);
    const keyS = singles(key);
    const valD = doubles(value);
    const valS = singles(value);
    // 病灶一：键侧声明了 {{x}} 插值，值侧却写成 {x}（#45/#56 主体形态）
    const missed = keyD.filter((name) => !valD.includes(name) && valS.includes(name));
    if (missed.length) {
      problems.push(`[${pack}] 值侧单花括号: ${key}\n         值: ${value.slice(0, 90)}\n         缺失插值: ${missed.map((m) => `{{${m}}}`).join(' ')}`);
    }
    // 病灶二（反向）：键侧单括号（文档占位约定），值侧却改成了 {{x}}
    const extra = keyS.filter((name) => valD.includes(name) && !valS.includes(name));
    if (keyS.length && extra.length) {
      problems.push(`[${pack}] 键侧单括号键的值侧混入双括号: ${key}\n         值: ${value.slice(0, 90)}`);
    }
    // 病灶三：键值均单括号——i18next 看不到 {{}} 不插值，且无白名单背书。
    // 这类若同时有 t() 调用点传 options 即 #45 A 类真缺陷（渲染字面量）；
    // 调用点由下方 t() 扫描联合判定（本键记入单括号键集）。
    if (keyS.length && keyS.length === singles(key).length && keyD.length === 0
      && valD.length === 0 && valS.length > 0 && !WHITELIST.has(key)) {
      bothSingle.add(key);
    }
  }
}

/* 病灶三联合判定：单括号键 × t() 调用点传 options → 报。
 * 扫 src/**\/*.{ts,tsx} 的 t('键') 第二参（`t('…', {` 或 `t("…", {`）。
 * 零依赖：正则抽取，逐键验证；孤儿键（无 t() 调用）不在此闸门域（#51 管辖）。 */
const srcDir = path.join(root, 'src');
const callFiles = [];
const walk = (dir) => {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p);
    else if (/\.(ts|tsx)$/.test(e.name) && !/\.test\./.test(e.name) && !p.includes(`${path.sep}i18n${path.sep}`)) callFiles.push(p);
  }
};
walk(srcDir);
const CALL_RE = /\bt\(\s*(['"])((?:\\.|(?!\1)[\s\S])*?)\1\s*,\s*\{/g;
const callsWithOptions = new Map();
for (const f of callFiles) {
  const text = fs.readFileSync(f, 'utf-8');
  for (const m of text.matchAll(CALL_RE)) {
    const raw = m[2].replace(/\\'/g, "'").replace(/\\"/g, '"');
    if (!callsWithOptions.has(raw)) callsWithOptions.set(raw, path.relative(root, f));
  }
}
for (const key of bothSingle) {
  if (callsWithOptions.has(key)) {
    problems.push(`[t() 调用点] 键值均单花括号且 t() 传了插值参数（i18next 不插值 → 渲染字面量）: ${key}\n         调用点: ${callsWithOptions.get(key)}`);
  }
}

if (problems.length) {
  console.error(`[i18n-interpolation] 发现 ${problems.length} 处键值花括号形态不一致：`);
  for (const p of problems) console.error('  ' + p);
  process.exit(1);
}
console.log(`[i18n-interpolation] PASS：${PACKS.length} 个语言包键值花括号形态一致（白名单 ${WHITELIST.size} 键）。`);