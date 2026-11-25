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

const CJK = /[\u4e00-\u9fff]/;
const bad = [];
for (const key of keys) {
  const value = en[key];
  if (typeof value !== 'string') continue; // 缺失由 i18n --check 负责
  if (!CJK.test(value)) continue;
  if (allowed && value.includes(allowed)) continue;
  bad.push({ key, value });
}

if (bad.length === 0) {
  console.log(`[i18n-en] ✓ ${pageFile} 用到的 ${keys.size} 个键，英文值均不含中文`);
  process.exit(0);
}

console.error(`[i18n-en] 检查失败：${pageFile} 有 ${bad.length} 个键的英文值仍是中文`);
for (const item of bad.slice(0, 30)) {
  console.error(`  - ${item.key.slice(0, 50)}`);
  console.error(`      => ${String(item.value).slice(0, 60)}`);
}
if (bad.length > 30) console.error(`  …还有 ${bad.length - 30} 条`);
process.exit(1);
