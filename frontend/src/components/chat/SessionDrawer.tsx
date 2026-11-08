/**
 * 通用会话抽屉——chat 页与 AI 运维页共用（此前两页各写了一份 + 两套 CSS）。
 *
 * 变体：`sidebar`（桌面常驻侧栏，可收起）/ `overlay`（覆盖层，移动端友好）。
 * 列表项由调用方给出 meta 文案；删除二次确认内建（chat 页此前裸删）。
 */
import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { ChevronLeft, GitFork, MessageSquarePlus, Pencil, Trash2 } from 'lucide-react';
import ConfirmDialog, { type ConfirmState } from '../ConfirmDialog';
import './SessionDrawer.css';

/** 与 SessionDrawer.css 的降级断点一致：窄屏时 sidebar 降级为覆盖层 */
const NARROW_MQ = '(max-width: 900px)';

/** 窄屏判定（监听断点变化，窗口拖宽/旋转即时切换挂载方式） */
function useIsNarrow(): boolean {
  const [narrow, setNarrow] = useState<boolean>(
    () => typeof window !== 'undefined' && Boolean(window.matchMedia?.(NARROW_MQ).matches),
  );
  useEffect(() => {
    const mq = window.matchMedia(NARROW_MQ);
    const onChange = (): void => setNarrow(mq.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return narrow;
}

export interface SessionDrawerItem {
  id: string;
  title: string;
  /** 列表项元信息（角色/日期/消息数等，调用方拼好） */
  meta?: string;
  /** #58：父会话 id（fork 产物；父被删时前端显示「父已删」容忍徽标） */
  parent_session_id?: string | null;
  /** #58：命名来源三态 "user"/"auto"/缺省 → 徽标「手工/自动」 */
  title_source?: string | null;
  /** #52：会话是否有消息——false 时 fork 入口禁用（tooltip「空会话无可分叉消息」） */
  forkable?: boolean;
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
  /** #58：行级 fork 入口（可选——AI 运维页不传，行为不变）。
   *  语义由调用方实现：活动会话从最新消息分叉；非活动会话就地复制。 */
  onFork?: (id: string) => void;
  /** #52：行级重命名入口（可选）。调用方负责 PATCH 后端并回写列表；
   *  提供后在列表项渲染「编辑」按钮 + 行内编辑态（Enter 提交 / Esc 取消 /
   *  空标题校验拒绝提交）。rename 语义上把 title_source 置 "user"
   *  （后端约定：user 档永不被自动标题覆盖）。 */
  onRename?: (id: string, title: string) => void;
  /** 标题（默认「会话列表」） */
  title?: string;
  /** 头部额外操作（渲染在标题与收起按钮之间，如聊天页的标题总结设置）；
   *  可选——AI 运维页不传，行为不变 */
  headerExtra?: React.ReactNode;
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
  onFork,
  onRename,
  title,
  headerExtra,
}: SessionDrawerProps): JSX.Element | null {
  const { t } = useTranslation();
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  // #52：行内重命名编辑态（编辑中的会话 id + 草稿；Enter 提交/Esc 取消）
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState('');
  const [renameError, setRenameError] = useState('');
  // hook 必须在提前 return 之前调用（open 切换时 hooks 数量不能变）
  const isNarrow = useIsNarrow();

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
        {headerExtra}
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
        {sessions.map((s) => {
          // #58：父悬空容忍——parent id 不在当前列表（被删）→「父已删」徽标
          const parentGone = Boolean(s.parent_session_id) && !sessions.some((x) => x.id === s.parent_session_id);
          // #52：行内重命名态（该条目进入编辑模式）
          if (onRename && renameId === s.id) {
            return (
              <div key={s.id} className={`sd-item editing ${s.id === activeId ? 'active' : ''}`}>
                <input
                  type="text"
                  className="sd-rename-input form-input"
                  aria-label={t('重命名会话')}
                  value={renameDraft}
                  autoFocus
                  onChange={(e) => { setRenameDraft(e.target.value); setRenameError(''); }}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') {
                      e.preventDefault();
                      const next = renameDraft.trim();
                      if (!next) { setRenameError(t('标题不能为空')); return; }
                      onRename(s.id, next);
                      setRenameId(null);
                      setRenameError('');
                    }
                    if (e.key === 'Escape') {
                      e.preventDefault();
                      setRenameId(null);
                      setRenameError('');
                    }
                  }}
                  onBlur={() => { setRenameId(null); setRenameError(''); }}
                />
                {renameError && <span className="sd-rename-error">{renameError}</span>}
              </div>
            );
          }
          return (
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
              <span className="sd-item-title">
                {s.title || t('新会话')}
                {parentGone && <span className="sd-badge sd-badge-gone">{t('父已删')}</span>}
                {s.title_source === 'user' && <span className="sd-badge">{t('手工')}</span>}
                {s.title_source === 'auto' && <span className="sd-badge">{t('自动')}</span>}
              </span>
              {s.meta && <span className="sd-item-meta">{s.meta}</span>}
            </button>
            {onFork && (
              <button
                type="button"
                className="sd-item-del"
                title={s.forkable === false ? t('空会话无可分叉消息') : t('分叉为新会话')}
                aria-label={t('分叉为新会话')}
                disabled={s.forkable === false}
                onClick={() => { if (s.forkable !== false) onFork(s.id); }}
              >
                <GitFork size={12} />
              </button>
            )}
            {onRename && (
              <button
                type="button"
                className="sd-item-del"
                title={t('重命名')}
                aria-label={t('重命名')}
                onClick={() => {
                  setRenameId(s.id);
                  setRenameDraft(s.title || '');
                  setRenameError('');
                }}
              >
                <Pencil size={12} />
              </button>
            )}
            <button
              type="button"
              className="sd-item-del"
              title={t('删除')}
              onClick={() => askDelete(s)}
            >
              <Trash2 size={12} />
            </button>
          </div>
          );
        })}
      </div>
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </>
  );

  const root = (
    <div className={`sd-root ${variant}${open ? ' open' : ''}`}>
      <aside className="sd-panel">{body}</aside>
      {variant === 'overlay' && (
        <div className="sd-mask" onClick={onClose} aria-hidden />
      )}
    </div>
  );
  // overlay 必须 portal 到 body：内联会被父级的 transform/z-index 层叠上下文
  // 困住（移动端汉堡按钮盖住抽屉标题的回归 bug 就是这么来的）。
  // sidebar 变体同理：桌面端内联（占位推开内容），窄屏降级为覆盖层时同样
  // 要 portal——否则内联在 .main-content（z-index:10）里，fixed z-160 只在
  // 该层叠上下文内部生效，汉堡按钮（根上下文 z-150）照样盖住抽屉。
  const usePortal = variant === 'overlay' || isNarrow;
  return usePortal ? createPortal(root, document.body) : root;
}
