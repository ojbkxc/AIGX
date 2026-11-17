import { useEffect, useRef } from 'react';

/**
 * Cloudflare Turnstile 组件（安全策略 → 机器人校验提供方 = turnstile 时启用）。
 *
 * 与 ALTCHA 的无感 PoW 不同：Turnstile 由 Cloudflare 托管并在页面内渲染一个
 * 小组件，token 由组件回调产出；`render=explicit` 模式下我们从
 * `window.turnstile.render` 拿 widget id，卸载时 remove。脚本全局只加载一次。
 */

type TurnstileApi = {
  render: (el: HTMLElement, options: Record<string, unknown>) => string;
  remove: (widgetId: string) => void;
};

declare global {
  interface Window {
    turnstile?: TurnstileApi;
  }
}

const SCRIPT_ID = 'cf-turnstile-script';
const SCRIPT_URL = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit';

/** 确保 Cloudflare 脚本已加载（并发挂载共用一个 script 标签）。 */
function loadTurnstile(): Promise<TurnstileApi> {
  if (window.turnstile) return Promise.resolve(window.turnstile);
  return new Promise((resolve, reject) => {
    const existing = document.getElementById(SCRIPT_ID) as HTMLScriptElement | null;
    const onload = (): void => {
      if (window.turnstile) resolve(window.turnstile);
      else reject(new Error('turnstile unavailable after load'));
    };
    if (existing) {
      existing.addEventListener('load', onload, { once: true });
      existing.addEventListener('error', () => reject(new Error('turnstile load failed')), {
        once: true,
      });
      return;
    }
    const script = document.createElement('script');
    script.id = SCRIPT_ID;
    script.src = SCRIPT_URL;
    script.async = true;
    script.defer = true;
    script.onload = onload;
    script.onerror = () => reject(new Error('turnstile load failed'));
    document.head.appendChild(script);
  });
}

export default function TurnstileWidget({
  siteKey,
  onToken,
  onError,
}: {
  siteKey: string;
  onToken: (token: string) => void;
  onError: () => void;
}): JSX.Element {
  const container = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let widgetId: string | null = null;
    let disposed = false;
    if (!siteKey) return undefined;
    void loadTurnstile()
      .then((api) => {
        if (disposed || !container.current) return;
        widgetId = api.render(container.current, {
          sitekey: siteKey,
          theme: 'auto',
          callback: (token: string) => onToken(token),
          'error-callback': () => onError(),
          // token 默认 5 分钟；过期交给组件自动刷新（自动刷新会再次回调）。
          'expired-callback': () => onToken(''),
        });
      })
      .catch(() => {
        if (!disposed) onError();
      });
    return () => {
      disposed = true;
      if (widgetId && window.turnstile) {
        try {
          window.turnstile.remove(widgetId);
        } catch {
          // 组件已被页面卸载：忽略
        }
      }
    };
  }, [siteKey, onToken, onError]);

  return (
    <div
      ref={container}
      style={{ display: 'flex', justifyContent: 'center', marginBottom: 12 }}
    />
  );
}
