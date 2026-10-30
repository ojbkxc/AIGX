/**
 * AIGX 前端共享工具
 * clsx + tailwind-merge 组合，参照 new-api / ds-free-api 的 cn() 惯例
 */

import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

/**
 * 合并 className：clsx 处理条件类名，twMerge 解决 Tailwind 冲突
 * @param inputs - 任意 className 组合
 * @returns {string} 合并后的 className
 */
export function cn(...inputs: (string | boolean | undefined | null)[]): string {
  return twMerge(clsx(inputs));
}

/** 除以单位后保留有效位数并去掉小数末尾多余的 0（1.50 → 1.5，120 → 120）。 */
function scaled(n: number, unit: number): string {
  const value = n / unit;
  const digits = Math.abs(value) >= 100 ? 0 : 2;
  const fixed = value.toFixed(digits);
  if (!fixed.includes('.')) return fixed;
  return fixed.replace(/0+$/, '').replace(/\.$/, '');
}

/**
 * 格式化大数字，使用中文单位（亿 / 万），供统计卡片与额度展示使用
 * @param {number|null} val - 数值
 * @returns {string} 格式化结果，null 显示 —
 */
export function fmtCompact(val: number | null | undefined): string {
  if (val == null) return '—';
  const n = Number(val);
  if (!Number.isFinite(n)) return '—';
  const abs = Math.abs(n);
  if (abs >= 1e8) return scaled(n, 1e8) + '亿';
  if (abs >= 1e4) return scaled(n, 1e4) + '万';
  return n.toLocaleString('zh-CN');
}

/**
 * 额度展示（缺省按 0）：与 fmtCompact 同一套中文单位，undefined/null 显示 0 而不是 —
 */
export function fmtQuota(val: number | null | undefined): string {
  const n = Number(val ?? 0);
  return Number.isFinite(n) ? fmtCompact(n) : '0';
}

/**
 * 判断当前用户是否管理员（从 localStorage 读取）
 * 后端角色字段以 /api/users/me 返回为准，此处仅做展示层过滤，
 * 真正的权限校验必须由后端执行。
 * @returns {boolean}
 */
export function isAdmin(): boolean {
  try {
    const role = localStorage.getItem('role');
    return role === 'admin';
  } catch {
    return false;
  }
}
