/**
 * 特殊路由三面板共用的零件：面板壳 / 分组 / 规则行 / 模型下拉 / 保存条 / 冲突提示。
 *
 * 三块配置的真实形状都是「一份规则列表」：
 *   一句话说清这类请求怎么被改写 → 规则列表（每行一句话）→ 行内编辑。
 * 差别只在表单字段，所以壳子与状态呈现收在这里，面板只写自己的字段。
 *
 * 视觉口径走 AIGX 自有令牌（Routing.css 的 .rt-* / App.css 的 .card 体系），
 * 不引入第二套设计语言。
 */
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, Plus, Trash2 } from 'lucide-react';
import { Badge, Button, Switch } from '../../ui';
import type { SpecialEditorState } from './useSpecialRouteEditor';

/** 模型下拉项 */
export interface ModelOption {
  id: string;
  label: string;
}

/** 面板壳：标题 + 一句话 + 总开关 + 正文 + 保存条 */
export function RulePanel<T, V extends { config?: Partial<T> | null; revision?: string }>({
  title,
  lede,
  editor,
  addLabel,
  addDisabled,
  onAddRule,
  children,
}: {
  title: string;
  lede: string;
  editor: SpecialEditorState<T, V>;
  addLabel?: string;
  addDisabled?: boolean;
  onAddRule?: () => void;
  children: ReactNode;
}): JSX.Element {
  const { t } = useTranslation();
  const { draft, loading, saving, dirty, error, conflict, notReady, update, save, reload, discard } = editor;

  if (notReady) {
    return (
      <section className="rt-special-panel">
        <header className="rt-special-head">
          <h2>{title}</h2>
        </header>
        <div className="rt-notice rt-notice-warn" role="status">
          {t('该配置端点尚未就绪（后端建设中）。就绪后这里可以启用并编辑规则；现在保存会明确失败，不会谎报成功。')}
        </div>
      </section>
    );
  }

  return (
    <section className="rt-special-panel" aria-label={title}>
      <header className="rt-special-head">
        <div className="rt-special-headtext">
          <h2>{title}</h2>
          <p className="rt-special-lede">{lede}</p>
        </div>
        <label className="rt-special-enable">
          <span>
            {draft && (draft as unknown as { enabled?: boolean }).enabled ? t('已启用') : t('已关闭')}
          </span>
          <Switch
            checked={Boolean(draft && (draft as unknown as { enabled?: boolean }).enabled)}
            disabled={loading || saving || !draft}
            ariaLabel={t('{{name}}总开关', { name: title })}
            onChange={(next) =>
              update((current) => ({ ...current, enabled: next }) as unknown as T)
            }
          />
        </label>
      </header>

      {conflict ? (
        <div className="rt-notice rt-notice-warn rt-conflict" role="alert">
          <strong>{t('修订号冲突：')}</strong>
          {t('这份配置在别处已被修改，本次保存被拒绝（未覆盖他人修改）。你的草稿仍保留在界面上；重新读取会丢弃草稿并载入最新版本，再保存即可。')}
          <div className="rt-conflict-actions">
            <Button variant="outline" size="sm" disabled={saving} onClick={() => void reload()}>
              {t('重新读取（丢弃我的草稿）')}
            </Button>
          </div>
        </div>
      ) : null}

      {error && !conflict ? (
        <div className="error-message" role="alert">
          {error}
        </div>
      ) : null}

      {loading ? (
        <div className="rt-special-loading">
          <Loader2 size={14} className="spin" />
          {t('读取中…')}
        </div>
      ) : (
        <div className="rt-special-body">
          {onAddRule ? (
            <div className="rt-special-toolbar">
              <Button variant="outline" size="sm" disabled={saving || addDisabled} onClick={onAddRule}>
                <Plus size={13} />
                {addLabel ?? t('添加规则')}
              </Button>
            </div>
          ) : null}
          {children}
        </div>
      )}

      <footer className="rt-savebar">
        <Button variant="primary" onClick={() => void save()} disabled={!dirty || saving || !draft}>
          {saving ? <Loader2 size={13} className="spin" /> : null}
          {t('保存')}
        </Button>
        <Button variant="outline" onClick={discard} disabled={!dirty || saving}>
          {t('放弃更改')}
        </Button>
        <Button variant="outline" disabled={loading || saving} onClick={() => void reload()}>
          {t('重新读取')}
        </Button>
        <span className="rt-dirty">{dirty ? t('有未保存的修改') : t('已保存')}</span>
      </footer>
    </section>
  );
}

/** 规则分组标题（计数 + 行尾操作） */
export function RuleSectionHead({
  title,
  count,
  actions,
}: {
  title: string;
  count?: string;
  actions?: ReactNode;
}): JSX.Element {
  return (
    <div className="rt-special-section-head">
      <h3>{title}</h3>
      {count ? <span className="rt-count">{count}</span> : null}
      {actions ? <div className="rt-special-section-actions">{actions}</div> : null}
    </div>
  );
}

/** 一条规则行：主体 + 行尾操作（编辑按钮由面板给） */
export function RuleRow({
  off,
  title,
  detail,
  meta,
  actions,
}: {
  off?: boolean;
  title: ReactNode;
  detail?: ReactNode;
  meta?: ReactNode;
  actions: ReactNode;
}): JSX.Element {
  return (
    <div className={`rt-special-row${off ? ' is-off' : ''}`}>
      <div className="rt-special-row-main">
        <div className="rt-special-row-title">{title}</div>
        {detail ? <div className="rt-special-row-detail">{detail}</div> : null}
      </div>
      {meta ? <div className="rt-special-row-meta">{meta}</div> : null}
      <div className="rt-special-row-actions">{actions}</div>
    </div>
  );
}

/** 删除按钮（行内） */
export function RowDeleteButton({ onClick, disabled }: { onClick: () => void; disabled?: boolean }): JSX.Element {
  const { t } = useTranslation();
  return (
    <Button variant="outline" size="sm" disabled={disabled} onClick={onClick}>
      <Trash2 size={13} />
      {t('删除')}
    </Button>
  );
}

/** 模型下拉：有候选时用原生 select（展示 provider · name，提交 id）；
 *  候选清单为空（端点降级 / 模型目录未加载）时退化为自由文本输入 ——
 *  否则用户连关闭规则都存不下去。 */
export function ModelSelect({
  value,
  options,
  onChange,
  disabled,
  ariaLabel,
  placeholder,
}: {
  value: string;
  options: ModelOption[];
  onChange: (next: string) => void;
  disabled?: boolean;
  ariaLabel: string;
  placeholder?: string;
}): JSX.Element {
  const { t } = useTranslation();
  if (!options.length) {
    return (
      <input
        className="form-input"
        aria-label={ariaLabel}
        placeholder={placeholder ?? t('模型名')}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      />
    );
  }
  return (
    <select
      className="form-input"
      aria-label={ariaLabel}
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value)}
    >
      <option value="">{placeholder ?? t('请选择模型')}</option>
      {options.some((o) => o.id === value) ? null : value ? (
        <option value={value}>{`${value}${t('（当前不可用）')}`}</option>
      ) : null}
      {options.map((o) => (
        <option key={o.id} value={o.id}>
          {o.label === o.id ? o.id : `${o.id} · ${o.label}`}
        </option>
      ))}
    </select>
  );
}

/** 能力/状态小徽章（规则行右侧） */
export function StateBadge({ on, children }: { on: boolean; children: ReactNode }): JSX.Element {
  return <Badge tone={on ? 'success' : 'neutral'}>{children}</Badge>;
}