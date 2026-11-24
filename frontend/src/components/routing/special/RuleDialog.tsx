/**
 * RuleDialog —— 特殊请求路由「编辑规则」弹窗。
 *
 * 对齐 A2A `routing-special-panels.tsx` 的 `CompactRuleDialog` / `VisionRuleDialog` /
 * `TitleRuleDialog`：点规则行 → 弹窗填字段 → 底栏「删除规则 / 取消 / 保存规则」。
 * 上游那一版把编辑放在弹窗里而不是行内展开，理由是「行内展开会把后面几行挤走，
 * 而一次只改一条」；AIGX 之前是行内展开，本波按 1:1 口径改为弹窗。
 *
 * AIGX 没有 shadcn 的 Dialog 组件，这里复用 App.css 既有的 `.modal-*` 类
 * （与 ConfirmDialog 同一套遮罩/底栏样式），不引入第二套弹窗语言。
 */
import { useEffect, useRef, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { Button } from '../../ui';

export default function RuleDialog({
  title,
  subtitle,
  saving,
  onClose,
  onApply,
  applyLabel,
  onDelete,
  deleteLabel,
  children,
}: {
  title: string;
  /** 弹窗顶部那句解释（A2A 每个弹窗都有一句「这条规则干什么」） */
  subtitle?: string;
  saving?: boolean;
  onClose: () => void;
  onApply: () => void;
  applyLabel?: string;
  onDelete?: () => void;
  deleteLabel?: string;
  children: ReactNode;
}): JSX.Element {
  const { t } = useTranslation();
  const boxRef = useRef<HTMLDivElement | null>(null);

  // Escape 关闭（保存中不关，避免把进行中的写入流程打断）；关闭后把焦点还给触发元素
  useEffect(() => {
    const restore = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape' && !saving) {
        onClose();
        return;
      }
      if (e.key === 'Tab' && boxRef.current) {
        const focusable = boxRef.current.querySelectorAll<HTMLElement>(
          'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
        );
        if (!focusable.length) return;
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        } else if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        }
      }
    };
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('keydown', onKey);
      restore?.focus?.();
    };
  }, [onClose, saving]);

  return (
    <div
      className="modal-overlay"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !saving) onClose();
      }}
    >
      <div
        className="modal-box"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        ref={boxRef}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <h3>{title}</h3>
          <button
            type="button"
            className="modal-close"
            aria-label={t('关闭')}
            disabled={saving}
            onClick={() => { if (!saving) onClose(); }}
          >
            ×
          </button>
        </div>
        <div className="modal-body">
          {subtitle ? <p className="form-hint" style={{ marginTop: 0 }}>{subtitle}</p> : null}
          {children}
        </div>
        <div className="modal-footer">
          {onDelete ? (
            <Button
              variant="outline"
              size="sm"
              disabled={saving}
              style={{ marginRight: 'auto' }}
              onClick={() => { onDelete(); onClose(); }}
            >
              {deleteLabel ?? t('删除规则')}
            </Button>
          ) : null}
          <Button variant="outline" size="sm" disabled={saving} onClick={onClose}>
            {t('取消')}
          </Button>
          <Button variant="primary" size="sm" disabled={saving} onClick={onApply}>
            {saving ? <Loader2 size={13} className="spin" /> : null}
            {applyLabel ?? t('保存规则')}
          </Button>
        </div>
      </div>
    </div>
  );
}
