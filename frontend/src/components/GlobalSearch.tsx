import { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Search, CornerDownLeft, Satellite, Users, KeyRound, FileText, Boxes, X, SlidersHorizontal } from 'lucide-react';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import { matchConfigEntries } from '../lib/config-index';

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
  const [users, setUsers] = useState<Array<{ id?: string | number; email?: string; username?: string; role?: string }>>([]);
  const [tokens, setTokens] = useState<Array<{ id?: string; name?: string; key?: string; group?: string }>>([]);
  const [models, setModels] = useState<Array<{ id?: string; owned_by?: string }>>([]);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // 打开前记住触发焦点元素，关闭时归还
  const triggerRef = useRef<Element | null>(null);

  // 打开时清空 + 聚焦 + 按角色加载实体数据 + 锁 body 滚动
  useEffect(() => {
    if (!open) return undefined;
    triggerRef.current = document.activeElement;
    setQuery('');
    setActive(0);
    document.body.style.overflow = 'hidden';
    const id = window.setTimeout(() => inputRef.current?.focus(), 0);
    if (isAdmin()) {
      api.listChannels().then((res) => setChannels(Array.isArray(res?.data) ? res.data : [])).catch(() => {});
      api.listUsers().then((res) => setUsers(Array.isArray(res?.data) ? res.data : [])).catch(() => {});
    }
    api.listTokens(isAdmin()).then((res) => setTokens(Array.isArray(res?.data) ? res.data : [])).catch(() => {});
    api.listModels().then((res) => setModels(Array.isArray(res?.data) ? res.data : [])).catch(() => {});
    return () => {
      window.clearTimeout(id);
      document.body.style.overflow = '';
      (triggerRef.current as HTMLElement | null)?.focus?.();
      triggerRef.current = null;
    };
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

  // 配置项命中（管理员）：查询词不落在任何**页名**上时，仍要能找到折在
  // 二级/三级 Tab 里的配置。典型场景：用户搜「易支付」「github」「linuxdo」——
  // 这三个词在侧栏一级菜单里根本不存在（那一层只有「套餐与兑换码」），
  // 在改之前只能搜出「无匹配结果」，于是结论是「这个配置线上没有」。
  // 空查询不铺配置项：Ctrl+K 刚打开时列全部导航页已经够看，再叠十几条配置会淹掉
  // 用户真正要的那一行。只有开始输入词才把配置面接进来。
  const configHits = useMemo(() => {
    if (!isAdmin() || !q) return [];
    return matchConfigEntries(q, t, true);
  }, [q, t]);

  // 实体命中（管理员：渠道/用户/令牌；普通用户：仅本人令牌；模型任何登录用户可见）
  const hits = useMemo(() => {
    if (!q) return { channel: [], user: [], token: [], model: [] };
    const match = (fields: Array<string | undefined>): boolean => fields.filter(Boolean).join(' ').toLowerCase().includes(q);
    const channel = isAdmin() ? channels.filter((c) => match([c.name, c.base_url])) : [];
    const user = isAdmin() ? users.filter((u) => match([u.email, u.username, u.role])) : [];
    const token = tokens.filter((k) => match([k.name, k.key, k.group]));
    const model = models.filter((m) => match([m.id, m.owned_by]));
    return { channel, user, token, model };
  }, [q, channels, users, tokens, models]);

  type EntityKind = 'channel' | 'user' | 'token' | 'model';
  type Row =
    | { kind: 'page'; path: string; title: string; sub: string }
    | { kind: 'config'; path: string; title: string; sub: string }
    | { kind: 'entity'; entity: EntityKind; path: string; title: string; sub: string };
  const rows: Row[] = useMemo(() => {
    const out: Row[] = [];
    for (const p of pageHits) out.push({ kind: 'page', path: p.path, title: t(p.labelKey), sub: t('页面') });
    for (const c of configHits) out.push({ kind: 'config', path: c.path, title: t(c.label), sub: t(c.hint) });
    for (const c of hits.channel.slice(0, 8)) out.push({ kind: 'entity', entity: 'channel', path: '/channels', title: c.name || '—', sub: c.base_url || '供应商' });
    for (const u of hits.user.slice(0, 8)) out.push({ kind: 'entity', entity: 'user', path: '/users', title: u.email || u.username || '—', sub: u.role || '用户' });
    for (const m of hits.model.slice(0, 8)) out.push({ kind: 'entity', entity: 'model', path: '/model-manage', title: m.id || '—', sub: m.owned_by || '模型' });
    for (const k of hits.token.slice(0, 8)) out.push({ kind: 'entity', entity: 'token', path: '/keys', title: k.name || '—', sub: k.group || '令牌' });
    return out;
  }, [pageHits, configHits, hits, t]);

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
    // 分组标题挂在每组**第一行**上。空查询时 pageHits 是全部页面、其后没有任何
    // 实体行；此时若仍按「页面有命中」判第一条，配置组标题就再也不会出现。
    // 判据统一成「该组非空且 i 正好落在该组首行」，与各组长度无关地成立。
    const pStart = 0;
    const pEnd = pStart + pageHits.length;
    const cfgStart = pEnd;
    const cfgEnd = cfgStart + configHits.length;
    const cStart = cfgEnd;
    const cEnd = cStart + hits.channel.length;
    const uEnd = cEnd + hits.user.length;
    const mEnd = uEnd + hits.model.length;
    if (i === pStart && pageHits.length > 0) return t('页面');
    if (i === cfgStart && configHits.length > 0) return t('配置项');
    if (i === cStart && hits.channel.length > 0) return t('供应商');
    if (i === cEnd && hits.user.length > 0) return t('用户');
    if (i === uEnd && hits.model.length > 0) return t('模型');
    if (i === mEnd && hits.token.length > 0) return t('令牌');
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
            placeholder={t('搜索页面、配置项、供应商、模型、用户、令牌…')}
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
                  ) : row.kind === 'config' ? (
                    <SlidersHorizontal size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  ) : row.entity === 'channel' ? (
                    <Satellite size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  ) : row.entity === 'user' ? (
                    <Users size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
                  ) : row.entity === 'model' ? (
                    <Boxes size={14} style={{ color: 'var(--text-muted)', flexShrink: 0 }} />
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
