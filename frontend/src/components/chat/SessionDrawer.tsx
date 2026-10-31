/**
 * 通用会话抽屉——chat 页与 AI 运维页共用（此前两页各写了一份 + 两套 CSS）。
 *
 * 变体：`sidebar`（桌面常驻侧栏，可收起）/ `overlay`（覆盖层，移动端友好）。
 * 列表项由调用方给出 meta 文案；删除二次确认内建（chat 页此前裸删）。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronLeft, MessageSquarePlus, Trash2 } from 'lucide-react';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import './SessionDrawer.css';

export interface SessionDrawerItem {
  id: string;
  title: string;
  /** 列表项元信息（角色/日期/消息数等，调用方拼好） */
  meta?: string;
}

export interface SessionDrawerProps {
  variant?: 'sidebar' | 'overlay';
  open: boolean;
  onClose: () => void;
  sessions: SessionDrawerItem[];
  activeId?: string | null;
  loading?: boolean;
  onSelect: (id: string) => void;
  onCreate: () => void;
  onDelete: (id: string) => void;
  /** 标题（默认「会话列表」） */
  title?: string;
}

export default function SessionDrawer({
  variant = 'sidebar',
  open,
  onClose,
  sessions,
  activeId,
  loading,
  onSelect,
  onCreate,
  onDelete,
  title,
}: SessionDrawerProps): JSX.Element | null {
  const { t } = useTranslation();
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  if (!open) return null;

  const askDelete = (item: SessionDrawerItem) => {
    setConfirmState({
      title: t('删除会话'),
      message: t('删除后不可恢复，确认删除') + `「${item.title || t('新会话')}」？`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        onDelete(item.id);
      },
    });
  };

  const body = (
    <>
      <div className="sd-head">
        <span>{title ?? t('会话列表')}</span>
        <button type="button" className="sd-close" onClick={onClose} aria-label={t('收起')}>
          <ChevronLeft size={14} />
        </button>
      </div>
      <button
        type="button"
        className="sd-new"
        onClick={() => {
          onCreate();
          onClose();
        }}
      >
        <MessageSquarePlus size={14} />
        {t('新会话')}
      </button>
      <div className="sd-list">
        {loading && <div className="sd-empty">{t('加载中…')}</div>}
        {!loading && sessions.length === 0 && (
          <div className="sd-empty">{t('暂无会话')}</div>
        )}
        {sessions.map((s) => (
          <div key={s.id} className={`sd-item ${s.id === activeId ? 'active' : ''}`}>
            <button
              type="button"
              className="sd-item-btn"
              title={s.title}
              onClick={() => {
                onSelect(s.id);
                if (variant === 'overlay') onClose();
              }}
            >
              <span className="sd-item-title">{s.title || t('新会话')}</span>
              {s.meta && <span className="sd-item-meta">{s.meta}</span>}
            </button>
            <button
              type="button"
              className="sd-item-del"
              title={t('删除')}
              onClick={() => askDelete(s)}
            >
              <Trash2 size={12} />
            </button>
          </div>
        ))}
      </div>
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </>
  );

  return (
    <div className={`sd-root ${variant}${open ? ' open' : ''}`}>
      <aside className="sd-panel">{body}</aside>
      {variant === 'overlay' && (
        <div className="sd-mask" onClick={onClose} aria-hidden />
      )}
    </div>
  );
}
