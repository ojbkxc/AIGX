#!/usr/bin/env node
/**
 * 检查「某个页面用到的 i18n key，英文值里是否还残留中文」。
 *
 * 为什么需要：`i18n:check`（`i18n-fill-missing.mjs --check`）只保证**两边 key 集合
 * 一致**，不保证 en 的值真的是英文。`[TODO en]` 前缀被清掉之后，466 个键的值直接
 * 变成了中文原文 —— key 集合一致、检查全绿，而英文界面在显示中文。**「集合一致」
 * 不等于「翻译正确」，这两件事必须分开验。**
 *
 * 用法：
 *   node scripts/i18n-no-chinese-in-page.mjs src/pages/PurgeConsole.tsx
 *   node scripts/i18n-no-chinese-in-page.mjs src/pages/PurgeConsole.tsx --allow "中文"
 *
 * 退出码：0 = 该页用到的键都有非中文英文值；1 = 有残留（逐条点名）。
 *
 * 范围刻意限定为「某一页」：全仓还有几百个中文值属于别的页，一次全修会掩盖
 * 本页的问题；按页推进才能既守住新代码、又不假装全仓已翻译。
 */
import fs from 'node:fs';
import path from 'node:path';

const args = process.argv.slice(2);
const pageFile = args.find((a) => !a.startsWith('--'));
const allowIdx = args.indexOf('--allow');
const allowed = allowIdx >= 0 ? args[allowIdx + 1] : null;

if (!pageFile) {
  console.error('用法: node scripts/i18n-no-chinese-in-page.mjs <page.tsx> [--allow "中文"]');
  process.exit(2);
}

const root = path.resolve(import.meta.dirname, '..');
const en = JSON.parse(fs.readFileSync(path.join(root, 'src/i18n/en.json'), 'utf8'));
const src = fs.readFileSync(path.join(root, pageFile), 'utf8');

// 抓 t('…') / t("…") 的单行字面量 key（含 {{n}} 这类插值）
const keys = new Set();
for (const m of src.matchAll(/\bt\(\s*'([^'\\]+)'/g)) keys.add(m[1]);
for (const m of src.matchAll(/\bt\(\s*"([^"\\]+)"/g)) keys.add(m[1]);

/**
 * 扫「绕过 `t()` 的中文字面量」——JSX 文本节点与硬编码属性值。
 *
 * 这类坏法**前三道检查都看不到**：它根本没进 json，所以 key 集合一致、英文值无残留、
 * 占位符全对；而英文界面直接显示中文。破甲页实测有 1 处
 * （`title={\`用「${rule.from}」做预览样例\`}`），是这轮才补上的盲区。
 *
 * 只认「渲染到界面上」的位置，避免把常量表里的 `label:`/`desc:` 误判成缺陷
 * （那些值在使用处都包了 `t()`）。
 */
function scanHardcodedCjk(src) {
  const lines = src.split('\n');
  const found = [];
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (!/[\u4e00-\u9fff]/.test(line)) continue;
    if (/^\s*(\/\/|\*|\/\*)/.test(line)) continue; // 整行注释

    // ① JSX 文本节点：`>…中文…<`（排除 {…} 表达式）
    for (const m of line.matchAll(/>\s*([^<>{}]*[\u4e00-\u9fff][^<>{}]*?)\s*</g)) {
      found.push({ line: i + 1, kind: 'jsx-text', text: m[1].trim().slice(0, 60) });
    }
    // ② 硬编码属性值
    for (const m of line.matchAll(
      /\b(title|aria-label|placeholder|alt)\s*=\s*(["'])([^"']*[\u4e00-\u9fff][^"']*)\2/g,
    )) {
      found.push({ line: i + 1, kind: `attr:${m[1]}`, text: m[3].trim().slice(0, 60) });
    }
    // ③ 属性里的模板串且不含 t(
    for (const m of line.matchAll(
      /\b(title|aria-label|placeholder)\s*=\s*\{\s*`([^`]*[\u4e00-\u9fff][^`]*)`\s*\}/g,
    )) {
      if (/t\(/.test(m[2])) continue;
      found.push({ line: i + 1, kind: `tpl:${m[1]}`, text: m[2].trim().slice(0, 60) });
    }
  }
  const seen = new Set();
  return found.filter((h) => {
    const k = `${h.line}|${h.text}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

const CJK = /[\u4e00-\u9fff]/;
const bad = [];
const phMismatch = [];
const placeholders = (v) =>
  new Set([...String(v).matchAll(/\{\{\s*(\w+)\s*\}\}/g)].map((m) => m[1]));

const zh = JSON.parse(fs.readFileSync(path.join(root, 'src/i18n/zh.json'), 'utf8'));

for (const key of keys) {
  const value = en[key];
  if (typeof value !== 'string') continue; // 缺失由 i18n --check 负责
  if (CJK.test(value) && !(allowed && value.includes(allowed))) {
    bad.push({ key, value });
  }
  // 占位符必须与 zh 一致：zh 用 `{{n}}`、en 写成 `{{count}}` 时，英文界面会
  // 原样显示 `{{count}}` 字面量——key 集合一致、检查全绿，界面却是坏的。
  if (typeof zh[key] === 'string') {
    const a = placeholders(zh[key]);
    const b = placeholders(value);
    const onlyZh = [...a].filter((x) => !b.has(x));
    const onlyEn = [...b].filter((x) => !a.has(x));
    if (onlyZh.length || onlyEn.length) phMismatch.push({ key, onlyZh, onlyEn });
  }
}

const hardcoded = scanHardcodedCjk(src);

if (bad.length === 0 && phMismatch.length === 0 && hardcoded.length === 0) {
  console.log(
    `[i18n-en] ✓ ${pageFile} 用到的 ${keys.size} 个键：英文值无中文残留、占位符与中文一致、无硬编码中文`,
  );
  process.exit(0);
}

if (bad.length > 0) {
  console.error(`[i18n-en] 检查失败：${pageFile} 有 ${bad.length} 个键的英文值仍是中文`);
  for (const item of bad.slice(0, 30)) {
    console.error(`  - ${item.key.slice(0, 50)}`);
    console.error(`      => ${String(item.value).slice(0, 60)}`);
  }
  if (bad.length > 30) console.error(`  …还有 ${bad.length - 30} 条`);
}
if (phMismatch.length > 0) {
  console.error(`[i18n-en] 检查失败：${phMismatch.length} 个键的插值占位符与中文不一致`);
  for (const item of phMismatch.slice(0, 20)) {
    console.error(`  - ${item.key.slice(0, 50)}`);
    if (item.onlyZh.length) console.error(`      zh 有而 en 缺: {{${item.onlyZh.join('}}, {{')}}}`);
    if (item.onlyEn.length) console.error(`      en 有而 zh 缺: {{${item.onlyEn.join('}}, {{')}}}`);
  }
}
if (hardcoded.length > 0) {
  console.error(
    `[i18n-en] 检查失败：${pageFile} 有 ${hardcoded.length} 处中文绕过了 t()（英文界面会显示中文）`,
  );
  for (const item of hardcoded.slice(0, 20)) {
    console.error(`  - L${item.line} [${item.kind}] ${item.text}`);
  }
  if (hardcoded.length > 20) console.error(`  …还有 ${hardcoded.length - 20} 处`);
}
process.exit(1);
