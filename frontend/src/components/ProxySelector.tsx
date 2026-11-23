/**
 * ProxySelector — 账号弹窗里的出口代理选择器。
 *
 * 后端 proxy 字段是单串，用前缀编码区分语义（见 src/proxy_pool/mod.rs）：
 *   ``                直连
 *   `pool:<id>`       引用池条目
 *   `rotate`          轮询池里全部启用条目（逐请求换出口 IP）
 *   `rotate:<id>,...` 轮询指定条目
 *   `socks5://...`    存量裸串（零迁移兼容）
 *
 * 本组件把这些编码翻成下拉 + 多选的可视形态，让账号配置不再需要手拼串。
 * 空串 / 裸串 / 编码串都能正确回显（编辑现有账号时不丢信息）。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import type { ProxyPoolItem } from '../types';

type Mode = 'direct' | 'pool' | 'rotateAll' | 'rotateIds' | 'custom' | 'clash';

/** GET /api/proxy-pool 响应里的 clash 快照（clash_proxy_options）。 */
interface ClashInfo {
  available?: boolean;
  error?: string | null;
  dir?: string | null;
  options?: Array<{ uid: string; name: string; port: number; kind: string; enabled: boolean }>;
}

interface Props {
  /** 当前 proxy 值（编码串）；编辑时从后端来（脱敏的 userinfo 占位） */
  value: string;
  onChange: (next: string) => void;
  /** 编辑态：原值占位提示（后端不回传密码，留空 = 保持） */
  placeholder?: string;
  /** 编辑态：账号 id（「测试出口」用它按账号现配代理探测；新增态无 id 只测当前串） */
  accountId?: string;
}

function detectMode(raw: string): Mode {
  const v = raw.trim();
  if (!v) return 'direct';
  if (v.startsWith('pool:')) return 'pool';
  if (v.startsWith('rotate:')) return 'rotateIds';
  if (v === 'rotate') return 'rotateAll';
  return 'custom';
}

/// 从 `rotate:id1,id2` 提 id 列表
function parseRotateIds(raw: string): string[] {
  const spec = raw.trim().replace(/^rotate:?/, '');
  return spec.split(',').map((s) => s.trim()).filter(Boolean);
}

export default function ProxySelector({ value, onChange, placeholder, accountId }: Props): JSX.Element {
  const { t } = useTranslation();
  const [items, setItems] = useState<ProxyPoolItem[]>([]);
  const [mode, setMode] = useState<Mode>(detectMode(value));
  const [poolId, setPoolId] = useState<string>('');
  const [rotateIds, setRotateIds] = useState<Set<string>>(new Set());
  const [custom, setCustom] = useState<string>('');
  const [clashInfo, setClashInfo] = useState<ClashInfo | null>(null);

  // 首次挂载按现有值回填各模式的状态
  useEffect(() => {
    const m = detectMode(value);
    setMode(m);
    if (m === 'pool') setPoolId(value.trim().slice(5).trim());
    else if (m === 'rotateIds') setRotateIds(new Set(parseRotateIds(value)));
    else if (m === 'custom') setCustom(value);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 拉代理池（轻量，弹窗打开时一次）；顺带取 clash 快照
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await api.listProxyPool();
        if (cancelled) return;
        const r = res as { items?: ProxyPoolItem[]; clash?: ClashInfo };
        setItems(r?.items ?? []);
        setClashInfo(r?.clash ?? null);
      } catch {
        // 池拉失败不阻塞账号表单：退化成自定义串模式
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Clash 出口镜像到池（POST /api/proxy-pool/sync-clash）。成功后刷新池 + clash 快照。
  const [syncing, setSyncing] = useState(false);
  const [syncMsg, setSyncMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const syncClash = async (): Promise<void> => {
    if (syncing) return;
    setSyncing(true);
    setSyncMsg(null);
    try {
      const res = await api.syncClashProxyPool();
      setItems(res?.items ?? []);
      setClashInfo((res as { clash?: ClashInfo })?.clash ?? null);
      const changes = Number(res?.changes ?? 0);
      const syncError = res?.syncError;
      if (syncError) {
        setSyncMsg({ ok: false, text: t('同步失败：{{reason}}', { reason: String(syncError) }) });
      } else {
        setSyncMsg({ ok: true, text: t('已同步 Clash 出口，变更 {{n}} 条', { n: changes }) });
      }
    } catch (err) {
      setSyncMsg({ ok: false, text: t('同步失败：{{reason}}', { reason: err instanceof Error ? err.message : String(err) }) });
    } finally {
      setSyncing(false);
    }
  };

  const emit = (m: Mode, id: string, ids: Set<string>, raw: string) => {
    if (m === 'direct') onChange('');
    else if (m === 'pool' || m === 'clash') onChange(id ? `pool:${id}` : '');
    else if (m === 'rotateAll') onChange('rotate');
    else if (m === 'rotateIds') onChange(`rotate:${[...ids].join(',')}`);
    else onChange(raw);
  };

  const switchMode = (m: Mode) => {
    setMode(m);
    emit(m, poolId, rotateIds, custom);
  };

  const enabledItems = items.filter((it) => it.enabled);

  // 「从系统代理导入」：读 OS 代理快照并拼成自定义代理串（A2A /api/proxies/system）
  const [sysBusy, setSysBusy] = useState(false);
  const [sysError, setSysError] = useState('');

  const importSystemProxy = async (): Promise<void> => {
    if (sysBusy) return;
    setSysBusy(true);
    setSysError('');
    try {
      const res = await api.systemProxy();
      const proxy = res?.data?.proxy;
      if (!proxy) {
        setSysError(t('未检测到系统代理，请手动填写'));
        return;
      }
      const auth = proxy.username
        ? `${encodeURIComponent(proxy.username)}${proxy.password ? `:${encodeURIComponent(proxy.password)}` : ''}@`
        : '';
      const next = `${proxy.protocol}://${auth}${proxy.host}:${proxy.port}`;
      setCustom(next);
      emit('custom', poolId, rotateIds, next);
    } catch (err) {
      setSysError(err instanceof Error ? err.message : String(err));
    } finally {
      setSysBusy(false);
    }
  };

  // 「测试出口」：沿用 A2A test-account，按账号现配代理（或无 id 时按当前串）探测出口
  const [testBusy, setTestBusy] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string } | null>(null);

  const testExit = async (): Promise<void> => {
    if (testBusy) return;
    setTestBusy(true);
    setTestResult(null);
    try {
      const res = await api.testAccountProxy({ account_id: accountId, proxy: value || null });
      const d = res?.data;
      if (d?.success) {
        const bits = [
          d.ip ? `IP ${d.ip}` : '',
          d.duration_ms != null ? `${d.duration_ms}ms` : '',
          d.proxy ? `· ${d.proxy}` : '',
        ].filter(Boolean).join(' ');
        setTestResult({ ok: true, text: t('出口可用：{{detail}}', { detail: bits || t('连通') }) });
      } else {
        setTestResult({ ok: false, text: t('出口不可用：{{reason}}', { reason: d?.error || t('连接失败') }) });
      }
    } catch (err) {
      setTestResult({ ok: false, text: t('出口不可用：{{reason}}', { reason: err instanceof Error ? err.message : String(err) }) });
    } finally {
      setTestBusy(false);
    }
  };

  return (
    <div className="proxy-selector">
      <div className="form-group">
        <label>{t('出口代理')}</label>
        <select
          className="form-input"
          value={mode}
          onChange={(e) => switchMode(e.target.value as Mode)}
        >
          <option value="direct">{t('直连（不走代理）')}</option>
          <option value="pool">{t('引用代理池条目')}</option>
          <option value="clash">{t('Clash Verge 出口（同步进池）')}</option>
          <option value="rotateAll">{t('轮询全部启用条目（换 IP）')}</option>
          <option value="rotateIds">{t('轮询指定条目')}</option>
          <option value="custom">{t('自定义代理串')}</option>
        </select>
      </div>

      {mode === 'pool' && (
        <div className="form-group">
          <label>{t('选择代理')}</label>
          {items.length === 0 ? (
            <div className="form-hint">{t('代理池为空，请先到「网络代理」页添加。')}</div>
          ) : (
            <select
              className="form-input"
              value={poolId}
              onChange={(e) => {
                setPoolId(e.target.value);
                emit('pool', e.target.value, rotateIds, custom);
              }}
            >
              <option value="">{t('— 请选择 —')}</option>
              {items.map((it) => (
                <option key={it.id} value={it.id}>
                  {it.name}（{it.protocol}://{it.host}:{it.port}）{it.enabled ? '' : ` · ${t('已禁用')}`}
                </option>
              ))}
            </select>
          )}
        </div>
      )}

      {mode === 'rotateIds' && (
        <div className="form-group">
          <label>{t('选择参与轮询的代理')}</label>
          {enabledItems.length === 0 ? (
            <div className="form-hint">{t('没有启用的代理，请先到「网络代理」页添加并启用。')}</div>
          ) : (
            <div className="proxy-selector-checks">
              {enabledItems.map((it) => {
                const checked = rotateIds.has(it.id);
                return (
                  <label key={it.id} className="proxy-selector-check">
                    <input
                      type="checkbox"
                      checked={checked}
                      onChange={(e) => {
                        const next = new Set(rotateIds);
                        if (e.target.checked) next.add(it.id);
                        else next.delete(it.id);
                        setRotateIds(next);
                        emit('rotateIds', poolId, next, custom);
                      }}
                    />
                    <span>{it.name}</span>
                    <span className="form-hint">{it.protocol}://{it.host}:{it.port}</span>
                  </label>
                );
              })}
            </div>
          )}
        </div>
      )}

      {mode === 'custom' && (
        <div className="form-group">
          <label>{t('代理串')}</label>
          <input
            className="form-input"
            placeholder={placeholder || 'socks5://127.0.0.1:1080'}
            value={custom}
            onChange={(e) => {
              setCustom(e.target.value);
              emit('custom', poolId, rotateIds, e.target.value);
            }}
          />
          <span className="form-hint">{t('支持 http(s)://、socks5://、socks5h://，可带 userinfo；socks5 会按 socks5h（远端 DNS）处理')}</span>
          {/* A2A `/api/proxies/system`：把 OS 配置的代理一键填进自定义串 */}
          <button
            type="button"
            className="btn btn-secondary btn-sm"
            disabled={sysBusy}
            onClick={() => void importSystemProxy()}
          >
            {sysBusy ? t('读取中…') : t('从系统代理导入')}
          </button>
          {sysError && <span className="form-hint" style={{ color: 'var(--danger, #dc2626)' }}>❌ {sysError}</span>}
        </div>
      )}

      {mode === 'rotateAll' && (
        <div className="form-hint">
          {enabledItems.length > 0
            ? t('当前池里 {{n}} 条启用条目参与轮询', { n: enabledItems.length })
            : t('池里没有启用条目；账号请求会报错，请先去启用至少一条')}
        </div>
      )}

      {/* clash 档（对齐 A2A accounts-proxy-form）：Clash Verge 出口镜像进池后选择。
          同步走 POST /api/proxy-pool/sync-clash（读取 verge.yaml 的混合端口 + 各监听器）。 */}
      {mode === 'clash' && (
        <div className="form-group">
          <label>{t('Clash Verge 出口')}</label>
          <div className="form-hint">
            {clashInfo?.available
              ? t('检测到 Clash Verge 配置（{{dir}}）。点「同步 Clash」把当前出口镜像进池后选择。', { dir: clashInfo.dir || '—' })
              : t('未检测到 Clash Verge 配置（{{reason}}）。安装并运行后点「同步 Clash」。', { reason: clashInfo?.error || t('未知原因') })}
          </div>
          <div className="proxy-test-row">
            <button
              type="button"
              className="btn btn-secondary btn-sm"
              disabled={syncing}
              onClick={() => void syncClash()}
            >
              {syncing ? t('同步中…') : t('同步 Clash')}
            </button>
            {syncMsg && (
              <span className="form-hint" style={{ color: syncMsg.ok ? 'var(--success, #16a34a)' : 'var(--danger, #dc2626)' }}>
                {syncMsg.ok ? '✓ ' : '✗ '}{syncMsg.text}
              </span>
            )}
          </div>
          {(() => {
            const clashItems = items.filter((it) => it.source === 'clash');
            if (clashItems.length === 0) {
              return <div className="form-hint">{t('池里还没有 Clash 出口，请先点上方「同步 Clash」。')}</div>;
            }
            return (
              <select
                className="form-input"
                value={poolId}
                onChange={(e) => {
                  setPoolId(e.target.value);
                  emit('clash', e.target.value, rotateIds, custom);
                }}
              >
                <option value="">{t('— 请选择 Clash 出口 —')}</option>
                {clashItems.map((it) => (
                  <option key={it.id} value={it.id}>
                    {it.name}（{it.protocol}://{it.host}:{it.port}）{it.enabled ? '' : ` · ${t('已禁用')}`}
                  </option>
                ))}
              </select>
            );
          })()}
        </div>
      )}

      {/* P2「测试出口」：按当前配置探测代理是否连通（A2A accounts-proxy-form test-account）。 */}
      <div className="form-group proxy-test-row">
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          disabled={testBusy}
          onClick={() => void testExit()}
        >
          {testBusy ? t('测试中…') : t('测试出口')}
        </button>
        {testResult && (
          <span className="form-hint" style={{ color: testResult.ok ? 'var(--success, #16a34a)' : 'var(--danger, #dc2626)' }}>
            {testResult.ok ? '✓ ' : '✗ '}{testResult.text}
          </span>
        )}
      </div>
    </div>
  );
}