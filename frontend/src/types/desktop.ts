/**
 * 桌面客户端登录态导入类型（#22 desktop 段）。
 *
 * **形状契约锚点**：GET /api/account-sessions/desktop/sources 投影
 * （src/api/admin/desktop_import.rs:165-176 list_sources；白名单
 * DESKTOP_SOURCES :180-210 = autoclaw/box-agent/meituan-catpaw/cline）。
 * POST /api/account-sessions/desktop-import 请求体只有 source_id + name
 * （desktop_import.rs:216-221——**无路径参数**，固定路径白名单只读）；
 * 错误语义：未知来源 400 / 非 Windows 501 / encrypted（DPAPI/safeStorage
 * 上游私有知识）501 / 文件缺失过大 500。
 */

/** 白名单来源投影（supported 判定：cfg!(windows) && !encrypted） */
export interface DesktopSourceItem {
  id: string;
  label: string;
  /** 固定绝对路径（只读展示，请求里不传） */
  path: string;
  /** true = 需要 DPAPI/safeStorage 解密（上游插件私有知识，暂不支持） */
  encrypted: boolean;
  /** 当前部署是否可导入 */
  supported: boolean;
  /** 不可用原因（supported=true 时缺省） */
  unsupportedReason?: string | null;
}
