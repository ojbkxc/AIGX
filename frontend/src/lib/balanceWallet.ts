/**
 * 钱包余额读数工具。
 *
 * 对齐 Agent2API v3.5.0-beta.3 ui-shared/usage-readings.ts：`available` 为空时
 * 界面用钱包兜底显示，但钱包里既有「积分池」也有「SOLO 并发 2 路」这类附加
 * 维度 —— 直接取第一个带数字的钱包会把并发数当成余额（实测 Trae 账号显示成
 * 「2 积分」）。`balanceWalletOf` 只返回真正代表余额的读数。
 */

type Wallet = Record<string, unknown>;

function object(value: unknown): Wallet | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Wallet) : null;
}

/** 钱包里只描述运行维度的条目，不是余额：SOLO 并发/套餐状态、快请求每月配额。 */
const NON_BALANCE_WALLET_TYPES = new Set(['solo_parallel', 'solo_package', 'fast_request_per']);

/** 数值读数：数字或数字字符串 → number；空串、非数值 → null。 */
export function usageNumberOrNull(value: unknown): number | null {
  if (typeof value !== 'number' && typeof value !== 'string') return null;
  if (typeof value === 'string' && !value.trim()) return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

/** 钱包当前余额不是套餐发放上限；旧版魔粒钱包没有 snapshot 标记。 */
export function isSnapshotWallet(value: unknown): boolean {
  const wallet = object(value);
  return wallet?.snapshot === true || wallet?.type === 'magicubes';
}

/**
 * 从 `wallets` 里找第一个真正代表余额的读数。
 * 跳过非余额类型（SOLO 并发/套餐、快请求配额）、快照钱包与无 balance 数字的条目。
 */
export function balanceWalletOf(wallets: readonly unknown[]): Wallet | null {
  for (const value of wallets) {
    const wallet = object(value);
    if (!wallet || NON_BALANCE_WALLET_TYPES.has(String(wallet.type || ''))) continue;
    if (isSnapshotWallet(wallet)) continue;
    if (usageNumberOrNull(wallet.balance) !== null) return wallet;
  }
  return null;
}
