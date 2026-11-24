#!/usr/bin/env node
/**
 * 等 `static/` 静止后再跑 e2e。
 *
 * 背景：破甲页 e2e 托管 `static/`（构建产物）。同树多 agent 时，别人跑
 * `npm run build` 的十几秒里 `static/` 正被替换——这期间起 e2e 会加载到半新
 * 半旧的 chunk，表现为「失败条数随重跑变化」（3 failed → 5 failed → 单跑全过）。
 * 那种波动不是 flaky，是环境在动。
 *
 * 判据（两条都满足才算静止）：
 *   1. `static/index.html` 的 mtime 距今 ≥ `--min-idle` 秒（默认 20）
 *   2. `git status --porcelain -- static` 为空（没有构建产物等待提交）
 *
 * 用法：
 *   node scripts/wait-static-still.mjs            # 检查，不满足直接退出 1
 *   node scripts/wait-static-still.mjs --wait 90  # 最多等 90s，等到静止再返回
 */
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = args.indexOf(name);
  return i >= 0 && args[i + 1] ? Number(args[i + 1]) : fallback;
};
const minIdleSec = flag('--min-idle', 20);
const maxWaitSec = flag('--wait', 0);

const repoRoot = path.resolve(import.meta.dirname, '..', '..');
const indexPath = path.join(repoRoot, 'static', 'index.html');

function snapshot() {
  if (!fs.existsSync(indexPath)) {
    return { ok: false, why: 'static/index.html 不存在 —— 先 npm run build' };
  }
  const idleSec = (Date.now() - fs.statSync(indexPath).mtimeMs) / 1000;
  let dirty = '';
  try {
    dirty = execFileSync('git', ['status', '--porcelain', '--', 'static'], {
      cwd: repoRoot,
      encoding: 'utf8',
    }).trim();
  } catch (err) {
    return { ok: false, why: `git status 失败：${err.message}` };
  }
  if (dirty) {
    const count = dirty.split('\n').length;
    return { ok: false, why: `static 有 ${count} 个未提交改动（可能正在重建）` };
  }
  if (idleSec < minIdleSec) {
    return { ok: false, why: `index.html 刚被改过 ${idleSec.toFixed(0)}s 前（阈值 ${minIdleSec}s）` };
  }
  return { ok: true, idleSec };
}

const deadline = Date.now() + maxWaitSec * 1000;
for (;;) {
  const result = snapshot();
  if (result.ok) {
    console.log(`[static-still] OK（已静止 ${result.idleSec.toFixed(0)}s）`);
    process.exit(0);
  }
  if (Date.now() >= deadline) {
    console.error(`[static-still] 未静止：${result.why}`);
    console.error('[static-still] 现在跑 e2e 的结果不可信——等 static 静止后再跑，或加 --wait 90');
    process.exit(1);
  }
  console.log(`[static-still] 等待中：${result.why}`);
  await new Promise((r) => setTimeout(r, 3000));
}