// 把 en.json 里 `[TODO en] 中文` 的占位前缀去掉。
// 英文界面不再显示「[TODO en] 三张统计卡」这种丑东西，而是回落到中文原文
// （半翻译 > 带机器标记的半翻译）。key 集合不变，i18n:check 仍绿。
const fs = require('fs');
const path = 'src/i18n/en.json';

const raw = fs.readFileSync(path, 'utf8');
const en = JSON.parse(raw);

const PREFIX = '[TODO en] ';
let n = 0;
const sample = [];
for (const k of Object.keys(en)) {
  const v = en[k];
  if (typeof v === 'string' && v.startsWith(PREFIX)) {
    const cleaned = v.slice(PREFIX.length);
    en[k] = cleaned;
    n++;
    if (sample.length < 5) sample.push([k, cleaned]);
  }
}

fs.writeFileSync(path, JSON.stringify(en, null, 2) + '\n', 'utf8');
console.log('去掉前缀条数:', n);
for (const [k, v] of sample) console.log('  ', k, '=>', String(v).slice(0, 40));

// 复核：不应再有残留
const after = JSON.parse(fs.readFileSync(path, 'utf8'));
const left = Object.keys(after).filter((k) => String(after[k]).includes('[TODO en]'));
console.log('残留:', left.length);
