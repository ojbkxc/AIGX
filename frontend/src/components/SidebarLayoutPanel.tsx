// 侧边栏菜单编辑面板（系统设置 → 显示 → 侧边栏菜单）。
//
// 控制主侧边栏：逐项显隐 / 拖拽排序（可跨组）/ 分组重命名 / 新建与删除分组 /
// 整组隐藏。改动即时生效并落库（lib/sidebar-layout.ts 负责缓存 + 服务端同步），
// 左侧真实侧边栏就是结果。
//
// 拖拽为何用指针事件而非 HTML5 拖放：拖动过程中布局一变就要重绘，而 HTML5
// 拖放的会话绑在源节点上，一重绘就断；指针事件的状态只在 JS 变量里，节点换掉
// 不影响会话。视觉照旧：被拖项压淡、跨过哪一行就在哪一行落位、松手才写盘。
import { useCallback, useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type KeyboardEvent as ReactKeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useToast } from './Toast';
import { Switch } from './ui';
import { useSidebarLayout } from '../hooks/useSidebarLayout';
import {
  addGroup,
  groupHasFixed,
  isBuiltinGroupLabel,
  locate,
  menuOf,
  moveGroup,
  moveItem,
  removeGroup,
  renameGroup,
  setGroupHidden,
  setItemHidden,
  defaultLayout,
  visibleCount,
  type LayoutGroup,
  type SidebarLayout,
} from '../lib/sidebar-layout';
import { moveGroupAdjacent, moveItemAdjacent } from '../lib/sidebar-groups';
import './SidebarLayoutPanel.css';

const GRIP_ICON = (
  <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" aria-hidden="true">
    <circle cx="9" cy="6" r="1.4" />
    <circle cx="15" cy="6" r="1.4" />
    <circle cx="9" cy="12" r="1.4" />
    <circle cx="15" cy="12" r="1.4" />
    <circle cx="9" cy="18" r="1.4" />
    <circle cx="15" cy="18" r="1.4" />
  </svg>
);

export default function SidebarLayoutPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const { layout, apply } = useSidebarLayout();

  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState('');
  const [adding, setAdding] = useState(false);
  const [addDraft, setAddDraft] = useState('');
  const [dragId, setDragId] = useState<string | null>(null);

  const layoutRef = useRef<SidebarLayout>(layout);
  useEffect(() => {
    layoutRef.current = layout;
  }, [layout]);

  const dragRef = useRef<{ kind: 'group' | 'item'; id: string } | null>(null);
  const teardownRef = useRef<(() => void) | null>(null);
  useEffect(() => () => { teardownRef.current?.(); }, []);

  const groupTitle = (group: LayoutGroup): string =>
    isBuiltinGroupLabel(group.label) ? t(group.label) : group.label;

  const totalMenus = layout.groups.reduce((sum, g) => sum + g.items.length, 0);
  const hiddenMenus = layout.hidden.length;
  const hiddenGroups = layout.groups.filter((g) => g.hidden).length;

  /* ── 变更动作 ── */

  const toggleItem = (key: string, visible: boolean): void => {
    apply(setItemHidden(layout, key, !visible));
  };

  const toggleGroup = (group: LayoutGroup): void => {
    if (!group.hidden && groupHasFixed(group)) {
      addToast(t('「系统设置」是固定项，含它的分组不能整组隐藏；可以单独隐藏组内其它菜单'), 'error');
      return;
    }
    apply(setGroupHidden(layout, group.id, !group.hidden));
  };

  const commitRename = (gid: string): void => {
    const next = renameGroup(layout, gid, renameDraft);
    setRenaming(null);
    if (next) apply(next);
  };

  const commitAdd = (): void => {
    const next = addGroup(layout, addDraft);
    if (!next) {
      addToast(t('请填一个分组名'), 'error');
      return;
    }
    setAdding(false);
    setAddDraft('');
    apply(next);
    addToast(t('已新建分组「{{name}}」', { name: addDraft.trim() }));
  };

  const handleRemoveGroup = (group: LayoutGroup): void => {
    const first = layout.groups.find((g) => g.id !== group.id);
    const message = group.items.length
      ? t('删除分组「{{name}}」？组内 {{n}} 个菜单会移到「{{target}}」，不会丢。', {
          name: groupTitle(group), n: group.items.length, target: first ? groupTitle(first) : '',
        })
      : t('删除空分组「{{name}}」？', { name: groupTitle(group) });
    if (!window.confirm(message)) return;
    if (apply(removeGroup(layout, group.id))) {
      addToast(t('已删除分组「{{name}}」', { name: groupTitle(group) }));
    }
  };

  const resetAll = (): void => {
    if (!window.confirm(t('会恢复默认的分组、顺序与显隐设置。你自建的分组会被删除（组内菜单不会丢）。'))) return;
    apply(defaultLayout());
    addToast(t('已恢复默认侧边栏'));
  };

  const moveIntoGroup = (group: LayoutGroup, key: string): void => {
    if (!key) return;
    const gi = layout.groups.findIndex((g) => g.id === group.id);
    if (gi < 0) return;
    apply(moveItem(layout, key, gi, layout.groups[gi].items.length));
  };

  /* ── 拖拽 ── */

  const startDrag = useCallback((
    kind: 'group' | 'item',
    id: string,
    event: ReactPointerEvent<HTMLElement>,
  ): void => {
    if (event.button !== 0) return;
    event.preventDefault();
    teardownRef.current?.();
    dragRef.current = { kind, id };
    setDragId(id);
    document.body.classList.add('sl-dragging');
    const source = event.currentTarget.closest<HTMLElement>(
      kind === 'group' ? '[data-sl-group]' : '[data-sl-item]',
    );
    source?.classList.add('is-dragging');
    layoutRef.current = layout;

    const onMove = (moveEvent: PointerEvent): void => {
      const current = dragRef.current;
      if (!current) return;
      const el = document.elementFromPoint(moveEvent.clientX, moveEvent.clientY) as HTMLElement | null;
      if (!el) return;
      const live = layoutRef.current;

      if (current.kind === 'group') {
        const card = el.closest<HTMLElement>('[data-sl-group]');
        if (!card) return;
        const gid = card.dataset.slGroup || '';
        if (!gid || gid === current.id) return;
        const targetIndex = live.groups.findIndex((g) => g.id === gid);
        if (targetIndex < 0) return;
        const rect = card.getBoundingClientRect();
        const after = moveEvent.clientY > rect.top + rect.height / 2;
        apply(moveGroup(live, current.id, after ? targetIndex + 1 : targetIndex));
        return;
      }

      const row = el.closest<HTMLElement>('[data-sl-item]');
      if (row) {
        const targetKey = row.dataset.slItem || '';
        const gid = row.dataset.slGid || '';
        if (!targetKey || targetKey === current.id) return;
        const gi = live.groups.findIndex((g) => g.id === gid);
        if (gi < 0) return;
        const index = live.groups[gi].items.indexOf(targetKey);
        if (index < 0) return;
        const rect = row.getBoundingClientRect();
        const after = moveEvent.clientY > rect.top + rect.height / 2;
        apply(moveItem(live, current.id, gi, after ? index + 1 : index));
        return;
      }

      const card = el.closest<HTMLElement>('[data-sl-group]');
      if (!card) return;
      const gi = live.groups.findIndex((g) => g.id === (card.dataset.slGroup || ''));
      if (gi < 0) return;
      apply(moveItem(live, current.id, gi, live.groups[gi].items.length));
    };

    const teardown = (): void => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      document.body.classList.remove('sl-dragging');
      document.querySelectorAll('.is-dragging').forEach((node) => node.classList.remove('is-dragging'));
      dragRef.current = null;
      teardownRef.current = null;
      setDragId(null);
    };
    const onUp = (): void => { teardown(); };

    teardownRef.current = teardown;
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }, [apply, layout]);

  /** 键盘排序：把手聚焦后 Alt+↑/↓ 移动一格，跨过组边界时落到相邻组。 */
  const moveByKeyboard = (kind: 'group' | 'item', id: string, up: boolean): void => {
    if (kind === 'group') {
      apply(moveGroupAdjacent(layout, id, up ? -1 : 1));
      return;
    }
    const from = locate(layout, id);
    if (!from) return;
    // 组首上移 / 组尾下移：跨到相邻组的端点（这一跳必须自己算，纯组内交换表达不了）
    if (up && from.index === 0 && from.gi > 0) {
      apply(moveItem(layout, id, from.gi - 1, layout.groups[from.gi - 1].items.length));
      return;
    }
    if (!up && from.index === layout.groups[from.gi].items.length - 1
      && from.gi < layout.groups.length - 1) {
      apply(moveItem(layout, id, from.gi + 1, 0));
      return;
    }
    apply(moveItemAdjacent(layout, id, up ? -1 : 1));
  };

  const onGripKey = (kind: 'group' | 'item', id: string) => (event: ReactKeyboardEvent): void => {
    if (!event.altKey) return;
    if (event.key === 'ArrowUp') { event.preventDefault(); moveByKeyboard(kind, id, true); }
    if (event.key === 'ArrowDown') { event.preventDefault(); moveByKeyboard(kind, id, false); }
  };

  return (
    <div className="card sl-panel" style={{ marginTop: 16 }}>
      <div className="card-header">
        <h2>{t('侧边栏菜单')}</h2>
        <p className="card-subtitle">
          {t('控制主侧边栏显示哪些菜单、按什么顺序、怎么分组。隐藏只影响导航显示，不影响页面访问权限（被隐藏的页面仍可经地址直接打开）。改动即时生效并跨端同步。')}
        </p>
      </div>
      <div className="card-body">
        <div className="settings-state">
          {t('拖动 ⠿ 调整顺序（菜单可跨组拖）；拨开关控制显隐；点分组名可重命名；组头下拉可把别的菜单移进来；眼睛按钮隐藏整组。键盘也能排序：把手聚焦后按 Alt+↑ / ↓。')}
        </div>

        <div className="sl-groups">
          {layout.groups.map((group) => {
            const shown = visibleCount(group, layout.hidden);
            const locked = groupHasFixed(group);
            const isRenaming = renaming === group.id;
            const candidates = layout.groups
              .flatMap((g) => g.items)
              .filter((key) => !group.items.includes(key));
            return (
              <div
                key={group.id}
                data-sl-group={group.id}
                className={`sl-group${group.hidden ? ' is-hidden' : ''}${dragId === group.id ? ' is-dragging' : ''}`}
              >
                <div className="sl-group-head">
                  <span
                    className="sl-grip"
                    role="button"
                    tabIndex={0}
                    title={t('拖动调整分组顺序（Alt+↑ / ↓ 也可）')}
                    aria-label={t('拖动分组 {{name}}', { name: groupTitle(group) })}
                    onPointerDown={(e) => startDrag('group', group.id, e)}
                    onKeyDown={onGripKey('group', group.id)}
                  >
                    {GRIP_ICON}
                  </span>

                  {isRenaming ? (
                    <input
                      className="form-input sl-rename"
                      value={renameDraft}
                      maxLength={12}
                      autoFocus
                      aria-label={t('分组名')}
                      onChange={(e) => setRenameDraft(e.target.value)}
                      onBlur={() => commitRename(group.id)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') { e.preventDefault(); commitRename(group.id); }
                        if (e.key === 'Escape') { e.preventDefault(); setRenaming(null); }
                      }}
                    />
                  ) : (
                    <button
                      type="button"
                      className="sl-group-title"
                      title={t('点击重命名')}
                      onClick={() => { setRenaming(group.id); setRenameDraft(group.label); }}
                    >
                      {groupTitle(group)}
                    </button>
                  )}

                  <span className="sl-group-meta">{t('{{n}} 项 · {{shown}} 显示', { n: group.items.length, shown })}</span>

                  <div className="sl-group-actions">
                    {candidates.length > 0 && (
                      <select
                        className="form-input sl-move-select"
                        value=""
                        title={t('把菜单移入「{{name}}」', { name: groupTitle(group) })}
                        aria-label={t('把菜单移入「{{name}}」', { name: groupTitle(group) })}
                        onChange={(e) => moveIntoGroup(group, e.target.value)}
                      >
                        <option value="">{t('移入菜单…')}</option>
                        {candidates.map((key) => {
                          const menu = menuOf(key);
                          return menu ? <option key={key} value={key}>{t(menu.labelKey)}</option> : null;
                        })}
                      </select>
                    )}
                    <button
                      type="button"
                      className="btn btn-outline btn-sm"
                      title={locked
                        ? t('含固定项「系统设置」，不可整组隐藏（可以单独隐藏组内其它菜单）')
                        : (group.hidden ? t('显示这一组') : t('隐藏这一组'))}
                      aria-pressed={!group.hidden}
                      onClick={() => toggleGroup(group)}
                    >
                      {group.hidden ? t('显示') : t('隐藏')}
                    </button>
                    <button
                      type="button"
                      className="btn btn-outline btn-sm"
                      disabled={layout.groups.length <= 1}
                      title={layout.groups.length <= 1 ? t('至少要保留一个分组') : t('删除这个分组（组内菜单移到第一组）')}
                      onClick={() => handleRemoveGroup(group)}
                    >
                      {t('删除')}
                    </button>
                  </div>
                </div>

                <div className="sl-items">
                  {group.items.length === 0 ? (
                    <div className="sl-empty">{t('空分组 · 用组头下拉把菜单移进来')}</div>
                  ) : group.items.map((key) => {
                    const menu = menuOf(key);
                    if (!menu) return null;
                    const isHidden = layout.hidden.includes(key);
                    return (
                      <div
                        key={key}
                        data-sl-item={key}
                        data-sl-gid={group.id}
                        className={`sl-item${isHidden ? ' is-hidden' : ''}${dragId === key ? ' is-dragging' : ''}`}
                      >
                        <span
                          className="sl-grip"
                          role="button"
                          tabIndex={0}
                          title={t('拖动调整顺序，可跨组（Alt+↑ / ↓ 也可）')}
                          aria-label={t('拖动菜单 {{name}}', { name: t(menu.labelKey) })}
                          onPointerDown={(e) => startDrag('item', key, e)}
                          onKeyDown={onGripKey('item', key)}
                        >
                          {GRIP_ICON}
                        </span>
                        <span className="sl-item-icon"><menu.icon size={14} strokeWidth={1.8} /></span>
                        <span className="sl-item-label" title={t(menu.labelKey)}>{t(menu.labelKey)}</span>
                        {menu.fixed ? (
                          <span className="sl-fixed" title={t('固定项不可隐藏')}>{t('固定')}</span>
                        ) : (
                          <Switch
                            checked={!isHidden}
                            onChange={(next) => toggleItem(key, next)}
                            ariaLabel={isHidden ? t('显示 {{name}}', { name: t(menu.labelKey) }) : t('隐藏 {{name}}', { name: t(menu.labelKey) })}
                          />
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>
            );
          })}
        </div>

        <div className="sl-add">
          {adding ? (
            <div className="sl-add-form">
              <input
                className="form-input"
                value={addDraft}
                maxLength={12}
                autoFocus
                placeholder={t('分组名，例如「常用」')}
                aria-label={t('新分组名')}
                onChange={(e) => setAddDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') { e.preventDefault(); commitAdd(); }
                  if (e.key === 'Escape') { e.preventDefault(); setAdding(false); setAddDraft(''); }
                }}
              />
              <button type="button" className="btn btn-primary btn-sm" onClick={commitAdd}>{t('添加')}</button>
              <button type="button" className="btn btn-outline btn-sm" onClick={() => { setAdding(false); setAddDraft(''); }}>{t('取消')}</button>
            </div>
          ) : (
            <button type="button" className="btn btn-outline" onClick={() => setAdding(true)}>{t('新建分组')}</button>
          )}
          <button type="button" className="btn btn-outline" onClick={resetAll}>{t('恢复默认')}</button>
        </div>

        <div className="settings-state">
          {t('共 {{total}} 个菜单 · 隐藏 {{hidden}} · {{groups}} 个分组', {
            total: totalMenus, hidden: hiddenMenus, groups: layout.groups.length,
          })}
          {hiddenGroups ? t('（{{n}} 个整组隐藏）', { n: hiddenGroups }) : ''}
        </div>
      </div>
    </div>
  );
}
