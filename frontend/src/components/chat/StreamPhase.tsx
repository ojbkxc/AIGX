/**
 * 流式阶段指示（思考中/生成中/停滞）——chat 页与运维页通用。
 *
 * 停滞检测：流式超过 stallSecs 没有任何事件就提示（此前只有 chat 页有，
 * 运维页缺停滞态）。
 */
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

export type StreamPhase = 'thinking' | 'generating' | 'stalled';

export interface StreamPhaseIndicatorProps {
  /** 当前是否在流式中 */
  active: boolean;
  /** 阶段（由调用方按后端事件推进：thinking/generating） */
  phase: 'thinking' | 'generating';
  /** 最近一次事件的时间戳（ms）；用于停滞检测 */
  lastEventAt: number;
  /** 多少秒无事件视为停滞，默认 20 */
  stallSecs?: number;
}

export default function StreamPhaseIndicator({
  active,
  phase,
  lastEventAt,
  stallSecs = 20,
}: StreamPhaseIndicatorProps): JSX.Element | null {
  const { t } = useTranslation();
  const [stalled, setStalled] = useState(false);
  const tickRef = useRef<number | null>(null);

  useEffect(() => {
    if (!active) {
      setStalled(false);
      return undefined;
    }
    const check = () => {
      setStalled(Date.now() - lastEventAt > stallSecs * 1000);
    };
    check();
    tickRef.current = window.setInterval(check, 2000);
    return () => {
      if (tickRef.current) window.clearInterval(tickRef.current);
    };
  }, [active, lastEventAt, stallSecs]);

  if (!active) return null;

  const label = stalled
    ? t('响应似乎停滞了（仍在等待上游）…')
    : phase === 'thinking'
      ? t('思考中…')
      : t('正在生成…');

  return (
    <div className={`stream-phase${stalled ? ' stalled' : ''}`}>
      <span className="stream-phase-dot" />
      {label}
    </div>
  );
}
