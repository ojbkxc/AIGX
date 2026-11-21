/**
 * RoutingWorkspace —— 模型覆盖对比（对照 A2A `RoutingWorkspace` 画布的
 * 「全局默认 vs 模型独立配置」同屏对比）。
 *
 * 回答一个具体问题：「这个模型的生效路由和全局默认到底差在哪」。
 * 左侧是内置默认（`inheritedProfile`，后端解析），右侧是模型生效档案
 * （`profile`）——**全部取自后端**，界面不自己合并（本仓 #31 教训）。
 *
 * 数据源：GET /api/routing/workspace?model=（be-routing）。
 * 三态降级：端点 404（未就绪）/ 有 workspace 无模型（选模型后自动拉）/
 * 请求失败（错误文案 + 重试）。
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import { Badge, Button, Card, EmptyState } from '../ui';
import { PROFILE_FIELDS, type Layer } from '../../types/routing-profile';
import type { RoutingWorkspaceView } from '../../types/routing-workspace';
import { LayerChip } from './TriStateField';
import { PROFILE_FIELD_LABELS, profileFieldText } from './profileSummary';

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

export default function RoutingWorkspace({ active }: { active: boolean }): JSX.Element {
  const { t } = useTranslation();
  const [model, setModel] = useState('');
  const [ws, setWs] = useState<RoutingWorkspaceView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [notReady, setNotReady] = useState(false);

  const load = useCallback(async (target: string): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getRoutingWorkspace(target.trim() || undefined);
      setWs(res?.data ?? null);
      setNotReady(false);
    } catch (err) {
      const st = statusOf(err);
      if (st === 404 || st === 405) {
        setNotReady(true);
        setWs(null);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (active && !ws && !notReady) void load('');
    // 仅在首次显示时自动拉一次；之后手动刷新（同 RouteSticky 口径）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  const models = useMemo(() => {
    const list = [
      ...(Array.isArray(ws?.models) ? ws?.models ?? [] : []),
      ...(Array.isArray(ws?.configuredModels) ? ws?.configuredModels ?? [] : []),
    ];
    return [...new Set(list.filter((m) => typeof m === 'string' && m.trim()))].sort((a, b) =>
      a.localeCompare(b),
    );
  }, [ws]);

  /** 逐字段生效来源：后端未带 provenance 时显示「来源未知」（不猜） */
  const provenanceOf = (f: (typeof PROFILE_FIELDS)[number]): Layer | null | undefined =>
    ws?.provenance ? ws.provenance[f] : undefined;

  if (notReady) {
    return (
      <Card title={t('模型覆盖对比')}>
        <div className="rt-notice rt-notice-warn" role="status">
          {t('路由工作台端点尚未就绪（后端建设中）。就绪后这里显示全局默认与该模型独立配置的逐字段差异。')}
        </div>
      </Card>
    );
  }

  const profile = ws?.profile ?? null;
  const inherited = ws?.inheritedProfile ?? null;
  const customized = ws?.customized === true;
  const hasModel = Boolean(ws && (ws.model || model.trim()));

  return (
    <Card
      title={t('模型覆盖对比')}
      actions={
        <Button variant="outline" onClick={() => void load(model)} disabled={loading}>
          {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
          {t('刷新')}
        </Button>
      }
    >
      <div className="rt-notice">
        {t('输入模型名（请求名）查看它相对全局默认的逐字段差异。生效值全部取自后端解析结果，界面不自己合并。')}
      </div>
      <div className="form-hint" style={{ marginTop: 4 }}>
        {t('数据源限制：请求日志无账号/会话维度，账号与会话相关列为空属预期；在途请求的 provider/channel 归因可能为空，不代表未选渠道。')}
      </div>

      <div className="rt-ws-toolbar">
        <input
          className="form-input"
          list="rt-ws-models"
          placeholder={t('模型名（请求名），如 gpt-4o')}
          value={model}
          onChange={(e) => setModel(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') void load(model);
          }}
        />
        <datalist id="rt-ws-models">
          {models.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
        <Button variant="outline" onClick={() => void load(model)} disabled={loading}>
          {loading ? <Loader2 size={13} className="spin" /> : null}
          {t('查看')}
        </Button>
        {ws ? (
          <Badge tone={customized ? 'info' : 'neutral'}>
            {customized ? t('有独立配置') : t('跟随全局')}
          </Badge>
        ) : null}
      </div>

      {error ? <div className="error-message">{error}</div> : null}

      {!ws && !loading ? (
        <EmptyState message={t('输入模型名后查看覆盖对比')} />
      ) : ws && !hasModel ? (
        <EmptyState message={t('全局默认视图：输入模型名查看它在哪几个字段上覆盖了全局')} />
      ) : null}

      {ws && profile && inherited ? (
        <div className="rt-table-wrap" style={{ marginTop: 12 }}>
          <table className="rt-table">
            <thead>
              <tr>
                <th>{t('字段')}</th>
                <th>{t('全局默认（该模型继承值）')}</th>
                <th>{t('该模型生效值')}</th>
                <th>{t('来源层')}</th>
              </tr>
            </thead>
            <tbody>
              {PROFILE_FIELDS.map((f) => {
                const left = profileFieldText(inherited, f, t);
                const right = profileFieldText(profile, f, t);
                const same = left === right;
                return (
                  <tr key={f}>
                    <td>{t(PROFILE_FIELD_LABELS[f])}</td>
                    <td className={same ? 'rt-muted' : undefined}>{left}</td>
                    <td className={same ? 'rt-muted' : 'rt-ws-changed'}>{right}</td>
                    <td>
                      <LayerChip layer={provenanceOf(f)} />
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : ws && !profile ? (
        <div className="rt-notice rt-notice-warn" style={{ marginTop: 12 }}>
          {t('后端未返回该模型的生效档案（workspace.profile 缺失），无法对比。')}
        </div>
      ) : null}

      {ws?.queue?.ordered?.length ? (
        <div style={{ marginTop: 12 }}>
          <div className="rt-rail-title">{t('当前候选（队首）')}</div>
          <ul style={{ margin: '6px 0', paddingLeft: 18 }}>
            {ws.queue.ordered.slice(0, 5).map((c, i) => (
              <li key={c.id ?? i}>
                {`${i + 1}. ${c.name ?? c.id ?? ''}`}
                <span className="rt-muted">{` · ${c.provider ?? ''} · ${c.state ?? ''}`}</span>
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {ws ? (
        <div className="form-hint" style={{ marginTop: 10 }}>
          {ws.revisionSupported === false || !ws.revision
            ? t('该端点不提供修订号（路由写入口为 PUT /api/routing，非 CAS）')
            : t('工作台修订号：{{rev}}', { rev: ws.revision })}
          {ws.profilesEnabled === false ? ` · ${t('多规则档案当前关闭，以下为档案关闭时的生效值')}` : ''}
          {ws.stickyCount != null ? ` · ${t('粘滞会话 {{n}} 条', { n: ws.stickyCount })}` : ''}
        </div>
      ) : null}
    </Card>
  );
}