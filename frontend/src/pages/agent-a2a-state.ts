/**
 * A2A（Agent2API v3.5.1）Agent 页状态层。
 *
 * 对齐 `agent-state.ts` 的职责划分：store 形状、筛选/搜索、计数、选中与重载。
 *
 * AIGX 后端**暂无**「本机客户端检测」接口（后端只有 /api/agent/* 的运维会话接口），
 * 这里按 A2A 的响应形状留一个数据入口 `loadAgentPayload`：后端就绪时替换该实现即可，
 * 页面结构与字段顺序无需改动。接口缺位期间保持空列表 + 空态，不伪造数据。
 */

import { A2A_TEXT } from './agent-a2a-types';
import type { A2aCatalogModel } from './agent-a2a-models';
import type { A2aClient, A2aFilterKey } from './agent-a2a-types';

/** A2A AgentStore（对齐 agent-state.ts 的 AgentStore） */
export type A2aStore = {
  clients: A2aClient[];
  /** 网关模型目录（A2A getAgentModels；AIGX 后端暂无该接口，恒为空） */
  catalog: A2aCatalogModel[];
  /** 网关基址，AIGX 暂无数据源，占位为空串 */
  gateway: string;
  defaultModel: string;
  requiresKey: boolean;
  gatewayKeys: Array<{ id: string; name: string }>;
  detectedAt: number;
  hostName: string;
  loading: boolean;
  refreshing: boolean;
  error: string;
  selected: string;
  filter: A2aFilterKey;
  query: string;
  busy: string;
};

const AUTO_REFRESH_COOLDOWN_MS = 15_000;

/**
 * A2A 客户端清单数据入口。
 *
 * AIGX 后端缺「本机客户端检测」接口：当前返回空负载，页面按 A2A 空态渲染。
 * 后端补 `/api/agent/clients` 后在此处接入即可。
 */
export async function loadAgentPayload(): Promise<{
  clients: A2aClient[];
  gateway: { baseUrl: string; requiresKey: boolean; keys: Array<{ id: string; name: string }>; defaultModel?: string };
  detectedAt: number;
  host: { name: string };
}> {
  return {
    clients: [],
    gateway: { baseUrl: '', requiresKey: false, keys: [] },
    detectedAt: 0,
    host: { name: '' },
  };
}

let state: A2aStore = {
  clients: [],
  catalog: [],
  gateway: '',
  defaultModel: '',
  requiresKey: false,
  gatewayKeys: [],
  detectedAt: 0,
  hostName: '',
  loading: false,
  refreshing: false,
  error: '',
  selected: '',
  filter: 'all',
  query: '',
  busy: '',
};

const listeners = new Set<() => void>();

export function getA2aStore(): A2aStore {
  return state;
}

export function subscribeA2aStore(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function patch(values: Partial<A2aStore>): void {
  state = { ...state, ...values };
  for (const listener of listeners) {
    listener();
  }
}

function syncVisibleSelection(next: A2aStore): string {
  const list = visibleClients(next);
  if (!list.length) return next.selected;
  return list.some((client) => client.id === next.selected) ? next.selected : list[0].id;
}

export function selectedClient(): A2aClient | undefined {
  return state.clients.find((client) => client.id === state.selected);
}

export function selectClient(id: string): void {
  patch({ selected: id });
}

export function setFilter(filter: A2aFilterKey): void {
  const selected = syncVisibleSelection({ ...state, filter });
  patch({ filter, selected });
}

export function setQuery(query: string): void {
  const selected = syncVisibleSelection({ ...state, query });
  patch({ query, selected });
}

export function clearFilters(): void {
  const selected = syncVisibleSelection({ ...state, filter: 'all', query: '' });
  patch({ filter: 'all', query: '', selected });
}

/** A2A 的筛选口径：via=接入本网关；direct=未接入；missing=未检测到 */
export function visibleClients(store: A2aStore = state): A2aClient[] {
  const query = store.query.trim().toLowerCase();
  return store.clients.filter((client) => {
    if (store.filter === 'via' && !client.viaGateway) return false;
    if (store.filter === 'direct' && (!client.detected || client.viaGateway)) return false;
    if (store.filter === 'missing' && client.detected) return false;
    return !query || `${client.name} ${client.vendor} ${client.configPath}`.toLowerCase().includes(query);
  });
}

export function counts(store: A2aStore = state) {
  const detected = store.clients.filter((client) => client.detected);
  const via = detected.filter((client) => client.viaGateway).length;
  return {
    all: store.clients.length,
    detected: detected.length,
    via,
    direct: detected.length - via,
    missing: store.clients.length - detected.length,
  };
}

export type LoadOptions = { silent?: boolean; force?: boolean };
let loadingRequest = 0;
let lastLiveLoadedAt = 0;

export async function loadPanel(options: LoadOptions = {}): Promise<void> {
  const force = Boolean(options.force);
  if (state.refreshing || state.busy) return;
  if (!force && lastLiveLoadedAt > 0 && Date.now() - lastLiveLoadedAt < AUTO_REFRESH_COOLDOWN_MS) return;

  const request = ++loadingRequest;
  const hadClients = state.clients.length > 0;
  patch({ loading: !hadClients, refreshing: true, error: '' });
  try {
    const payload = await loadAgentPayload();
    if (request !== loadingRequest) return;
    const next: A2aStore = {
      ...state,
      clients: Array.isArray(payload.clients) ? payload.clients : [],
      gateway: payload.gateway.baseUrl || '',
      defaultModel: payload.gateway.defaultModel || '',
      requiresKey: payload.gateway.requiresKey,
      gatewayKeys: payload.gateway.keys || [],
      detectedAt: payload.detectedAt || 0,
      hostName: payload.host?.name || '',
      selected: '',
    };
    next.selected = syncVisibleSelection(next);
    lastLiveLoadedAt = Date.now();
    patch(next);
  } catch (error) {
    if (request === loadingRequest) {
      patch({ error: error instanceof Error ? error.message : String(error) });
    }
  } finally {
    if (request === loadingRequest) patch({ loading: false, refreshing: false });
  }
}

/** 格式化「上次检测」时间（A2A 只显示时分） */
export function formatDetectedTime(detectedAt: number): string {
  if (!detectedAt) return '';
  try {
    return new Date(detectedAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  } catch {
    return '';
  }
}

/** 检测状态标签（对齐 agent-model.ts 的 detectionLabel） */
export function detectionLabel(client: A2aClient): string {
  return {
    missing: A2A_TEXT.notDetectedRaw,
    uninitialized: '已安装 · 暂无配置',
    read_error: A2A_TEXT.readingFailed,
    unsupported: '仅支持查看',
    ready: '已检测 · 暂无配置',
  }[client.detectionState] || A2A_TEXT.notDetectedRaw;
}

/** 连接方式标签（对齐 agent-model.ts 的 kindLabel） */
export function kindLabel(value: string): string {
  return {
    gateway: A2A_TEXT.modeGateway,
    official: A2A_TEXT.modeOfficial,
    relay: A2A_TEXT.modeRelay,
    local: A2A_TEXT.modeLocal,
  }[value] || value;
}

/** 客户端协议标签（对齐 agent-model.ts 的 protocolLabel） */
export function protocolLabel(value: string): string {
  return {
    anthropic: 'Anthropic Messages',
    responses: 'OpenAI Responses',
    openai_chat: 'OpenAI Chat Completions',
    gemini: 'Gemini API',
    native: '客户端原生配置',
  }[value] || value;
}

/** 一键接入的阻断原因（对齐 agent-quick-model.ts 的 quickBlockReason） */
export function quickBlockReason(client: A2aClient): string {
  if (!client.supportsGateway) return client.supportNote || '这个客户端不能接入本网关';
  if (!client.writable) return client.readError || client.supportNote;
  return '';
}
