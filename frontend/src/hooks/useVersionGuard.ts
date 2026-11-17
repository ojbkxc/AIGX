/**
 * 前后端版本握手 —— 防止「浏览器跑着旧 JS、后端已经是新版」。
 *
 * ── 为什么需要它 ─────────────────────────────────────────────
 * `index.html` 是 `no-cache`、带哈希的 assets 是 `immutable`，所以**刷新一次**
 * 一定拿到匹配的前后端。真正的问题是**已经打开的标签页**：它的 JS 已经在内存里，
 * 之后的接口调用都对着新后端，字段/语义可能已经变了，而用户毫无察觉。
 *
 * ── 判据 ────────────────────────────────────────────────────
 * 构建时把版本号烧进 bundle（`__APP_VERSION__`，见 vite.config.ts）；
 * 定时回源读 `version.json`（构建产物同目录、同样 no-store），
 * 两者不同 = 「我这份 JS 不是当前部署的那份」。
 *
 * 服务端侧的 `update::static_version` / `versionMismatch` 只能看出
 * 「磁盘上的 static 与二进制不同版」，看不出浏览器内存里跑的是哪一版 ——
 * 两者互补，缺一不可。
 */
import { useEffect, useState } from 'react';

/** 构建时注入的版本号（vite define）。 */
declare const __APP_VERSION__: string;

/** 轮询间隔：版本变更是低频事件，1 分钟足够且几乎无成本。 */
const POLL_INTERVAL_MS = 60_000;

/** 返回「当前部署的前端版本」，读取失败返回 null（不误报）。 */
async function fetchDeployedVersion(): Promise<string | null> {
  try {
    const res = await fetch(`/version.json?ts=${Date.now()}`, { cache: 'no-store' });
    if (!res.ok) return null;
    const data = (await res.json()) as { version?: string | null };
    const version = typeof data.version === 'string' ? data.version.trim() : '';
    return version || null;
  } catch {
    return null;
  }
}

/**
 * 是否「已部署新版本而后未刷新」。
 *
 * 只在**两边都有值且不同**时报 true：构建缺失版本号（开发态 / 手工部署）
 * 时静默，不制造假告警。
 */
export function useVersionGuard(): { stale: boolean; deployed: string | null } {
  const [deployed, setDeployed] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      const version = await fetchDeployedVersion();
      if (!cancelled) setDeployed(version);
    };
    void check();
    const timer = window.setInterval(() => void check(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const baked = typeof __APP_VERSION__ === 'string' ? __APP_VERSION__.trim() : '';
  const stale = Boolean(baked && deployed && baked !== deployed);
  return { stale, deployed };
}
