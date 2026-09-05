/**
 * ../api 的 TS 声明桥。
 * 运行时实现仍在 api.js（供 Vite 打包 .jsx 页面）；
 * 本文件让 .tsx 页面获得顶层方法类型而不触发 TS7016。
 */
export interface ApiClient {
  login(email: string, password: string): Promise<any>;
  register(username: string, email: string, password: string): Promise<any>;
  forgotPassword(email: string): Promise<any>;
  logout(): Promise<any>;
  getUsageSummary(): Promise<any>;
  getTodayTokens(): Promise<any>;
  getLimits(): Promise<any>;
  getTrend(): Promise<any>;
  saveEpayConfig(config: any): Promise<any>;
  listGroups(): Promise<any>;
  getIpLists(): Promise<any>;
  addIpWhitelist(ip: string): Promise<any>;
  addIpBlacklist(ip: string): Promise<any>;
  removeIpWhitelist(pattern: string): Promise<any>;
  removeIpBlacklist(pattern: string): Promise<any>;
  listKeys(): Promise<any>;
  getRequestLogs(): Promise<any>;
  getModelMappings(): Promise<any>;
  saveModelMapping(mapping: any): Promise<any>;
  deleteModelMapping(id: string): Promise<any>;
  listOrders(): Promise<any>;
  chatCompletions(payload: any): Promise<any>;
  listPrices(): Promise<any>;
  listRedemptions(): Promise<any>;
  getSecurityIncidents(): Promise<any>;
  getSecurityAlerts(): Promise<any>;
  saveSettings(usage: any, limits: any, notification: any): Promise<any>;
  getBalance(): Promise<any>;
  getTransactions(): Promise<any>;
  [key: string]: any;
}

export const api: ApiClient;
export default api;