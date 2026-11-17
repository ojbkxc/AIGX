import { useCallback, useEffect, useRef, useState } from 'react';
import { buildPayload, solveChallenge, type AltchaChallenge } from '../lib/altcha';

/** 领题端点：与服务端路由、后端 `handle_captcha` 三处同名。 */
const CHALLENGE_URL = '/api/auth/captcha';
/** 题目寿命：服务端 10 分钟过期，提前 30 秒重领，避免踩线提交拿到「已过期」。 */
const REFRESH_MS = 9.5 * 60 * 1000;

export type AltchaState = 'disabled' | 'verifying' | 'verified' | 'error';
/** 机器人校验提供方：后端 `captcha_provider` 决定，前端按它选组件。 */
export type CaptchaProvider = 'altcha' | 'turnstile';

/**
 * 机器人校验（ALTCHA proof-of-work / Cloudflare Turnstile）——登录页 / 注册页共用。
 *
 * 挂载即领题：ALTCHA 由本 hook 在浏览器里求解（与官方 widget 同观感）；
 * Turnstile 只把 `siteKey` 交给页面渲染 Cloudflare 组件，token 回调后置
 * `verified`。开关关闭时领题端点回 400，本 hook 落到 `disabled`，页面据此
 * 隐藏校验块、提交时不带 captcha 字段（后端同样放行）。
 */
export function useAltcha(): {
  state: AltchaState;
  payload: string | null;
  reset: () => void;
  provider: CaptchaProvider;
  siteKey: string;
  /** Turnstile 组件拿到 token 后回填（payload = token）。 */
  setTurnstileToken: (token: string) => void;
  /** Turnstile 组件报错 / token 过期。 */
  markError: () => void;
  /** Turnstile 重试用：变化时页面按新 key 重挂组件。 */
  widgetKey: number;
} {
  const [state, setState] = useState<AltchaState>('verifying');
  const [payload, setPayload] = useState<string | null>(null);
  const [provider, setProvider] = useState<CaptchaProvider>('altcha');
  const [siteKey, setSiteKey] = useState('');
  const [widgetKey, setWidgetKey] = useState(0);
  // 求解代次：reset / 重新领题 +1，旧的一轮跑完直接丢弃结果
  const generation = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 指向最新一轮的 start：定时器到点自动重领时用它，避免闭包引用过期函数
  const startRef = useRef<() => void>(() => {});

  const start = useCallback(async (): Promise<void> => {
    const gen = ++generation.current;
    setPayload(null);
    setState('verifying');
    let raw: (AltchaChallenge & { provider?: string; siteKey?: string }) | null = null;
    try {
      const res = await fetch(CHALLENGE_URL);
      if (res.status === 400) {
        // 开关关闭：接口明确回 400，页面隐藏校验块
        if (gen === generation.current) setState('disabled');
        return;
      }
      if (!res.ok) throw new Error(`领题失败（HTTP ${res.status}）`);
      raw = (await res.json()) as AltchaChallenge & { provider?: string; siteKey?: string };
    } catch {
      if (gen === generation.current) setState('error');
      return;
    }
    if (gen !== generation.current) return;
    const next: CaptchaProvider = raw.provider === 'turnstile' ? 'turnstile' : 'altcha';
    setProvider(next);
    if (next === 'turnstile') {
      // token 由 Cloudflare 组件产出；到期由组件自行刷新并再次回调
      setSiteKey(String(raw.siteKey ?? ''));
      return;
    }
    try {
      const number = await solveChallenge(raw, {
        isStale: () => gen !== generation.current,
      });
      if (gen !== generation.current || number < 0) return;
      setPayload(buildPayload(raw, number));
      setState('verified');
      // 答案有寿命（服务端 10 分钟）：到点自动重领，省得用户停在过期页面上
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => startRef.current(), REFRESH_MS);
    } catch {
      if (gen === generation.current) setState('error');
    }
  }, []);

  const reset = useCallback((): void => {
    setWidgetKey((key) => key + 1);
    void start();
  }, [start]);
  startRef.current = reset;

  const setTurnstileToken = useCallback((token: string): void => {
    if (!token) {
      setPayload(null);
      setState('verifying');
      return;
    }
    setPayload(token);
    setState('verified');
  }, []);

  const markError = useCallback((): void => {
    setPayload(null);
    setState('error');
  }, []);

  useEffect(() => {
    void start();
    return () => {
      generation.current += 1;
      if (timer.current) clearTimeout(timer.current);
    };
  }, [start]);

  return {
    state,
    payload,
    reset,
    provider,
    siteKey,
    setTurnstileToken,
    markError,
    widgetKey,
  };
}
