/**
 * 域端点（design 1.4：从 `api/index.ts` 的 `api` 对象按域搬出，纯搬移）。
 *
 * 方法原样保留，由 `index.ts` 展开合并进同一个 `api` 对象 —— 调用方仍写
 * `api.<method>()`，导入路径不变。
 */

import type {
  ApiResponse,
  ApiList,
  OrderItem,
  PlanItem,
  UserSubscriptionItem,
  AlertEvent,
} from '@/types';
import { API_BASE, request } from '../client';

export const endpoints = {
  // ==================== 订阅（#85 套餐订阅化，对齐 new-api subscription） ====================
  /** 上架订阅套餐列表（用户侧购买页） */
  subscriptionPlans: (): Promise<ApiResponse<PlanItem[]>> =>
    request<ApiResponse<PlanItem[]>>('GET', `${API_BASE}/subscription/plans`),
  /** 当前用户全部订阅（含过期/取消） */
  subscriptionSelf: (): Promise<ApiResponse<UserSubscriptionItem[]>> =>
    request<ApiResponse<UserSubscriptionItem[]>>('GET', `${API_BASE}/subscription/self`),
  /** 余额购买订阅 */
  subscriptionBalancePay: (plan_id: string): Promise<ApiResponse<UserSubscriptionItem>> =>
    request<ApiResponse<UserSubscriptionItem>>('POST', `${API_BASE}/subscription/balance/pay`, { plan_id }),
  /** 管理员给用户绑定订阅 */
  adminBindSubscription: (user_id: string, plan_id: string): Promise<ApiResponse<UserSubscriptionItem>> =>
    request<ApiResponse<UserSubscriptionItem>>('POST', `${API_BASE}/subscription/admin/bind`, { user_id, plan_id }),
  /** 管理员查看指定用户订阅 */
  adminListUserSubscriptions: (user_id: string): Promise<ApiResponse<UserSubscriptionItem[]>> =>
    request<ApiResponse<UserSubscriptionItem[]>>('GET', `${API_BASE}/subscription/admin/users/${encodeURIComponent(user_id)}/subscriptions`),
  /** 管理员取消订阅并回退分组 */
  adminCancelSubscription: (id: string): Promise<ApiResponse<UserSubscriptionItem>> =>
    request<ApiResponse<UserSubscriptionItem>>('POST', `${API_BASE}/subscription/admin/subscriptions/${encodeURIComponent(id)}/cancel`),
  getSecurityIncidents: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/monitor/security/events`),
  getSecurityAlerts: (): Promise<ApiList<AlertEvent>> =>
    request<ApiList<AlertEvent>>('GET', `${API_BASE}/alerts/active`),
  getBalance: (): Promise<ApiResponse<Record<string, unknown>>> =>
    request<ApiResponse<Record<string, unknown>>>('GET', `${API_BASE}/users/me`),
  getTransactions: (): Promise<ApiList<OrderItem>> =>
    request<ApiList<OrderItem>>('GET', `${API_BASE}/orders/me`),
};
