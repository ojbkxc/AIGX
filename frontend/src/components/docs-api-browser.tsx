/**
 * DocsApiBrowser — AIGX「完整接入文档」浏览器（正文见 lib/docs-api-content）。
 *
 * 形态移植自 Agent2API v3.5.1 `desktop-tauri/ui-shared/docs-api-browser.tsx`，
 * 但按 AIGX 的实际组件面重写（AIGX 没有 A2A 的 @ui 套件）：
 * 1. 章节导航：左侧目录按分组列出，一级是「接口地址」+ 正文各章；
 * 2. 搜索：按「空格分词、全部命中」过滤目录（标题/摘要/路径/正文/字段/示例），
 *    命中的关键词在正文里 <mark> 高亮；Enter 直接跳到第一个结果，Esc 清空；
 * 3. 分页：底部分「上一章 / 下一章」，按目录顺序（不按搜索结果顺序，避免跳过章节）；
 * 4. 一键复制：每个代码块、每条接口地址、Base URL 都能单独复制，复制成功/失败走 toast。
 *
 * 为什么不做「滚动定位」：正文单章最长约 1.5 屏，章节间是并列关系（不是一篇长文），
 * 用章节 pager 比长滚动更接近 A2A 原体验，也让「搜索命中 → 定位」有明确落点。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowLeft, ArrowRight, Check, Copy, Search, X } from 'lucide-react';
import { api } from '../api';
import {
  DOCS_ENDPOINTS,
  DOCS_GROUPS,
  docsApiChapters,
  docsChapterMatches,
  type DocsChapter,
  type DocsExample,
} from '../lib/docs-api-content';
import { isAdmin } from '../lib/utils';
import type { GatewaySettingsItem } from '../types';
import { useToast } from './Toast';
import './docs-api.css';

/** 伪章节 id：接口地址总览（不是正文章，但占目录一格，与 A2A 一致） */
const ADDRESSES_ID = 'addresses';

/** 通配监听地址：不是客户端可访问的地址，回退站点 origin（与 KeysDocsPanel 同一口径） */
const WILDCARD_HOSTS = ['0.0.0.0', '::', '::0', '127.0.0.1', 'localhost'];

/**
 * 解析文档里要展示的 Base URL。
 *
 * 与 `KeysDocsPanel` 完全同一口径（那里是 4 张卡的地址来源）：
 * 管理员读 `/api/gateway/settings` 的 host/port（通配地址回退站点 origin），
 * 普通用户无权读该端点，直接用 `window.location.origin`。
 * 不共用同一个 hook 是刻意的：两个面板各拉一次代价极小（同一 GET），
 * 而把 KeysDocsPanel 的取数逻辑抽走会动到既有 4 张卡（本轮不改它们的语义）。
 */
function useGatewayBase(enabled: boolean): string {
  const [baseUrl, setBaseUrl] = useState('');
  useEffect(() => {
    if (!enabled) return;
    if (!isAdmin()) {
      setBaseUrl(window.location.origin);
      return;
    }
    let alive = true;
    api.getGatewaySettings()
      .then((res) => {
        if (!alive) return;
        const gs = res?.data as GatewaySettingsItem | undefined;
        const host = gs?.host?.trim() || '';
        if (host && !WILDCARD_HOSTS.includes(host.toLowerCase())) {
          setBaseUrl(`http://${host}:${gs?.port ?? 8080}`);
        } else {
          setBaseUrl(window.location.origin);
        }
      })
      .catch(() => {
        if (alive) setBaseUrl(window.location.origin);
      });
    return () => {
      alive = false;
    };
  }, [enabled]);
  return baseUrl;
}

/** 目录条目（伪章节与正文章统一形状，供导航/分页/搜索共用） */
interface BrowserEntry {
  id: string;
  title: string;
  summary: string;
  group: string;
  groupTitle: string;
  path?: string;
  /** 该条目参与搜索的额外文本（伪章节用接口表拼） */
  extraText?: string;
  chapter?: DocsChapter;
}

const escapeRegExp = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** 关键词高亮：把命中的分词包成 <mark>，其余原样输出 */
function Highlight({ text, terms }: { text: string; terms: string[] }): JSX.Element {
  if (terms.length === 0) return <>{text}</>;
  const pattern = new RegExp(`(${terms.map(escapeRegExp).join('|')})`, 'gi');
  const parts = text.split(pattern);
  return (
    <>
      {parts.map((part, index) =>
        index % 2 === 1 ? (
          <mark className="docs-api-mark" key={index}>
            {part}
          </mark>
        ) : (
          <span key={index}>{part}</span>
        ),
      )}
    </>
  );
}

/** 一个可复制的代码块 */
function ExampleBlock({
  example,
  onCopy,
}: {
  example: DocsExample;
  onCopy: (text: string, label: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="docs-api-example">
      <div className="docs-api-example-head">
        <span className="docs-api-example-label">{t(example.label)}</span>
        <button
          type="button"
          className="docs-api-copy"
          onClick={() => onCopy(example.code, t(example.label))}
          aria-label={`${t('复制示例')}：${t(example.label)}`}
        >
          <Copy size={12} /> {t('复制示例')}
        </button>
      </div>
      <pre className="docs-api-pre" tabIndex={0} aria-label={t(example.label)}>
        <code data-language={example.language}>{example.code}</code>
      </pre>
    </div>
  );
}

export interface DocsApiBrowserProps {
  /** 页面实际的 Base URL；不传则由组件自己按角色解析（推荐） */
  origin?: string;
}

export default function DocsApiBrowser({ origin }: DocsApiBrowserProps = {}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  // origin prop 缺省时自己解析（Keys 页直接 `<DocsApiBrowser />` 即可）
  const resolvedOrigin = useGatewayBase(!origin);
  const effectiveOrigin = origin || resolvedOrigin;

  const base = (effectiveOrigin || '').replace(/\/+$/, '') || 'http://127.0.0.1:9527';
  const chapters = useMemo(() => docsApiChapters(effectiveOrigin), [effectiveOrigin]);

  // ── 目录 = 接口地址（伪章）+ 正文各章 ─────────────────────────
  const entries = useMemo<BrowserEntry[]>(() => {
    const addresses: BrowserEntry = {
      id: ADDRESSES_ID,
      title: '接口地址',
      summary: '当前网关的客户端地址与全部接口路径，逐条可复制。',
      group: 'start',
      groupTitle: '接入指南',
      extraText: DOCS_ENDPOINTS.map((ep) => `${ep.method} ${ep.path} ${ep.name} ${ep.note} ${ep.group} ${ep.auth}`).join('\n'),
    };
    const groupTitle = new Map(DOCS_GROUPS.map((group) => [group.id, group.title]));
    const body: BrowserEntry[] = chapters.map((chapter) => ({
      id: chapter.id,
      title: chapter.title,
      summary: chapter.summary,
      group: chapter.group,
      groupTitle: groupTitle.get(chapter.group) ?? '',
      path: chapter.path,
      chapter,
    }));
    return [addresses, ...body];
  }, [chapters]);

  const [selected, setSelected] = useState<string>(ADDRESSES_ID);
  const [query, setQuery] = useState('');
  const [copiedKey, setCopiedKey] = useState('');
  const articleRef = useRef<HTMLElement | null>(null);
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const terms = useMemo(
    () => query.trim().toLowerCase().split(/\s+/).filter(Boolean),
    [query],
  );
  const searching = terms.length > 0;

  const active = entries.find((entry) => entry.id === selected) ?? entries[0];

  // 搜索过滤：伪章节按接口表文本，正文章按 docsChapterMatches
  const matches = useCallback(
    (entry: BrowserEntry): boolean => {
      if (terms.length === 0) return true;
      if (entry.chapter) {
        if (docsChapterMatches(entry.chapter, query)) return true;
        // 正文章也要能被「接口路径」之类命中，附加判据不额外扩大口径
        return false;
      }
      const haystack = [entry.title, entry.summary, entry.extraText ?? ''].join('\n').toLowerCase();
      return terms.every((term) => haystack.includes(term));
    },
    [query, terms],
  );

  const visibleEntries = useMemo(() => entries.filter((entry) => matches(entry)), [entries, matches]);

  const grouped = useMemo(
    () =>
      DOCS_GROUPS.map((group) => ({
        ...group,
        entries: visibleEntries.filter((entry) => entry.group === group.id),
      })).filter((group) => group.entries.length > 0),
    [visibleEntries],
  );

  const index = entries.findIndex((entry) => entry.id === active.id);
  const previous = index > 0 ? entries[index - 1] : undefined;
  const next = index >= 0 && index < entries.length - 1 ? entries[index + 1] : undefined;

  // 切章后回到顶部并把焦点交给标题（键盘用户不迷路）
  useEffect(() => {
    if (articleRef.current) articleRef.current.scrollTop = 0;
    headingRef.current?.focus({ preventScroll: true });
  }, [active.id]);

  const copyText = useCallback(
    (text: string, label: string): void => {
      const done = (): void => {
        addToast(`${t('已复制')}：${label}`);
        setCopiedKey(label);
        window.setTimeout(() => setCopiedKey(''), 1600);
      };
      const fail = (): void => addToast(t('复制失败，请手动选择复制'), 'error');
      if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
        void navigator.clipboard.writeText(text).then(done).catch(fail);
        return;
      }
      // 降级：临时 textarea + execCommand（file:// 或旧浏览器）
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand('copy');
      document.body.removeChild(ta);
      ok ? done() : fail();
    },
    [addToast, t],
  );

  const navigate = useCallback((id: string) => {
    setSelected(id);
  }, []);

  const clearSearch = useCallback(() => {
    setQuery('');
    searchRef.current?.focus();
  }, []);

  const pageLink = (entry: BrowserEntry, direction: 'previous' | 'next'): JSX.Element => (
    <button type="button" className="docs-api-pager-btn" onClick={() => navigate(entry.id)}>
      {direction === 'previous' && <ArrowLeft size={14} />}
      <span className="docs-api-pager-text">
        <span className="docs-api-pager-kicker">{direction === 'previous' ? t('上一章') : t('下一章')}</span>
        <span className="docs-api-pager-title">{t(entry.title)}</span>
      </span>
      {direction === 'next' && <ArrowRight size={14} />}
    </button>
  );

  return (
    <div className="docs-api">
      <div className="docs-api-layout">
        {/* ── 目录 ─────────────────────────────────────────────── */}
        <aside className="docs-api-side" aria-label={t('文档目录')}>
          <div className="docs-api-side-head">
            <div className="docs-api-side-title">{t('接口文档')}</div>
            <p className="docs-api-side-caption">{t('接入指南与接口参考（完整正文）')}</p>
            <div className="docs-api-search">
              <Search size={14} className="docs-api-search-icon" aria-hidden="true" />
              <input
                ref={searchRef}
                type="search"
                value={query}
                autoComplete="off"
                placeholder={t('搜索文档、参数或接口…')}
                aria-label={t('搜索文档')}
                onChange={(event) => setQuery(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Escape') {
                    event.preventDefault();
                    clearSearch();
                  } else if (
                    event.key === 'Enter' &&
                    !event.nativeEvent.isComposing &&
                    searching &&
                    visibleEntries[0]
                  ) {
                    event.preventDefault();
                    navigate(visibleEntries[0].id);
                  }
                }}
              />
              {query && (
                <button
                  type="button"
                  className="docs-api-search-clear"
                  onClick={clearSearch}
                  aria-label={t('清除搜索')}
                  title={t('清除搜索')}
                >
                  <X size={13} />
                </button>
              )}
            </div>
          </div>

          <nav className="docs-api-nav" aria-label={t('接口参考目录')}>
            {searching && (
              <div className="docs-api-hit" role="status">
                {t('找到 {{count}} 个章节', { count: visibleEntries.length })}
              </div>
            )}
            {grouped.map((group) => (
              <section key={group.id} className="docs-api-nav-group">
                <div className="docs-api-nav-group-label">{t(group.title)}</div>
                <ul className="docs-api-nav-list">
                  {group.entries.map((entry) => (
                    <li key={entry.id}>
                      <button
                        type="button"
                        className={`docs-api-nav-item${active.id === entry.id ? ' docs-api-nav-item-active' : ''}`}
                        aria-current={active.id === entry.id ? 'page' : undefined}
                        onClick={() => navigate(entry.id)}
                      >
                        <span className="docs-api-nav-title">
                          <Highlight text={t(entry.title)} terms={terms} />
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
            {visibleEntries.length === 0 && (
              <div className="docs-api-empty">
                <div className="docs-api-empty-title">{t('没有找到匹配的文档')}</div>
                <p className="docs-api-empty-note">{t('试试接口路径、参数名或其他关键词。')}</p>
                <button type="button" className="docs-api-copy" onClick={clearSearch}>
                  {t('清除搜索')}
                </button>
              </div>
            )}
          </nav>
        </aside>

        {/* ── 正文 ─────────────────────────────────────────────── */}
        <article
          id="docs-api-article"
          ref={articleRef}
          className="docs-api-article"
          aria-labelledby="docs-api-article-title"
        >
          <div className="docs-api-article-inner">
            <header className="docs-api-head">
              <div className="docs-api-breadcrumb">
                <span>{t('接口文档')}</span>
                <span aria-hidden="true">/</span>
                <span>{t(active.groupTitle)}</span>
              </div>
              <h2 id="docs-api-article-title" ref={headingRef} tabIndex={-1}>
                {t(active.title)}
              </h2>
              <p className="docs-api-summary">
                <Highlight text={t(active.summary)} terms={terms} />
              </p>
              {active.path && <code className="docs-api-path">{active.path}</code>}
            </header>

            {active.id === ADDRESSES_ID ? (
              <AddressesSection base={base} terms={terms} onCopy={copyText} />
            ) : (
              active.chapter && <ChapterBody chapter={active.chapter} terms={terms} onCopy={copyText} />
            )}

            <nav className="docs-api-pager" aria-label={t('章节导航')}>
              <div className="docs-api-pager-side">{previous ? pageLink(previous, 'previous') : null}</div>
              <div className="docs-api-pager-side docs-api-pager-side-next">
                {next ? pageLink(next, 'next') : null}
              </div>
            </nav>
          </div>
        </article>
      </div>
      {/* copiedKey 只用于给按钮一个短暂的「已复制」反馈；这里挂 aria-live 让读屏也知道 */}
      <span className="sr-only" role="status" aria-live="polite">
        {copiedKey ? `${t('已复制')}：${copiedKey}` : ''}
      </span>
    </div>
  );
}

/** 接口地址总览（伪章节正文） */
function AddressesSection({
  base,
  terms,
  onCopy,
}: {
  base: string;
  terms: string[];
  onCopy: (text: string, label: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  let lastGroup = '';
  return (
    <div className="docs-api-body">
      <p className="docs-api-p">
        {t('下表是当前网关的数据面地址。带 /v1 的路径与无前缀兼容路径同时可用；Anthropic 官方 SDK / Claude Code 填根地址（不带 /v1），其余客户端按各协议说明填。')}
      </p>
      <div className="docs-api-endpoints">
        {DOCS_ENDPOINTS.map((endpoint) => {
          const showGroup = endpoint.group !== lastGroup;
          lastGroup = endpoint.group;
          const full = endpoint.method
            ? `${endpoint.method} ${base}${endpoint.path}`
            : `${base}${endpoint.path}`;
          return (
            <div key={endpoint.id}>
              {showGroup && <div className="docs-api-ep-group">{t(endpoint.group)}</div>}
              <div className="docs-api-ep">
                <div className="docs-api-ep-head">
                  <span className="docs-api-ep-name">
                    <Highlight text={t(endpoint.name)} terms={terms} />
                  </span>
                  <span className="docs-api-ep-note">
                    <Highlight text={t(endpoint.note)} terms={terms} />
                  </span>
                </div>
                <div className="docs-api-ep-line">
                  <code className="docs-api-ep-value">{full}</code>
                  <button
                    type="button"
                    className="docs-api-copy docs-api-copy-icon"
                    onClick={() => onCopy(full, t(endpoint.name))}
                    aria-label={`${t('复制地址')}：${t(endpoint.name)}`}
                    title={t('复制地址')}
                  >
                    <Copy size={12} />
                  </button>
                </div>
                <div className="docs-api-ep-auth">
                  {t('鉴权')}：{t(endpoint.auth)}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** 一章正文（段落 / 字段表 / 注意事项 / 警告 / 示例） */
function ChapterBody({
  chapter,
  terms,
  onCopy,
}: {
  chapter: DocsChapter;
  terms: string[];
  onCopy: (text: string, label: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="docs-api-body">
      {chapter.paragraphs?.length ? (
        <div className="docs-api-prose">
          {chapter.paragraphs.map((paragraph, index) => (
            <p className="docs-api-p" key={index}>
              <Highlight text={t(paragraph)} terms={terms} />
            </p>
          ))}
        </div>
      ) : null}

      {chapter.fields?.length ? (
        <section className="docs-api-block" aria-label={t('字段与限制')}>
          <h3 className="docs-api-h3">{t('字段与限制')}</h3>
          <div className="docs-api-table-wrap">
            <table className="docs-api-table">
              <thead>
                <tr>
                  <th scope="col">{t('字段 / 状态')}</th>
                  <th scope="col">{t('类型 / 范围')}</th>
                  <th scope="col">{t('说明')}</th>
                </tr>
              </thead>
              <tbody>
                {chapter.fields.map((field) => (
                  <tr key={field.name}>
                    <td>
                      <code className="docs-api-field-name">
                        <Highlight text={field.name} terms={terms} />
                      </code>
                    </td>
                    <td className="docs-api-field-type">
                      <Highlight text={field.type} terms={terms} />
                    </td>
                    <td>
                      <Highlight text={t(field.description)} terms={terms} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      ) : null}

      {chapter.warning ? (
        <div className="docs-api-warning" role="note">
          <strong>{t('注意')}</strong>
          <span>
            <Highlight text={t(chapter.warning)} terms={terms} />
          </span>
        </div>
      ) : null}

      {chapter.points?.length ? (
        <section className="docs-api-block" aria-label={t('使用说明与兼容边界')}>
          <h3 className="docs-api-h3">{t('使用说明与兼容边界')}</h3>
          <ul className="docs-api-points">
            {chapter.points.map((point, index) => (
              <li key={index}>
                <Highlight text={t(point)} terms={terms} />
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {chapter.examples?.length ? (
        <section className="docs-api-block" aria-label={t('请求与响应示例')}>
          <h3 className="docs-api-h3">{t('请求与响应示例')}</h3>
          <p className="docs-api-note">{t('示例仅供复制，不会在本页发起请求或消耗额度。key 一律写占位符 sk-xxx。')}</p>
          {chapter.examples.map((example) => (
            <ExampleBlock key={example.label} example={example} onCopy={onCopy} />
          ))}
        </section>
      ) : null}
    </div>
  );
}

/** 成功复制的勾选反馈（供将来内联到按钮；当前 toast 已覆盖，保留导出便于复用） */
export function CopiedIcon(): JSX.Element {
  return <Check size={12} />;
}
