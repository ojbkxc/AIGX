#!/usr/bin/env node
/**
 * 一条命令完成「重建 static + 报告变更 + 给出精确的 git add 命令」。
 *
 * 为什么需要它：CI 有闸门「提交里的 static 产物必须与提交里的前端源码一致」。
 * 改了 `frontend/src/**` 却忘了 `vite build` 就会红，而报错只说「请执行 npm run
 * build 并提交产物」—— 不告诉你是谁漏了、该 add 哪些路径。共享工作树下这个坑
 * 每轮都要踩一两次。
 *
 * 这个脚本做三件事，**不做第四件**：
 *   1. 跑 `vite build`
 *   2. 列出 `static/` 的变更摘要（新增/删除/修改各多少）
 *   3. 打印可以直接复制的 `git add` 命令
 *
 * **它不自动 git add、不自动 commit** —— 共享工作树里 `static/` 可能混着别人
 * 未提交的产物，自动 add 会把别人的在途改动一起带走。
 *
 * 用法：
 *   cd frontend && node scripts/sync-static.mjs
 *   cd frontend && node scripts/sync-static.mjs --check    # 只检查，不构建
 */
import { execFileSync, execSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const frontend = resolve(here, '..');
const repo = resolve(frontend, '..');
const staticDir = resolve(repo, 'static');

const checkOnly = process.argv.includes('--check');

function run(cmd, cwd) {
  return execSync(cmd, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

/** `git status --porcelain static` 的分组统计 */
function staticStatus() {
  const raw = run('git status --porcelain static', repo);
  const lines = raw.split('\n').filter((l) => l.trim().length > 0);
  const buckets = { added: [], modified: [], deleted: [] };
  for (const line of lines) {
    const code = line.slice(0, 2);
    const path = line.slice(3).trim();
    if (code === '??' || code === 'A ') buckets.added.push(path);
    else if (code.trim() === 'D') buckets.deleted.push(path);
    else buckets.modified.push(path);
  }
  return { lines, buckets };
}

if (!existsSync(staticDir)) {
  console.error('[sync-static] 找不到 static/ 目录：' + staticDir);
  process.exit(1);
}

if (!checkOnly) {
  console.log('[sync-static] vite build …');
  try {
    execFileSync(process.execPath, ['node_modules/vite/bin/vite.js', 'build'], {
      cwd: frontend,
      stdio: 'inherit',
    });
  } catch {
    console.error('[sync-static] 构建失败，先修构建错误（tsc 可能也有问题）');
    process.exit(1);
  }
}

const { lines, buckets } = staticStatus();

if (lines.length === 0) {
  console.log('[sync-static] ✓ static/ 与当前源码一致，无需提交');
  process.exit(0);
}

console.log(
  `[sync-static] static/ 有 ${lines.length} 处变更` +
    `（新增 ${buckets.added.length} / 修改 ${buckets.modified.length} / 删除 ${buckets.deleted.length}）`,
);

// 是否混着别人未提交的前端源码改动 —— 提示风险，但不阻止
let srcDirty = [];
try {
  const raw = run(
    "git status --porcelain -- 'frontend/src' 'frontend/e2e' 'frontend/scripts'",
    repo,
  );
  srcDirty = raw.split('\n').filter((l) => l.trim().length > 0);
} catch {
  /* 忽略 */
}

if (srcDirty.length > 0) {
  console.log('');
  console.log('[sync-static] ⚠ 工作树里还有别人未提交的前端源码改动：');
  for (const line of srcDirty.slice(0, 10)) console.log('    ' + line.trim());
  if (srcDirty.length > 10) console.log(`    …还有 ${srcDirty.length - 10} 条`);
  console.log('[sync-static]   本次产物已把它们的当前状态一起编进去。');
  console.log('[sync-static]   如果你要单独提交自己的改动，先确认这些不是别人的在途工作。');
}

console.log('');
console.log('[sync-static] 提交产物（复制执行）：');
console.log('');
console.log('    cd ' + repo);
console.log('    git add -- static');
console.log('    git commit -F <消息文件> -- static');
console.log('');
console.log('[sync-static] 提示：如果你的提交里也带了前端源码，把源码路径一起写进同一个 commit，');
console.log('[sync-static]       让「源码 ↔ 产物」在同一个提交里对齐，CI 的 static 闸门才不会红。');
