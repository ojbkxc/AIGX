// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 网页工具调用面板（系统设置 → 网关）—— 对照 A2A ui-islands/settings-web-tools.tsx：
//   为「明确声明支持工具调用的网页模型」开启工具模拟。网关用提示词与响应校验
//   把工具调用转换给客户端执行；原生工具调用与无工具普通聊天不受影响。
// 契约：GET /api/web-tool-emulation（{enabled, providers[]}）、
//       PUT /api/web-tool-emulation `{enabled}`。端点未就绪（404）降级。

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, Switch } from '../components/ui';

interface ToolSettings {
  enabled: boolean;
  providers: Array<{ id: string; label: string; models?: unknown[] }>;
}

function parse(value: unknown): ToolSettings | null {
  if (!value || typeof value !== 'object' || typeof (value as ToolSettings).enabled !== 'boolean') return null;
  const providers = Array.isArray((value as ToolSettings).providers) ? (value as ToolSettings).providers : [];
  return { enabled: (value as ToolSettings).enabled, providers };
}

export default function WebToolsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [saved, setSaved] = useState<ToolSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getWebToolEmulation();
      setSaved(parse(res?.data));
      setNotReady(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (enabled: boolean): Promise<void> => {
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      const res = await api.saveWebToolEmulation(enabled);
      const parsed = parse(res?.data);
      if (parsed) setSaved(parsed);
      else if (saved) setSaved({ ...saved, enabled });
      addToast(t('网页工具调用设置已保存'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  if (loading) {
    return (
      <Card title={t('网页工具调用')}>
        <div className="settings-form">
          <Loader2 size={14} className="confirm-dialog-spin" /> {t('读取中')}
        </div>
      </Card>
    );
  }
  if (notReady) {
    return (
      <Card title={t('网页工具调用')}>
        <div className="empty-state">
          <p>{t('网页工具调用接口尚未就绪')}</p>
        </div>
      </Card>
    );
  }

  return (
    <Card
      title={t('网页工具调用')}
      actions={
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy}>
          <RefreshCw size={13} /> {t('刷新')}
        </Button>
      }
    >
      <div className="settings-form">
        <div className="form-group">
          <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <Switch
              checked={saved?.enabled === true}
              disabled={busy || !saved}
              onChange={(v) => void save(v)}
              ariaLabel={t('为明确声明支持的网页模型启用工具模拟')}
            />
            {t('为明确声明支持的网页模型启用工具模拟')}
          </label>
          <span className="form-hint">
            {t('默认开启，仅作用于插件明确声明的模型。网关通过提示词与响应校验转换工具调用，实际操作仍由客户端执行；原生工具调用和没有工具的普通聊天不受影响。')}
          </span>
        </div>

        <div className="form-group">
          <label>{t('已声明支持工具调用的模型提供方')}</label>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
            {saved?.providers?.length ? (
              saved.providers.map((p) => (
                <Badge key={p.id} tone="neutral">
                  {p.label || p.id}
                </Badge>
              ))
            ) : (
              <span className="form-hint">{t('当前没有声明此能力的插件模型，请更新网页插件并刷新模型目录。')}</span>
            )}
          </div>
        </div>

        <span className="form-hint">
          {t('工具模式下，正文与工具参数在整轮正常结束并校验后下发，思考内容仍可流式返回。工具说明或历史超过网页预算时明确报错，不会静默裁剪；格式错误、缺失参数和强制调用失败不会自动重发。')}
        </span>

        {error ? <div className="error-message">{error}</div> : null}
      </div>
    </Card>
  );
}
