/**
 * AIGX 前端共享工具（100年不过时的接口层）
 * clsx + tailwind-merge 组合，参照 new-api / ds-free-api 的 cn() 惯例
 */

import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

/**
 * 合并 className：clsx 处理条件类名，twMerge 解决 Tailwind 冲突
 * @param {...any} inputs - 任意 className 组合
 * @returns {string} 合并后的 className
 */
export function cn(...inputs: (string | boolean | undefined | null)[]): string {
  return twMerge(clsx(inputs));
}

/**
 * 格式化大数字（K/M/B），供统计卡片使用
 * @param {number|null} val - 数值
 * @returns {string} 格式化结果，null 显示 —
 */
export function fmtCompact(val: number | null | undefined): string {
  if (val == null) return '—';
  const n = Number(val);
  if (!Number.isFinite(n)) return '—';
  if (n >= 1e9) return (n / 1e9).toFixed(1) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'K';
  return n.toLocaleString('zh-CN');
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