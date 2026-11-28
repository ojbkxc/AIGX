/**
 * 路由页账号目录（A2A `routing-accounts.tsx` + `useRoutingAccounts` 的 AIGX 落点）。
 *
 * A2A 的路由配置卡直接吃 `workspace.accounts`（账号目录随路由工作区下发）；
 * AIGX 的 `/api/routing*` 没有账号段，账号目录在 `/api/account-sessions`。
 * 本模块把后者按 A2A 的字段语义投影成路由页可用的目录：
 *
 *   id / 名称 / provider(+label) / 启用态 / 在途数 / 并发上限 / 声明模型
 *
 * 同时带上 A2A `AccountSorting` 的对应物：GET/PUT `/api/accounts/sorting`
 * （账号页「排序规则」的同一数据源，路由页改的就是同一份消耗规则）。
 *
 * 共享单例 store：ProfileForm 与 GroupEditor 同屏挂载，各自 use 这个 hook
 * 只拉一次数据（A2A 那边也是页面级一份 workspace），启停/排序写操作后
 * 整份目录重读，所有订阅者一起更新。
 *
 * 降级口径：端点未就绪 / 网络失败 → `error` 有值、目录为空，卡片退化成
 * 「按 id 维护」的旧形态；绝不编造账号名或启用态。
 */
import { useEffect, useMemo, useSyncExternalStore } from 'react';
import { api } from '../../api';
import type {
  AccountProviderItem,
  AccountSessionItem,
  AccountSortRule,
  AccountSortSettings,
} from '../../types';
import type { RouteModelChoice } from './routing-model-picker';

export interface RoutingAccountInfo {
  id: string;
  /** 展示名（缺省 = id；A2A `displayNameOf` 同口径） */
  name: string;
  provider: string;
  providerLabel?: string;
  /** 启用态（configured_status 优先，status 兜底；缺字段视为启用，与账号页一致） */
  enabled: boolean;
  /** 在途连接数（connections 快照优先，列表 in_flight 兜底；都没有 = undefined，不显示） */
  inFlight?: number;
  maxConcurrency?: number;
  /** 账号声明的模型（空/缺 = 未声明，模型筛选时不因此隐藏该账号） */
  models?: string[];
}

interface DirectoryState {
  accounts: RoutingAccountInfo[];
  providers: AccountProviderItem[];
  /** 消耗排序配置表（provider → rules）；null = 还没读到 / 读取失败 */
  sorting: AccountSortSettings | null;
  /** 余额排序状态（A2A `balanceSortStates`：mixedUnits / noData；后端没给 = 空表） */
  balanceSortStates: Record<string, string>;
  loading: boolean
  error: string;
}

let state: DirectoryState = {
  accounts: [],
  providers: [],
  sorting: null,
  balanceSortStates: {},
  loading: false,
  error: '',
};
const listeners = new Set<() => void>();
let subscribers = 0;
let loadingPromise: Promise<void> | null = null;

function setState(next: Partial<DirectoryState>): void {
  state = { ...state, ...next };
  listeners.forEach((listener) => listener());
}

const subscribe = (listener: () => void): (() => void) => {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
};
const getSnapshot = (): DirectoryState => state;

function toAccountInfo(session: AccountSessionItem, connections: Record<string, number> | null): RoutingAccountInfo {
  const configured = session.configured_status || session.status || '';
  const inFlight = connections?.[session.id] ?? session.in_flight;
  return {
    id: session.id,
    name: session.name || session.email || session.id,
    provider: session.provider,
    providerLabel: session.provider_label,
    enabled: configured ? configured === 'enabled' : session.status !== 'disabled',
    inFlight: typeof inFlight === 'number' && Number.isFinite(inFlight) ? inFlight : undefined,
    maxConcurrency: typeof session.max_concurrency === 'number' ? session.max_concurrency : undefined,
    models: Array.isArray(session.models) ? session.models : undefined,
  };
}

/** 排序响应归一：新后端 `{configured:{provider:rules}}`，旧后端扁平表（同账号页口径） */
function sortingMapOf(data: unknown): AccountSortSettings {
  const raw = (data ?? {}) as Record<string, unknown>;
  const source = raw.configured && typeof raw.configured === 'object'
    ? (raw.configured as Record<string, unknown>)
    : raw;
  const out: AccountSortSettings = {};
  for (const [provider, rules] of Object.entries(source)) {
    if (Array.isArray(rules)) out[provider] = rules as AccountSortRule[];
  }
  return out;
}

/** 余额排序状态表（同响应的 balanceSortStates 键，防御性读取） */
function balanceStatesOf(data: unknown): Record<string, string> {
  const raw = (data ?? {}) as Record<string, unknown>;
  const table = raw.balanceSortStates;
  if (!table || typeof table !== 'object') return {};
  const out: Record<string, string> = {};
  for (const [provider, value] of Object.entries(table)) {
    if (typeof value === 'string') out[provider] = value;
  }
  return out;
}

async function loadDirectory(): Promise<void> {
  if (loadingPromise) return loadingPromise;
  loadingPromise = (async () => {
    setState({ loading: true, error: '' });
    try {
      const [sessionsRes, providersRes, sortingRes, connections] = await Promise.all([
        api.listAccountSessions(),
        api.accountProviders().catch(() => null),
        api.getAccountSorting().catch(() => null),
        api.getAccountSessionConnections(),
      ]);
      const accounts = (sessionsRes?.data ?? []).map((item) => toAccountInfo(item, connections));
      setState({
        accounts,
        providers: providersRes?.data ?? [],
        sorting: sortingRes ? sortingMapOf(sortingRes.data) : null,
        balanceSortStates: sortingRes ? balanceStatesOf(sortingRes.data) : {},
        loading: false,
      });
    } catch (err) {
      setState({ loading: false, error: err instanceof Error ? err.message : String(err) });
    } finally {
      loadingPromise = null;
    }
  })();
  return loadingPromise;
}

export interface AccountEnableResult {
  ok: number;
  failed: Array<{ id: string; error: string }>;
}

/**
 * 全局账号启停（A2A `useRoutingAccounts.setEnabled`）：立即保存，不走路由草稿。
 * 优先 POST /api/account-sessions/batch；端点未上线（404/501）时退回逐条 PATCH，
 * 与账号页同一降级路径。
 */
export async function setAccountsEnabled(ids: string[], enabled: boolean): Promise<AccountEnableResult> {
  const targets = state.accounts.filter((account) => ids.includes(account.id) && account.enabled !== enabled);
  if (!targets.length) return { ok: 0, failed: [] };
  const result: AccountEnableResult = { ok: 0, failed: [] };
  const applyLocal = (id: string): void => {
    setState({
      accounts: state.accounts.map((account) => (account.id === id ? { ...account, enabled } : account)),
    });
  };
  try {
    const res = await api.batchAccountSessions({ op: enabled ? 'enable' : 'disable', ids: targets.map((a) => a.id) });
    for (const receipt of res?.data ?? []) {
      if (receipt.ok) { result.ok += 1; applyLocal(receipt.id); }
      else result.failed.push({ id: receipt.id, error: receipt.error || '失败' });
    }
  } catch (err) {
    const status = (err as { status?: number } | null)?.status;
    if (status !== 404 && status !== 501) {
      result.failed = targets.map((account) => ({ id: account.id, error: err instanceof Error ? err.message : String(err) }));
      return result;
    }
    for (const account of targets) {
      try {
        await api.patchAccountSession(account.id, { status: enabled ? 'enabled' : 'disabled' });
        result.ok += 1;
        applyLocal(account.id);
      } catch (failure) {
        result.failed.push({ id: account.id, error: failure instanceof Error ? failure.message : String(failure) });
      }
    }
  }
  // 与账号页同步口径：写后重读，让在途/状态位回到服务端真值
  void loadDirectory();
  return result;
}

/** 保存一家 provider 的消耗规则（A2A `saveGroupSortRules` 同语义），成功则更新本地表 */
export async function saveProviderSortRules(provider: string, rules: AccountSortRule[]): Promise<void> {
  const res = await api.saveAccountSorting(provider, rules);
  const next = sortingMapOf(res?.data);
  setState({ sorting: { ...(state.sorting ?? {}), ...next, [provider]: next[provider] ?? rules } });
}

export interface RoutingAccountDirectory extends DirectoryState {
  /** 模型目录候选（provider + model，按账号声明模型展开、去重） */
  catalog: RouteModelChoice[];
  /** 全部已知模型 id（模型筛选候选；A2A `workspace.models` 的替代数据源） */
  models: string[];
  refresh(): Promise<void>;
  setEnabled(ids: string[], enabled: boolean): Promise<AccountEnableResult>;
  saveSorting(provider: string, rules: AccountSortRule[]): Promise<void>;
}

export function useRoutingAccountDirectory(): RoutingAccountDirectory {
  const snapshot = useSyncExternalStore(subscribe, getSnapshot);
  useEffect(() => {
    subscribers += 1;
    if (!state.accounts.length && !state.error) void loadDirectory();
    return () => { subscribers -= 1; };
  }, []);
  const catalog = useMemo<RouteModelChoice[]>(() => {
    const seen = new Set<string>();
    const out: RouteModelChoice[] = [];
    for (const account of snapshot.accounts) {
      for (const model of account.models ?? []) {
        const id = String(model ?? '').trim();
        if (!id) continue;
        const key = `${account.provider}\u0000${id.toLowerCase()}`;
        if (seen.has(key)) continue;
        seen.add(key);
        out.push({ provider: account.provider, providerLabel: account.providerLabel, model: id });
      }
    }
    return out;
  }, [snapshot.accounts]);
  const models = useMemo(
    () => [...new Set(catalog.map((choice) => choice.model))].sort((a, b) => a.localeCompare(b)),
    [catalog],
  );
  return useMemo(() => ({
    ...snapshot,
    catalog,
    models,
    refresh: loadDirectory,
    setEnabled: setAccountsEnabled,
    saveSorting: saveProviderSortRules,
  }), [snapshot, catalog, models]);
}
