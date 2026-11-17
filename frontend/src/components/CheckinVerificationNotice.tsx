// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 签到中心「验证提示卡」—— 对齐 A2A ui-islands checkin-verification-notice.tsx。
//
// A2A 里这张卡是常驻右下角的浮动状态卡：ZCode 类账号领取套餐时，网关会
// 触发一次人机验证（阿里云无痕 / 滑块），前端把「正在验证 / 需人工完成滑块 /
// 提交流程中」如实告诉管理员，并给一个「取消验证」。本文件把同一份信息结构
// 按 AIGX 自研组件重写 —— A2A 用 `wbZcodeClaimVerification` 全局桥接，
// AIGX 侧没有这条桥（网关托管，验证在管理页发起），所以状态来自
// `lib/aliyunCaptcha` 的进行中标记 + BenefitsPanel 的回调。

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, ShieldAlert } from 'lucide-react';
import { Button } from '../components/ui';
import { cancel as cancelAltiyunSolve, isBusy as captchaBusy } from '../lib/aliyunCaptcha';
import './CheckinVerificationNotice.css';

export interface VerificationNoticeState {
  /** 正在为哪个账号的哪项领取做验证 */
  accountName: string;
  planName: string;
  /** pending（等用户）/ submitting（凭据提交中） */
  phase: 'pending' | 'submitting';
}

/**
 * CheckinVerificationNotice — 验证状态卡（浮动，不抢焦点）。
 *
 * 只在「验证器正在跑」时渲染。轮询 `aliyunCaptcha.isBusy()` 拿到「验证器活动」
 * 这个粗粒度信号；更细的 phase 由调用方通过 `state` 传入（BenefitsPanel 在
 * 领取时自己知道是在等滑块还是已提交）。
 */
export default function CheckinVerificationNotice({
  state,
  onCancel,
}: {
  state: VerificationNoticeState | null;
  onCancel?: () => void;
}): JSX.Element | null {
  const { t } = useTranslation();
  const [, bump] = useState(0);

  // 轮询验证器活动信号（500ms）：取消 / 完成后自然收起
  useEffect(() => {
    if (!state) return undefined;
    const timer = window.setInterval(() => bump((n) => n + 1), 500);
    return () => window.clearInterval(timer);
  }, [state]);

  if (!state && !captchaBusy()) return null;
  if (!state) return null;

  const submitting = state.phase === 'submitting';
  return (
    <div className="cvf-card" role="status" aria-live="polite">
      <div className="cvf-title">
        {submitting ? <Loader2 size={14} className="cvf-spin" /> : <ShieldAlert size={14} />}
        {t('领取验证')}
      </div>
      <p className="cvf-target">{state.accountName} · {state.planName}</p>
      <p className="cvf-hint">
        {submitting
          ? t('验证凭据正在提交，领取结果请查看签到记录')
          : t('正在为本次领取验证；若出现滑块，请手动完成')}
      </p>
      <Button
        variant="outline"
        size="sm"
        disabled={submitting}
        onClick={() => {
          cancelAltiyunSolve();
          onCancel?.();
        }}
      >
        {t('取消验证')}
      </Button>
    </div>
  );
}