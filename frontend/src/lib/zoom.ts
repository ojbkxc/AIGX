/**
 * 界面缩放 — 对照 Agent2API「设置 → 显示 → 界面缩放」。
 *
 * 桌面壳用原生 webview zoom；Web 端用 CSS `zoom`（Chrome/Edge/Safari 与
 * Firefox 126+ 均支持），对整页做等比缩放。选择持久化在 localStorage，
 * 首屏由 `initZoom()` 应用，避免加载后再跳变的闪动。
 */

const STORAGE_KEY = 'ui-zoom';

/** 可选档位：80%–130%，步长 5%。 */
export const ZOOM_LEVELS: number[] = [0.8, 0.85, 0.9, 0.95, 1, 1.05, 1.1, 1.15, 1.2, 1.25, 1.3];

/** 读取持久化的缩放比例（非法值回退 1.0）。 */
export function getZoom(): number {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return 1;
    const value = Number(raw);
    if (Number.isFinite(value) && value >= 0.8 && value <= 1.3) return value;
  } catch {
    // localStorage 不可用时回退默认
  }
  return 1;
}

/** 应用缩放并持久化。 */
export function applyZoom(scale: number): void {
  const clamped = Math.min(1.3, Math.max(0.8, scale));
  const root = document.documentElement;
  if (clamped === 1) {
    root.style.removeProperty('zoom');
  } else {
    root.style.setProperty('zoom', String(clamped));
  }
  try {
    localStorage.setItem(STORAGE_KEY, String(clamped));
  } catch {
    // 忽略持久化失败
  }
}

/** 首屏初始化（main.tsx 调用）。 */
export function initZoom(): void {
  applyZoom(getZoom());
}
