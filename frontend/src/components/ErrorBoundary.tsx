import React from 'react';
import { withTranslation, type WithTranslation } from 'react-i18next';

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
  info: { componentStack?: string } | null;
}

type ErrorBoundaryProps = WithTranslation & {
  children?: React.ReactNode;
};

/**
 * ErrorBoundary — 捕获子树渲染异常，展示友好错误页。
 * 保持 cf-ai-gw 玻璃拟态风格，复用 App.css 中的 CSS 变量。
 */
class ErrorBoundary extends React.Component<ErrorBoundaryProps, ErrorBoundaryState> {
  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = { hasError: false, error: null, info: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error, info: null };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    // 仅在控制台留痕，不外发
    // eslint-disable-next-line no-console
    console.error('[ErrorBoundary]', error, info);
    this.setState({ info });
  }

  handleRetry = () => {
    this.setState({ hasError: false, error: null, info: null });
  };

  handleHome = () => {
    this.setState({ hasError: false, error: null, info: null });
    window.location.href = '/';
  };

  render() {
    if (!this.state.hasError) return this.props.children;
    const { t } = this.props;
    const errText =
      (this.state.error && (this.state.error.message || String(this.state.error))) ||
      t('未知错误');

    return (
      <div
        style={{
          minHeight: '100vh',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          padding: '24px',
          background: 'var(--bg, #0b1020)',
          color: 'var(--text-main, #f8fafc)',
          fontFamily: "'Outfit', sans-serif",
        }}
      >
        <div
          style={{
            width: '100%',
            maxWidth: '520px',
            padding: '28px 24px',
            borderRadius: '16px',
            background: 'var(--card-bg, rgba(30,41,59,0.45))',
            border: '1px solid var(--border-color, rgba(255,255,255,0.08))',
            backdropFilter: 'blur(var(--glass-blur, 20px))',
            WebkitBackdropFilter: 'blur(var(--glass-blur, 20px))',
            boxShadow: '0 20px 60px rgba(0,0,0,0.35)',
            textAlign: 'center',
          }}
        >
          <div
            style={{
              width: '56px',
              height: '56px',
              margin: '0 auto 16px',
              borderRadius: '14px',
              background: 'var(--primary-gradient, linear-gradient(135deg,#6366f1,#a855f7,#ec4899))',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              fontSize: '28px',
              color: 'white',
              boxShadow: '0 8px 24px rgba(168,85,247,0.35)',
            }}
            aria-hidden="true"
          >
            ⚠️
          </div>

          <h1
            style={{
              fontSize: '20px',
              fontWeight: 700,
              margin: '0 0 8px',
              letterSpacing: '-0.5px',
            }}
          >
            {t('出错了')}
          </h1>
          <p
            style={{
              fontSize: '13px',
              color: 'var(--text-muted, #94a3b8)',
              margin: '0 0 16px',
              lineHeight: 1.6,
            }}
          >
            {t('页面渲染时发生异常，可以重试或返回首页。')}
          </p>

          <pre
            style={{
              textAlign: 'left',
              margin: '0 0 24px',
              padding: '12px 14px',
              borderRadius: '10px',
              background: 'rgba(0,0,0,0.25)',
              border: '1px solid var(--border-color, rgba(255,255,255,0.08))',
              color: 'var(--text-muted, #94a3b8)',
              fontSize: '12px',
              fontFamily: 'monospace',
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-word',
              maxHeight: '180px',
              overflow: 'auto',
            }}
          >
            {errText}
          </pre>

          <div style={{ display: 'flex', gap: '12px', justifyContent: 'center' }}>
            <button
              onClick={this.handleRetry}
              style={{
                flex: '0 1 auto',
                padding: '10px 22px',
                borderRadius: '10px',
                border: 'none',
                cursor: 'pointer',
                fontWeight: 600,
                fontSize: '14px',
                color: 'white',
                background: 'var(--primary-gradient, linear-gradient(135deg,#6366f1,#a855f7,#ec4899))',
                boxShadow: '0 6px 18px rgba(99,102,241,0.3)',
              }}
            >
              {t('重试')}
            </button>
            <button
              onClick={this.handleHome}
              style={{
                flex: '0 1 auto',
                padding: '10px 22px',
                borderRadius: '10px',
                cursor: 'pointer',
                fontWeight: 600,
                fontSize: '14px',
                color: 'var(--text-main, #f8fafc)',
                background: 'transparent',
                border: '1px solid var(--border-color, rgba(255,255,255,0.16))',
              }}
            >
              {t('返回首页')}
            </button>
          </div>
        </div>
      </div>
    );
  }
}

export default withTranslation()(ErrorBoundary);
