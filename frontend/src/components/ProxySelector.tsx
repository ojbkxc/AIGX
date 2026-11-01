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

type Mode = 'direct' | 'pool' | 'rotateAll' | 'rotateIds' | 'custom';

interface Props {
  /** 当前 proxy 值（编码串）；编辑时从后端来（脱敏的 userinfo 占位） */
  value: string;
  onChange: (next: string) => void;
  /** 编辑态：原值占位提示（后端不回传密码，留空 = 保持） */
  placeholder?: string;
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

export default function ProxySelector({ value, onChange, placeholder }: Props): JSX.Element {
  const { t } = useTranslation();
  const [items, setItems] = useState<ProxyPoolItem[]>([]);
  const [mode, setMode] = useState<Mode>(detectMode(value));
  const [poolId, setPoolId] = useState<string>('');
  const [rotateIds, setRotateIds] = useState<Set<string>>(new Set());
  const [custom, setCustom] = useState<string>('');

  // 首次挂载按现有值回填各模式的状态
  useEffect(() => {
    const m = detectMode(value);
    setMode(m);
    if (m === 'pool') setPoolId(value.trim().slice(5).trim());
    else if (m === 'rotateIds') setRotateIds(new Set(parseRotateIds(value)));
    else if (m === 'custom') setCustom(value);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 拉代理池（轻量，弹窗打开时一次）
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const res = await api.listProxyPool();
        if (!cancelled) setItems((res as { items?: ProxyPoolItem[] })?.items ?? []);
      } catch {
        // 池拉失败不阻塞账号表单：退化成自定义串模式
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const emit = (m: Mode, id: string, ids: Set<string>, raw: string) => {
    if (m === 'direct') onChange('');
    else if (m === 'pool') onChange(id ? `pool:${id}` : '');
    else if (m === 'rotateAll') onChange('rotate');
    else if (m === 'rotateIds') onChange(`rotate:${[...ids].join(',')}`);
    else onChange(raw);
  };

  const switchMode = (m: Mode) => {
    setMode(m);
    emit(m, poolId, rotateIds, custom);
  };

  const enabledItems = items.filter((it) => it.enabled);

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
          <span className="form-hint">{t('支持 http(s):// 与 socks5://，可带 userinfo')}</span>
        </div>
      )}

      {mode === 'rotateAll' && (
        <div className="form-hint">
          {enabledItems.length > 0
            ? t('当前池里 {{n}} 条启用条目参与轮询', { n: enabledItems.length })
            : t('池里没有启用条目；账号请求会报错，请先去启用至少一条')}
        </div>
      )}
    </div>
  );
}