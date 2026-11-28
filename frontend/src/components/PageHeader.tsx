/**
 * PageHeader — 公共页面标题区组件。
 *
 * 用于收敛 AIGX 前端各页裸写的 `<div className="page-header">…</div>` 结构，
 * 全局 CSS 唯一定义点：App.css `.page-header / h1 / p` 与响应式断点。
 *
 * 渲染形态（与现有 CSS 兼容，不引入新布局规则）：
 *
 * 1) 平铺（无 actions）：
 *      <div className="page-header"><h1/><p/></div>
 *    用于 Users / Logs / Settings / Keys / Wallet / Tickets(列表) /
 *    Channels / AccountSessions / Plans / Redemptions / Products /
 *    Profile / ModelPricingPage 等仅需要标题+副标题的页面。
 *
 * 2) 左侧 wrapper + 右侧 actions：
 *      <div className="page-header"><div><h1/><p/></div>{actions}</div>
 *    用于 Dashboard / ModelManage / Prompts / PluginManagerPage /
 *    CheckinCenter / Diagnostics 等需要在标题右侧放按钮的页面。
 *    当传入了 `actions` 时，自动在 h1+p 外面套一层 <div> 以保持
 *    `.page-header` 的 flex 双列布局。
 *    兼容：Routing / LimitPage / Maintenance / ToolsPage 等历史上
 *    就包了 <div> 但没有 actions 的页面 — 这些场景下即使不传
 *    actions 也保留 wrapper（见下方 `forceLeftWrapper`）。
 *
 * 3) 特殊 className 扩展：
 *    通过 `className` 透传，例如 Gateways 的 `page-header gw-header`。
 *
 * 4) embedded 模式：
 *    当 `hidden=true` 时返回 null。Channels / AccountSessions /
 *    Plans / Redemptions / CheckinCenter / Diagnostics 等嵌入到
 *    宿主页面时通过 `!embedded` 控制本区不渲染，把这些场景换成
 *    `hidden={embedded}` 即可。
 *
 * 样式：
 * - 不引入任何新的 CSS。继续走 App.css `.page-header / h1 / p`。
 * - 不改 i18n 文案；调用方继续传 `t('…')` 表达式。
 *
 * Props：
 * - `title`: 必需。页面主标题内容（通常 `{t('…')}`）。
 * - `subtitle`: 可选。副标题内容，没有时可以不传。
 * - `actions`: 可选 ReactNode。传入时渲染在 page-header 右侧，
 *              且 h1+p 外面自动包一层 <div> 用于 flex 布局。
 * - `className`: 可选额外类名，透传到 `.page-header` 外层。
 * - `forceLeftWrapper`: 无 actions 也强制给 h1+p 包 <div>，
 *              用于保留某些历史 DOM 结构（如 Routing / LimitPage /
 *              Maintenance / ToolsPage / Dashboard:249 设置视图）。
 * - `hidden`: 为 true 时返回 null，供 embedded 模式使用。
 */

import type { ReactNode } from 'react';

export interface PageHeaderProps {
  title: ReactNode;
  subtitle?: ReactNode;
  actions?: ReactNode;
  className?: string;
  forceLeftWrapper?: boolean;
  hidden?: boolean;
}

export function PageHeader({
  title,
  subtitle,
  actions,
  className,
  forceLeftWrapper = false,
  hidden = false,
}: PageHeaderProps): JSX.Element | null {
  if (hidden) return null;

  const classes = 'page-header' + (className ? ` ${className}` : '');
  const useLeftWrapper = Boolean(actions) || forceLeftWrapper;

  if (useLeftWrapper) {
    return (
      <div className={classes}>
        <div>
          <h1>{title}</h1>
          {subtitle != null ? <p>{subtitle}</p> : null}
        </div>
        {actions}
      </div>
    );
  }

  return (
    <div className={classes}>
      <h1>{title}</h1>
      {subtitle != null ? <p>{subtitle}</p> : null}
    </div>
  );
}

export default PageHeader;
