import { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Search, CornerDownLeft, Satellite, Users, KeyRound, FileText, X } from 'lucide-react';
import { api } from '../api';
import { isAdmin } from '../lib/utils';

export interface SearchNavItem {
  path: string;
  labelKey: string;
  adminOnly?: boolean;
}

interface GlobalSearchProps {
  open: boolean;
  onClose: () => void;
  navItems: SearchNavItem[];
}

/**
 * GlobalSearch — Ctrl+K 命令面板式全局搜索。
 * - 页面导航：本地即时匹配菜单项（任何角色）。
 * - 实体搜索：管理员搜渠道/用户/令牌；普通用户仅搜本人令牌。
 * - 点击结果跳转对应管理页；Esc 关闭；↑↓ 键盘选择。
 */
export default function GlobalSearch({ open, onClose, navItems }: GlobalSearchProps): JSX.Element | null {
  const navigate = useNavigate();
  const { t } = useTranslation();
  const [query, setQuery] = useState('');
  const [channels, setChannels] = useState<Array<{ id?: string; name?: string; base_url?: string }>>([]);
  const [users, setUsers] = useState<Array<{ id?: string; email?: string; username?: string; role?: string }>>([]);
  const [tokens, setTokens] = useState<Array<{ id?: string; name?: string; key?: string; group?: string }>>([]);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // 打开时清空 + 聚焦 + 按角色加载实体数据
  useEffect(() => {
    if (!open) return;
    setQuery('');
    setActive(0);
    const id = window.setTimeout(() => inputRef.current?.focus(), 0);
    if (isAdmin()) {
      api.listChannels().then((res) => setChannels(Array.isArray(res?.data) ? res.data : res || [])).catch(() => {});
      api.listUsers().then((res) => setUsers(Array.isArray(res?.data) ? res.data : res || [])).catch(() => {});
    }
    api.listTokens().then((res) => setTokens(Array.isArray(res?.data) ? res.data : res || [])).catch(() => {});
    return () => window.clearTimeout(id);
  }, [open]);

  // Ctrl/Cmd+K 打开（由父级 Sidebar 转发 onClose，这里只负责唤起）
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  const q = query.trim().toLowerCase();

  // 页面导航命中（本地，任何角色）
  const pageHits = useMemo(() => {
    const items = isAdmin() ? navItems : navItems.filter((n) => !n.adminOnly);
    return q ? items.filter((n) => t(n.labelKey).toLowerCase().includes(q)) : items;
  }, [q, navItems, t]);

  // 实体命中（管理员：渠道/用户/令牌；普通用户：仅本人令牌）
  const hits = useMemo(() => {
    if (!q) return { channel: [], user: [], token: [] };
    const match = (fields: Array<string | undefined>): boolean => fields.filter(Boolean).join(' ').toLowerCase().includes(q);
    const channel = isAdmin() ? channels.filter((c) => match([c.name, c.base_url])) : [];
    const user = isAdmin() ? users.filter((u) => match([u.email, u.username, u.role])) : [];
    const token = tokens.filter((k) => match([k.name, k.key, k.group]));
    return { channel, user, token };
  }, [q, channels, users, tokens]);

  type Row = { kind: 'page'; path: string; title: string; sub: string } | { kind: 'entity'; path: string; title: string; sub: string };
  const rows: Row[] = useMemo(() => {
    const out: Row[] = [];
    for (const p of pageHits) out.push({ kind: 'page', path: p.path, title: t(p.labelKey), sub: t('页面') });
    for (const c of hits.channel.slice(0, 8)) out.push({ kind: 'entity', path: '/channels', title: c.name || '—', sub: c.base_url || '渠道' });
    for (const u of hits.user.slice(0, 8)) out.push({ kind: 'entity', path: '/users', title: u.email || u.username || '—', sub: u.role || '用户' });
    for (const k of hits.token.slice(0, 8)) out.push({ kind: 'entity', path: '/keys', title: k.name || '—', sub: k.group || '令牌' });
    return out;
  }, [pageHits, hits, t]);

  useEffect(() => { setActive(0); }, [q]);

  const go = (row: Row | undefined): void => {
    if (!row) return;
    onClose();
    navigate(row.path);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>): void => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive((a) => Math.min(a + 1, rows.length - 1)); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
    else if (e.key === 'Enter') { e.preventDefault(); go(rows[active]); }
  };

  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${active}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }, [active]);

  if (!open) return null;

  const groupLabel = (i: number): string | null => {
    if (i === 0 && pageHits.length > 0) return t('页面');
    const pEnd = pageHits.length;
    const cStart = pEnd;
    const cEnd = cStart + hits.channel.length;
    const uEnd = cEnd + hits.user.length;
    if (i === cStart && hits.channel.length > 0) return t('渠道');
    if (i === cEnd && hits.user.length > 0) return t('用户');
    if (i === uEnd && hits.token.length > 0) return t('令牌');
    return null;
  };

  return (
    <div className="global-search-overlay" onClick={onClose} role="dialog" aria-modal="true" aria-label={t('全局搜索')}>
      <div className="global-search-panel" onClick={(e) => e.stopPropagation()}>
        <div className="global-search-input-row">
          <Search size={16} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder={t('搜索页面、渠道、用户、令牌…')}
            aria-label={t('全局搜索')}
          />
          <button type="button" className="global-search-close" onClick={onClose} aria-label={t('关闭')}>
            <X size={14} />
          </button>
        </div>
        <div className="global-search-results" ref={listRef}>
          {rows.length === 0 ? (
            <div className="global-search-empty">{t('无匹配结果')}</div>
          ) : (
            rows.map((row, i) => (
              <div key={`${row.kind}-${row.title}-${i}`}>
                {groupLabel(i) && <div className="global-search-group">{groupLabel(i)}</div>}
                <button
                  type="button"
                  className={`global-search-item ${i === active ? 'active' : ''}`}
                  data-idx={i}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => go(row)}
                >
                  {row.kind === 'page' ? (
                    <FileText size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  ) : row.sub === '渠道' ? (
                    <Satellite size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  ) : row.sub === '用户' ? (
                    <Users size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  ) : (
                    <KeyRound size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  )}
                  <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{row.title}</span>
                  <span className="sub">{row.sub}</span>
                  {i === active && <CornerDownLeft size={12} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />}
                </button>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
