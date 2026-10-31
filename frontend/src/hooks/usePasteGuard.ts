/**
 * 大段粘贴守卫：粘贴大文本后的 150ms 内禁用 Enter 发送。
 *
 * 粘贴带换行的大段内容时，用户常在松开 Ctrl 之前就松开 Enter——
 * 如果不拦截，整段内容会被当"回车发送"提前发出去（chat 页与运维页
 * 都踩过这个坑，此前三处各写了一份）。
 */
import { useCallback, useEffect, useRef } from 'react';

export interface PasteGuard {
  /** textarea 的 onPaste 处理器 */
  onPaste: (e: React.ClipboardEvent) => void;
  /** textarea 的 onKeyDown 里先问这个：true = 当前按键应忽略 */
  shouldSuppressEnter: () => boolean;
}

export function usePasteGuard(): PasteGuard {
  const ref = useRef(false);

  const onPaste = useCallback((e: React.ClipboardEvent) => {
    const text = e.clipboardData?.getData('text') ?? '';
    // 含换行或大段文本视为"整段内容"，给 150ms 让用户的 Ctrl/Enter 松开
    if (text.includes('\n') || text.length > 200) {
      ref.current = true;
      window.setTimeout(() => {
        ref.current = false;
      }, 150);
    }
  }, []);

  const shouldSuppressEnter = useCallback(() => ref.current, []);

  // 卸载清理（timer 已由 setTimeout 自然过期，这里只复位标记）
  useEffect(() => () => {
    ref.current = false;
  }, []);

  return { onPaste, shouldSuppressEnter };
}
