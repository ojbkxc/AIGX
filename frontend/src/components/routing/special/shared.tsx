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
import { Button, Switch } from '../../ui';
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
  serverError,
  children,
}: {
  title: string;
  lede: string;
  editor: SpecialEditorState<T, V>;
  /** 后端解析已存配置失败时的错误（config 为 null）：如实告知，且禁止覆盖保存 */
  serverError?: string | null;
  children: ReactNode;
}): JSX.Element {
  const { t } = useTranslation();
  const { draft, loading, saving, dirty, error, conflict, notReady, update, save, reload, discard } = editor;
  const blockedByServerError = Boolean(serverError);

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

      {/* 对齐 A2A `RulePanel` 的那句常驻说明：规则不改选账号规则、也不越过网关 Key 权限。 */}
      <p className="rt-special-note">
        {t('规则全局生效，不受原路由池限制；指定模型按自己的路由选账号，网关 Key 权限仍然生效。')}
      </p>

      {serverError ? (
        <div className="rt-notice rt-notice-warn" role="alert">
          <strong>{t('服务端已存配置解析失败：')}</strong>
          {serverError}
          <div className="form-hint">
            {t('为避免用空草稿覆盖掉服务端文件，保存已禁用。请先修复服务端配置或联系管理员。')}
          </div>
        </div>
      ) : null}

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
          {children}
        </div>
      )}

      {/* 底栏口径照 A2A `RulePanel`：状态字在左，动作在右，顺序「重新读取 → 撤销修改 → 保存」。
          保存条留在面板底部而不是提到页头工具条：三块配置各有一份修订号、各自独立保存，
          页头那一格同时只能属于一块，「保存」离它管的内容太远反而容易点错。 */}
      <footer className="rt-savebar rt-special-foot">
        <span className="rt-special-foot-state">{dirty ? t('有未保存的修改') : t('已保存')}</span>
        <div className="rt-special-foot-actions">
          <Button variant="secondary" size="sm" disabled={loading || saving} onClick={() => void reload()}>
            {t('重新读取')}
          </Button>
          <Button variant="secondary" size="sm" onClick={discard} disabled={!dirty || saving}>
            {t('撤销修改')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            onClick={() => void save()}
            disabled={!dirty || saving || !draft || blockedByServerError}
          >
            {saving ? <Loader2 size={13} className="spin" /> : null}
            {saving ? t('保存中…') : t('保存')}
          </Button>
        </div>
      </footer>
    </section>
  );
}

/**
 * 弹窗顶部那条「启用规则」：开关在左、状态右对齐。
 *
 * 对齐 A2A `RuleSwitchRow` —— 启用状态是这条规则的头等属性，单独一行放在弹窗最上面，
 * 而不是混在表单字段网格里（混在一起时读者要扫完整个表单才知道这条规则是开是关）。
 */
export function RuleSwitchRow({
  enabled,
  disabled,
  onChange,
}: {
  enabled: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="rt-special-switch-row">
      <Switch checked={enabled} disabled={disabled} ariaLabel={t('启用规则')} onChange={onChange} />
      <span>{t('启用规则')}</span>
      <small>{enabled ? t('已启用') : t('已停用')}</small>
    </div>
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

/**
 * 「添加规则」按钮：挂在 RuleSectionHead 的行尾操作位（对齐 A2A ·
 * 它的「添加规则」就在规则分组标题右侧，而不是面板顶部另起一条工具条）。
 * 禁用判据由各面板决定（上限已满 / 无候选模型 / 保存中）。
 */
export function AddRuleButton({
  onClick,
  disabled,
  label,
}: {
  onClick: () => void;
  disabled?: boolean;
  label?: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <Button variant="outline" size="sm" disabled={disabled} onClick={onClick}>
      <Plus size={13} />
      {label ?? t('添加规则')}
    </Button>
  );
}

/**
 * 一条规则行：整行可点（真按钮，键盘能 Tab 到），右侧一枚 chevron 暗示「点进去改」。
 *
 * 对齐 A2A `RuleRow`：行尾不再挂「编辑 / 删除」两枚按钮（那会让每行右侧变成
 * 按钮带，且删除是危险动作却与编辑同级），删除收进弹窗底栏的「删除规则」。
 */
export function RuleRow({
  off,
  title,
  detail,
  meta,
  actions,
  onOpen,
}: {
  off?: boolean;
  title: ReactNode;
  detail?: ReactNode;
  meta?: ReactNode;
  /** 行尾附加内容（计数/徽标等）；编辑动作本身由 onOpen 承担 */
  actions?: ReactNode;
  /** 整行点击：打开该规则的编辑弹窗 */
  onOpen?: () => void;
}): JSX.Element {
  const body = (
    <>
      <div className="rt-special-row-main">
        <div className="rt-special-row-title">{title}</div>
        {detail ? <div className="rt-special-row-detail">{detail}</div> : null}
      </div>
      {meta || actions ? (
        <div className="rt-special-row-meta">
          {meta}
          {actions}
        </div>
      ) : null}
      {onOpen ? (
        <span className="rt-special-row-caret" aria-hidden="true">›</span>
      ) : null}
    </>
  );
  if (!onOpen) {
    return <div className={`rt-special-row${off ? ' is-off' : ''}`}>{body}</div>;
  }
  return (
    <button
      type="button"
      className={`rt-special-row rt-special-row-btn${off ? ' is-off' : ''}`}
      onClick={onOpen}
    >
      {body}
    </button>
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

