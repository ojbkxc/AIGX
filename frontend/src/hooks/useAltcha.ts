import { useCallback, useEffect, useRef, useState } from 'react';
import { buildPayload, solveChallenge, type AltchaChallenge } from '../lib/altcha';

/** 领题端点：与服务端路由、后端 `handle_captcha` 三处同名。 */
const CHALLENGE_URL = '/api/auth/captcha';
/** 题目寿命：服务端 10 分钟过期，提前 30 秒重领，避免踩线提交拿到「已过期」。 */
const REFRESH_MS = 9.5 * 60 * 1000;

export type AltchaState = 'disabled' | 'verifying' | 'verified' | 'error';

/**
 * 机器人校验（ALTCHA proof-of-work）——登录页 / 注册页共用。
 *
 * 挂载即领题求解（与官方 widget 同观感：进来就是「验证中…」，真人不需操作）。
 * 开关关闭时领题端点回 400，本 hook 落到 `disabled`，页面据此隐藏校验块、
 * 提交时不带 captcha 字段（后端同样放行）。
 */
export function useAltcha(): {
  state: AltchaState;
  payload: string | null;
  reset: () => void;
} {
  const [state, setState] = useState<AltchaState>('verifying');
  const [payload, setPayload] = useState<string | null>(null);
  // 求解代次：reset / 重新领题 +1，旧的一轮跑完直接丢弃结果
  const generation = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // 指向最新一轮的 start：定时器到点自动重领时用它，避免闭包引用过期函数
  const startRef = useRef<() => void>(() => {});

  const start = useCallback(async (): Promise<void> => {
    const gen = ++generation.current;
    setPayload(null);
    setState('verifying');
    let challenge: AltchaChallenge;
    try {
      const res = await fetch(CHALLENGE_URL);
      if (res.status === 400) {
        // 开关关闭：接口明确回 400，页面隐藏校验块
        if (gen === generation.current) setState('disabled');
        return;
      }
      if (!res.ok) throw new Error(`领题失败（HTTP ${res.status}）`);
      challenge = (await res.json()) as AltchaChallenge;
    } catch {
      if (gen === generation.current) setState('error');
      return;
    }
    try {
      const number = await solveChallenge(challenge, {
        isStale: () => gen !== generation.current,
      });
      if (gen !== generation.current || number < 0) return;
      setPayload(buildPayload(challenge, number));
      setState('verified');
      // 答案有寿命（服务端 10 分钟）：到点自动重领，省得用户停在过期页面上
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => startRef.current(), REFRESH_MS);
    } catch {
      if (gen === generation.current) setState('error');
    }
  }, []);

  const reset = useCallback((): void => {
    void start();
  }, [start]);
  startRef.current = reset;

  useEffect(() => {
    void start();
    return () => {
      generation.current += 1;
      if (timer.current) clearTimeout(timer.current);
    };
  }, [start]);

  return { state, payload, reset };
}