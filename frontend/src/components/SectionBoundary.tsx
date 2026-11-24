import React from 'react';
import { withTranslation, type WithTranslation } from 'react-i18next';

interface SectionBoundaryState {
  hasError: boolean;
  error: Error | null;
}

type SectionBoundaryProps = WithTranslation & {
  /** 区块名（显示在降级 UI 上，让人一眼知道哪块坏了） */
  title: string;
  children?: React.ReactNode;
};

/**
 * SectionBoundary — **区块级**错误隔离。
 *
 * 为什么要有它（与 `ErrorBoundary` 的分工）：
 * - `ErrorBoundary` 管「整页崩了」：要么全屏错误页、要么占满内容区；
 * - 本组件只管「某一块坏了」：破甲页有 8 个区块，预设库/词表/统计各自解析
 *   不同形状的数据。一个区块吃到意外形状就**不该拖垮整页**——尤其是「保存」
 *   按钮和开关，用户可能只是想赶紧把当前改动落库。
 *
 * 之前实测过这个代价：一个 `d.reduce is not a function` 让整页只剩「出错了」，
 * 明明坏的是列表渲染，却连配置都改不了。
 *
 * 必须是 class：React 只能在 class 组件里捕获子树渲染异常（`componentDidCatch`）。
 */
class SectionBoundary extends React.Component<SectionBoundaryProps, SectionBoundaryState> {
  constructor(props: SectionBoundaryProps) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): Partial<SectionBoundaryState> {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error): void {
    // 保留控制台痕迹：降级 UI 只是兜底，真因仍要能在 devtools 里看到
    console.error(`[SectionBoundary:${this.props.title}]`, error);
  }

  private readonly reset = (): void => {
    this.setState({ hasError: false, error: null });
  };

  render(): React.ReactNode {
    if (!this.state.hasError) return this.props.children;
    const { t, title } = this.props;
    return (
      <div className="purge-warning-box" role="alert" data-testid="purge-section-fallback">
        <p className="purge-warning-row">
          <span>
            {t('「{{title}}」这块渲染出错，页面其余部分仍可用。', { title })}
            {this.state.error ? ` ${this.state.error.message}` : ''}
          </span>
        </p>
        <div className="purge-actions">
          <button type="button" className="btn btn-outline btn-sm" onClick={this.reset}>
            {t('重试这一块')}
          </button>
        </div>
      </div>
    );
  }
}

export const SectionGuard = withTranslation()(SectionBoundary);