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
 *   2. `git status --porcelain --untracked-files=no -- static` 为空
 *      —— **只看已跟踪文件的改动**。未跟踪的新 chunk 不影响页面：`index.html`
 *      引用的是它自己那份产物，多出来的文件没人加载。把 `??` 也算 dirty 会
 *      在「别人新增页面产出新 chunk」时误拦，明明可以跑。
 *      需要连未跟踪一起严格判定时加 `--strict-untracked`。
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
const strictUntracked = args.includes('--strict-untracked');

const repoRoot = path.resolve(import.meta.dirname, '..', '..');
const indexPath = path.join(repoRoot, 'static', 'index.html');

function snapshot() {
  if (!fs.existsSync(indexPath)) {
    return { ok: false, why: 'static/index.html 不存在 —— 先 npm run build' };
  }
  const idleSec = (Date.now() - fs.statSync(indexPath).mtimeMs) / 1000;
  let dirty = '';
  try {
    const gitArgs = [
      'status',
      '--porcelain',
      '--untracked-files=no',
      '--',
      'static/assets',
    ];
    if (strictUntracked) {
      gitArgs.splice(2, 1);
      gitArgs[gitArgs.length - 1] = 'static';
    }
    dirty = execFileSync('git', gitArgs, {
      cwd: repoRoot,
      encoding: 'utf8',
    }).trim();
  } catch (err) {
    return { ok: false, why: `git status 失败：${err.message}` };
  }
  // **只统计 M（修改）**：`D`（删除旧 hash chunk）与 `??`（新增 chunk）都是本进程
  // 刚 build 完的正常副产物。前两版判据把 D/?? 也算进去，结果每次都拦住自己的构建
  // ——判据过严和过松一样坏：它会教人「别信它，硬闯」。
  const modified = dirty
    .split('\n')
    .filter((line) => /^ ?M/.test(line));
  if (modified.length > 0) {
    return {
      ok: false,
      why: `static 有 ${modified.length} 个 chunk 正被改写（可能正在重建）`,
    };
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