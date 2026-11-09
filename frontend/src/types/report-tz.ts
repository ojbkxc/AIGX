/**
 * 报表时区设置与存量重算的类型（#20 批3 前端面）。
 *
 * 独立成文件（不并进 `types/index.ts`）：该文件正被并行编辑（#13），
 * 与 `routing-replay.ts` / `routing-profile.ts` / `desktop.ts` 同款隔离理由。
 *
 * 契约来源（逐条对真实产出方，非桩）：
 * - `report_settings_json`（`src/api/admin/settings.rs:626-637`）＝ GET/PUT 响应 data；
 * - `TzMigrateReport` / `BackfillReport`（`src/log/mod.rs:1904-1929`，serde 无 rename_all）；
 * - `handle_agg_tz_migrate`（`src/api/admin/logs.rs:695-752`）＝ 迁移响应外挂的三个驼峰键。
 */

/** GET / PUT `/api/report/settings` 的 `data`（camelCase 手构，六键） */
export interface ReportSettingsItem {
  /** 配置文件里存的值（本次 PUT 想要的口径） */
  timezoneOffset: number;
  /** 进程当前生效值（原子快照，读路径实际用的那个） */
  effectiveOffset: number;
  /** 存量聚合行落盘时的口径；`null` = 老库从没记过口径标记 */
  storedOffset: number | null;
  /** `storedOffset !== effectiveOffset`（老库 `null` 保守为 true）——迁移提示条的唯一判据 */
  needsMigration: boolean;
  /** 值域下界（当前 -12）——表单校验边界，别自己抄常量 */
  minOffset: number;
  /** 值域上界（当前 14） */
  maxOffset: number;
}

/** 重算内核报告（`TzMigrateReport`） */
export interface TzMigrateReportData {
  /** 新口径生效起始报表日（含）；空库 = 今天 */
  effective_from: string;
  /** 删除窗上界 = 昨天（回填/迁移都不覆盖今天） */
  window_to: string;
  /** 是否真的执行了删除（false = 窗口空或口径已一致幂等短路） */
  window_applied: boolean;
  /** 删掉的旧口径聚合行数（主档 + 用户行） */
  rows_deleted: number;
  /** 重算统计 */
  backfill: TzMigrateBackfill;
}

/** 回填统计（`BackfillReport`） */
export interface TzMigrateBackfill {
  /** 读入并参与折叠的明细条数（今天行不计） */
  scanned: number;
  /** 写入的历史日主档数 */
  days_written: number;
  /** 写入的用户日行数 */
  user_rows_written: number;
  /** shrink 守卫拒绝覆盖的行数（窗口外残留 ⇒ 需人工核对明细） */
  skipped_shrink: number;
}

/**
 * POST `/api/logs/agg/tz-migrate` 的完整响应：
 * `ApiResponse<TzMigrateReportData>` 之外再挂三个驼峰面板键。
 */
export interface TzMigrateResult {
  success: boolean;
  data: TzMigrateReportData;
  /** 与 `data.effective_from` 同值，面板友好键（直接进成功吐司 / 详情页） */
  tzEffectiveFrom: string;
  /** 今天（不参与重算的那个报表日） */
  today: string;
  /** 四条坑提示（后端中文串，前端原样渲染，**不过 i18n**） */
  notices: string[];
}
