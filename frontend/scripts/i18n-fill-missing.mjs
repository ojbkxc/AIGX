#!/usr/bin/env node
/**
 * i18n 缺失 key 扫描 / 补齐（一次性工具，可入库）。
 *
 * 背景：前端用「中文原文当 key」的约定（key === 中文文案），t() 的字面量 key
 * 必须同时存在于 zh.json 与 en.json，否则英文界面会回退显示中文。
 *
 * 本脚本：
 *   1. 扫描 src/**\/*.tsx 里的 t('…') / t("…") 字面量 key；
 *   2. 找出 zh.json / en.json 里都不存在的 key；
 *   3. 把缺失 key 追加进 zh.json（value = 中文原文）与
 *      en.json（value = "[TODO en] 中文原文"，**不机翻猜测**，留待人工翻译）；
 *   4. 收尾统一两份文件的 key 集合，确保 zh / en 完全一致。
 *
 * 用法：
 *   node scripts/i18n-fill-missing.mjs            # 扫描并写回（幂等）
 *   node scripts/i18n-fill-missing.mjs --check    # 只检查，有缺失则 exit 1
 *   node scripts/i18n-fill-missing.mjs --dry-run  # 只报告，不写文件
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const srcDir = path.join(root, 'src');
const zhPath = path.join(srcDir, 'i18n', 'zh.json');
const enPath = path.join(srcDir, 'i18n', 'en.json');

const args = new Set(process.argv.slice(2));
const CHECK_ONLY = args.has('--check');
const DRY_RUN = args.has('--dry-run');

/** 只匹配「独立的」 t(...) 调用（排除 Element( / .t( 等误伤） */
// 前置不能是单词字符 / $ / . —— 否则 `createElement(`、`foo.t(` 会被误当 i18n 调用。
const CALL_RE = /(?<![\w$.])t\(\s*(['"])((?:\\.|(?!\1)[\s\S])*?)\1/g;

/** JS 字符串字面量里的转义还原（只处理 key 里可能出现的几种） */
const ESCAPES = { "'": "'", '"': '"', '\\': '\\', n: '\n', r: '\r', t: '\t' };
const unescapeLiteral = (s) => s.replace(/\\(['"\\nrt])/g, (_m, c) => ESCAPES[c]);

/** 递归收集目录下的 .tsx 文件 */
function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else if (entry.isFile() && entry.name.endsWith('.tsx')) out.push(full);
  }
  return out;
}

/** 从所有 tsx 文件里抽取 t() 字面量 key */
function extractKeys(files) {
  const keys = new Set();
  for (const file of files) {
    const code = fs.readFileSync(file, 'utf8');
    for (const m of code.matchAll(CALL_RE)) {
      const key = unescapeLiteral(m[2]);
      if (!key || key.includes('\n') || key.includes('\r')) continue;
      keys.add(key);
    }
  }
  return keys;
}

function main() {
  const files = walk(srcDir);
  const keys = extractKeys(files);

  const zh = JSON.parse(fs.readFileSync(zhPath, 'utf8'));
  const en = JSON.parse(fs.readFileSync(enPath, 'utf8'));

  const missingInZh = [];
  const missingInEn = [];
  const missingInBoth = [];
  for (const k of keys) {
    const inZh = Object.prototype.hasOwnProperty.call(zh, k);
    const inEn = Object.prototype.hasOwnProperty.call(en, k);
    if (!inZh) missingInZh.push(k);
    if (!inEn) missingInEn.push(k);
    if (!inZh && !inEn) missingInBoth.push(k);
  }

  // 收尾：把两份文件的 key 集合对齐（覆盖「只在一边」以及「扫描到但两边都缺」的情况）。
  const union = new Set([...keys, ...Object.keys(zh), ...Object.keys(en)]);
  const addedZh = [];
  const addedEn = [];
  for (const k of [...union].sort((a, b) => a.localeCompare(b))) {
    if (!Object.prototype.hasOwnProperty.call(zh, k)) {
      zh[k] = k; // 中文约定：key === value
      addedZh.push(k);
    }
    if (!Object.prototype.hasOwnProperty.call(en, k)) {
      en[k] = `[TODO en] ${k}`; // 不猜翻译，显式标注待人工处理
      addedEn.push(k);
    }
  }

  console.log(`[i18n] 扫描 ${files.length} 个 .tsx 文件，抽取 t() 字面量 key：${keys.size}`);
  console.log(`[i18n] zh.json=${Object.keys(zh).length}  en.json=${Object.keys(en).length}`);
  console.log(`[i18n] 缺失：zh ${missingInZh.length} 个 / en ${missingInEn.length} 个，两者都缺 ${missingInBoth.length} 个`);
  console.log(`[i18n] 本次补齐：zh +${addedZh.length}，en +${addedEn.length}`);

  // zh / en key 集合一致性校验
  const zhKeys = new Set(Object.keys(zh));
  const enKeys = new Set(Object.keys(en));
  const onlyZh = [...zhKeys].filter((k) => !enKeys.has(k));
  const onlyEn = [...enKeys].filter((k) => !zhKeys.has(k));
  if (onlyZh.length || onlyEn.length) {
    console.warn(`[i18n] ⚠ key 集合仍不一致：only-zh=${onlyZh.length} only-en=${onlyEn.length}`);
  } else {
    console.log('[i18n] ✓ zh / en key 集合完全一致');
  }

  if (CHECK_ONLY) {
    const bad = missingInZh.length + missingInEn.length;
    if (bad > 0) {
      console.error(`[i18n] 检查失败：仍有 ${missingInBoth.length} 个 key 两边都缺`);
      process.exit(1);
    }
    return;
  }
  if (DRY_RUN) return;

  fs.writeFileSync(zhPath, JSON.stringify(zh, null, 2) + '\n', 'utf8');
  fs.writeFileSync(enPath, JSON.stringify(en, null, 2) + '\n', 'utf8');
  console.log('[i18n] 已写回 zh.json / en.json');
}

main();
