//! 签到台账 + 定时计划 + 设置（KV 持久化）。
//!
//! ## 为什么台账与计划分开存
//!
//! - 台账（`checkin:rec:{ts}:{account_id}`）只追加，按时间倒序读，永不改写。
//! - 计划（`checkin:schedules`）是配置，整表读写。
//!
//! 两者的写频率与读模式完全不同，混在一张表里会让「追加一条记录」也要
//! 重写全部计划。
//!
//! ## 台账为什么不用 FileStore 前缀扫描做分页
//!
//! 记录量按「账号数 × 天数」增长（几十个账号一年约几万条）。前缀扫描会
//! 把全部键名读进内存再排序；这里改为**按日期分桶**（`checkin:rec:{YYYY-MM-DD}`
//! 存当天数组），查询时只读需要的日期桶 —— 与 Agent2API 的 `checkin_records`
//! 按 ts 索引同一目的，但不必引入新表。

use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 单次签到结果的分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckinOutcome {
    /// 真领到了
    Success,
    /// 今天已经领过（上游幂等返回 / 活动已领取）
    Already,
    /// 窗口未开、暂不可领 —— 不算失败，稍后重试
    Pending,
    /// 失败（登录态失效 / 上游报错）
    Failed,
    /// 需要人机验证（验证码令牌缺失，或被上游拒绝需重新求解）—— 不是账号
    /// 失效，前端据此提示管理员去求解验证码（见 `checkin-frontend`）。
    Captcha,
}

impl CheckinOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Already => "already",
            Self::Pending => "pending",
            Self::Failed => "failed",
            Self::Captcha => "captcha",
        }
    }

    /// 台账/统计里的 `ok` 列：success 与 already 都算「今天完成了」。
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Success | Self::Already)
    }
}

/// 一条签到记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckinRecord {
    /// unix 秒
    pub ts: i64,
    /// 日期（YYYY-MM-DD，UTC+8）
    pub date: String,
    pub account_id: String,
    pub account_name: String,
    pub provider: String,
    /// 触发来源：schedule / manual
    pub source: String,
    /// 记录类型：manual | schedule | makeup | retry —— 与 `source` **同值双写**
    /// （旧读端只认 `source`，新读端用 `kind`），存量记录缺省为空串。
    #[serde(default)]
    pub kind: String,
    pub outcome: CheckinOutcome,
    pub ok: bool,
    /// 本次执行链是否附带保活动作（签到后自动链对国际版账号顺带保活时置真）
    #[serde(default)]
    pub kept_active: bool,
    pub message: String,
    /// 失败原文（成功 / 今日已领时为空）—— 供台账「失败原因」列直读。
    #[serde(default)]
    pub error: String,
}

/// 一个定时签到条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckinSchedule {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    /// 每天执行的时刻（HH:MM，按配置时区）
    pub time: String,
    /// 窗口结束时刻（HH:MM）；空 = 无窗口，仅按 `time` 触发。
    /// `randomize=true` 时，每个账号在 `[time, time_end]` 内取确定性随机时刻。
    #[serde(default)]
    pub time_end: String,
    /// true = 每个账号在 `[time, time_end]` 内取**确定性**随机时刻
    /// （`hash(account_id) % 窗口分钟数`，同账号同日同刻、重启不漂）。
    #[serde(default)]
    pub randomize: bool,
    /// 每账号每天重试次数上限（0 = 关闭重试；缺省 3，与旧 `RETRY_LIMIT` 一致）。
    #[serde(default = "default_retry_limit")]
    pub retry_limit: u32,
    /// 重试间隔（分钟，缺省 5；<=0 时回落全局 `retry_gap_ms`）。
    #[serde(default = "default_retry_gap_minutes")]
    pub retry_gap_minutes: u32,
    /// 补签轮时刻（HH:MM）；空 = 不跑补签轮。到点时挑当天该条目下未签上的
    /// 账号再跑一次（`kind="makeup"`），每天至多一轮。
    #[serde(default)]
    pub makeup_time: String,
    /// 参与签到的 provider（空 = 全部支持的）
    #[serde(default)]
    pub providers: Vec<String>,
    /// 逐家时刻覆盖（`{provider: "HH:MM"}`；空串 = 撤销覆盖回落内置/条目时刻）。
    /// 三档取值：条目覆盖 > 内置专属（qoder 10:01 / zcode 19:01）> 条目 time。
    /// 存量配置零迁移：没覆盖的家照旧按条目时刻跑。
    #[serde(default)]
    pub provider_times: HashMap<String, String>,
    /// 上次触发日期（YYYY-MM-DD，当天去重用；存量兼容字段）
    #[serde(default)]
    pub last_fired_date: String,
    /// 逐家已跑标记（`{provider: date}`）：同一条条目可在 00:01 / 10:01 /
    /// 19:01 各跑一批。为空 = 升级前的旧条目，回退用 `last_fired_date` 判定
    /// （否则升级当天会把已签过的家全部重签一遍）。
    #[serde(default)]
    pub last_fired_providers: HashMap<String, String>,
    /// 上次结果摘要
    #[serde(default)]
    pub last_result: String,
    /// 上次执行计数（对齐 Agent2API 的「成功 n·保活 n·失败 n」）
    #[serde(default)]
    pub last_ok: usize,
    #[serde(default)]
    pub last_keepalive: usize,
    #[serde(default)]
    pub last_failed: usize,
}

/// 一次计划执行的计数汇总（界面「成功 n·保活 n·失败 n」）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScheduleCounts {
    pub ok: usize,
    pub keepalive: usize,
    pub failed: usize,
}

/// 签到总设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckinSettings {
    /// 定时签到总开关
    #[serde(default)]
    pub enabled: bool,
    /// 每个账号串行执行之间的间隔（毫秒，防风控）
    #[serde(default = "default_gap_ms")]
    pub gap_ms: u64,
    /// 台账保留天数（0 = 永久保留，不自动 prune）
    #[serde(default)]
    pub keep_days: i64,
    /// 签到时区偏移（小时，默认 8 = UTC+8）。0 = 使用系统本地时区。
    #[serde(default = "default_tz_offset")]
    pub timezone_offset: i32,
    /// 失败/窗口未开的重试间隔（毫秒，0 = 关掉重试；缺省 10 分钟，
    /// 对齐 A2A `RETRY_GAP_MS`）。
    #[serde(default = "default_retry_gap_ms")]
    pub retry_gap_ms: u64,
}

fn default_tz_offset() -> i32 {
    8
}

fn default_gap_ms() -> u64 {
    800
}

fn default_retry_gap_ms() -> u64 {
    10 * 60 * 1000
}

/// 条目缺省重试上限（与旧调度常量 `RETRY_LIMIT` 一致）。
fn default_retry_limit() -> u32 {
    3
}

/// 条目缺省重试间隔（分钟）。
fn default_retry_gap_minutes() -> u32 {
    5
}

impl Default for CheckinSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            gap_ms: default_gap_ms(),
            keep_days: 0,
            timezone_offset: default_tz_offset(),
            retry_gap_ms: default_retry_gap_ms(),
        }
    }
}

/// 设置 + 计划合并存一条（配置量小，整表读写足够）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ScheduleFile {
    #[serde(default)]
    settings: CheckinSettings,
    #[serde(default)]
    schedules: Vec<CheckinSchedule>,
}

const SCHEDULE_KEY: &str = "checkin:schedules";
/// 台账按日分桶的键前缀
const RECORD_PREFIX: &str = "checkin:rec:";
/// 台账最多保留的天数（按日桶删除）
const KEEP_DAYS: i64 = 90;

/// 签到存储。
pub struct CheckinStore {
    store: Arc<FileStore>,
    schedules: RwLock<ScheduleFile>,
}

impl CheckinStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            schedules: RwLock::new(ScheduleFile::default()),
        }
    }

    pub fn load(&self) -> Result<()> {
        if let Some(file) = self.store.get::<ScheduleFile>(SCHEDULE_KEY)? {
            *self.schedules.write() = file;
        }
        Ok(())
    }

    /// 底层 FileStore 句柄。
    ///
    /// 调度器跑国际版保活链要读 `checkin_keepalive` 模型链配置、写
    /// `benefit:keepalive:*` 保活记忆 —— 两者都落在这个库里。
    pub fn file_store(&self) -> &Arc<FileStore> {
        &self.store
    }

    fn persist_schedules(&self) -> Result<()> {
        // 先 clone 再释放读锁，避免读守卫存活于整个 put（锁内同步 IO）
        let snapshot = self.schedules.read().clone();
        self.store.put(SCHEDULE_KEY, &snapshot)
    }

    // ── 设置 ──

    pub fn settings(&self) -> CheckinSettings {
        self.schedules.read().settings.clone()
    }

    pub fn set_settings(&self, settings: CheckinSettings) -> Result<CheckinSettings> {
        let tz_offset = settings.timezone_offset;
        {
            let mut guard = self.schedules.write();
            guard.settings = settings;
        }
        // 同步时区偏移到 scheduler 的静态变量
        super::scheduler::set_checkin_tz_offset(tz_offset);
        self.persist_schedules()?;
        Ok(self.settings())
    }

    // ── 计划 ──

    pub fn schedules(&self) -> Vec<CheckinSchedule> {
        self.schedules.read().schedules.clone()
    }

    /// 新增或更新一个计划（按 id 匹配）。返回最新的全部计划。
    pub fn upsert_schedule(&self, mut schedule: CheckinSchedule) -> Result<Vec<CheckinSchedule>> {
        if schedule.id.trim().is_empty() {
            schedule.id = uuid::Uuid::new_v4().to_string();
        }
        {
            let mut guard = self.schedules.write();
            match guard.schedules.iter_mut().find(|s| s.id == schedule.id) {
                Some(existing) => *existing = schedule,
                None => guard.schedules.push(schedule),
            }
        }
        self.persist_schedules()?;
        Ok(self.schedules())
    }

    pub fn remove_schedule(&self, id: &str) -> Result<Vec<CheckinSchedule>> {
        {
            let mut guard = self.schedules.write();
            guard.schedules.retain(|s| s.id != id);
        }
        self.persist_schedules()?;
        Ok(self.schedules())
    }

    /// 记下某计划今天已触发（当天去重，整条维度）。`counts` 落「成功 n·保活 n·
    /// 失败 n」供界面展示。
    pub fn mark_fired(
        &self,
        id: &str,
        date: &str,
        result: &str,
        counts: &ScheduleCounts,
    ) -> Result<()> {
        {
            let mut guard = self.schedules.write();
            if let Some(schedule) = guard.schedules.iter_mut().find(|s| s.id == id) {
                schedule.last_fired_date = date.to_string();
                schedule.last_result = result.to_string();
                schedule.last_ok = counts.ok;
                schedule.last_keepalive = counts.keepalive;
                schedule.last_failed = counts.failed;
            }
        }
        self.persist_schedules()
    }

    /// 记下某计划的某家 provider 今天已跑（逐家去重）。执行前调用——
    /// 中途被杀也不会在重启后重跑整条计划（对齐 A2A「先落标记再执行」）。
    pub fn mark_fired_provider(&self, id: &str, provider: &str, date: &str) -> Result<()> {
        {
            let mut guard = self.schedules.write();
            if let Some(schedule) = guard.schedules.iter_mut().find(|s| s.id == id) {
                schedule
                    .last_fired_providers
                    .insert(provider.to_string(), date.to_string());
                schedule.last_fired_date = date.to_string();
            }
        }
        self.persist_schedules()
    }

    // ── 台账 ──

    fn record_key(date: &str) -> String {
        format!("{RECORD_PREFIX}{date}")
    }

    /// 当前时间（按配置的签到时区）。
    fn now_tz() -> chrono::DateTime<chrono::FixedOffset> {
        chrono::Utc::now().with_timezone(&super::scheduler::beijing())
    }

    /// 追加一条记录（按日分桶）。
    pub fn append_record(&self, record: CheckinRecord) -> Result<()> {
        let key = Self::record_key(&record.date);
        let mut bucket: Vec<CheckinRecord> = self.store.get(&key)?.unwrap_or_default();
        bucket.push(record);
        self.store.put(&key, &bucket)
    }

    /// 读最近 `days` 天的记录，按时间倒序。
    ///
    /// `provider` / `account_id` / `outcome` / `keyword` 为空表示不过滤
    /// （`keyword` 对账号名、账号 id、上游、说明做大小写不敏感包含匹配）。
    pub fn recent_records(
        &self,
        days: u32,
        provider: Option<&str>,
        account_id: Option<&str>,
        outcome: Option<CheckinOutcome>,
        keyword: Option<&str>,
    ) -> Result<Vec<CheckinRecord>> {
        let days = days.clamp(1, KEEP_DAYS as u32) as i64;
        let today = Self::now_tz();
        let keyword = keyword
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty());
        let mut out: Vec<CheckinRecord> = Vec::new();
        for offset in 0..days {
            let date = (today - chrono::Duration::days(offset))
                .format("%Y-%m-%d")
                .to_string();
            let bucket: Vec<CheckinRecord> = self
                .store
                .get(&Self::record_key(&date))?
                .unwrap_or_default();
            out.extend(bucket.into_iter().filter(|r| {
                provider.map(|p| r.provider == p).unwrap_or(true)
                    && account_id.map(|a| r.account_id == a).unwrap_or(true)
                    && outcome.map(|o| r.outcome == o).unwrap_or(true)
                    && keyword.as_ref().is_none_or(|k| {
                        r.account_name.to_lowercase().contains(k)
                            || r.account_id.to_lowercase().contains(k)
                            || r.provider.to_lowercase().contains(k)
                            || r.message.to_lowercase().contains(k)
                    })
            }));
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.ts));
        Ok(out)
    }

    /// 台账总条数（统计卡「累计记录数」）。
    pub fn total_records(&self) -> Result<usize> {
        let mut total = 0usize;
        for key in self.store.list(RECORD_PREFIX)? {
            if let Some(bucket) = self.store.get::<Vec<CheckinRecord>>(&key)? {
                total += bucket.len();
            }
        }
        Ok(total)
    }

    /// 把某账号某 provider 当日最后一条记录标记「附带保活」。
    ///
    /// 台账的设计是「只追加、不改写」，但「本次执行链附带了保活」是元数据
    /// 修正（由签到后的自动链在保活成功后回写），不改变记录的语义，因此
    /// 只改这一个布尔字段。找不到记录时静默成功（保活回写不该让主流程失败）。
    pub fn mark_kept_active(&self, account_id: &str, provider: &str, date: &str) -> Result<()> {
        let key = Self::record_key(date);
        let mut bucket: Vec<CheckinRecord> = self.store.get(&key)?.unwrap_or_default();
        if let Some(last) = bucket
            .iter_mut()
            .rev()
            .find(|r| r.account_id == account_id && r.provider == provider)
        {
            last.kept_active = true;
            self.store.put(&key, &bucket)?;
        }
        Ok(())
    }

    /// 清空台账（`days` 天前的桶一并删除，保留窗口外的不动）。
    pub fn clear_records(&self, days: u32) -> Result<usize> {
        let days = days.clamp(1, KEEP_DAYS as u32) as i64;
        let today = Self::now_tz();
        let mut removed = 0usize;
        for offset in 0..days {
            let date = (today - chrono::Duration::days(offset))
                .format("%Y-%m-%d")
                .to_string();
            let key = Self::record_key(&date);
            if let Some(bucket) = self.store.get::<Vec<CheckinRecord>>(&key)? {
                removed += bucket.len();
                self.store.delete(&key)?;
            }
        }
        Ok(removed)
    }

    /// 清空全部台账记录（删除所有日桶）。
    ///
    /// 对应基线 `DELETE /api/checkin-records?all=1` 的显式全清。
    /// 调用方必须在端点层做 `all=1` 护栏校验，本函数不做二次检查。
    pub fn clear_all(&self) -> Result<usize> {
        let mut removed = 0usize;
        for key in self.store.list(RECORD_PREFIX)? {
            if let Some(bucket) = self.store.get::<Vec<CheckinRecord>>(&key)? {
                removed += bucket.len();
            }
            self.store.delete(&key)?;
        }
        Ok(removed)
    }

    /// 按筛选条件删除匹配的记录（逐条删除，不删整个日桶）。
    ///
    /// 对应基线 `DELETE /api/checkin-records` 带筛选的删除语义。
    /// 匹配逻辑与 [`recent_records`] 的过滤条件一致。
    /// 空桶（删完后无记录）会被一并删除。
    pub fn remove_matching(
        &self,
        provider: Option<&str>,
        account_id: Option<&str>,
        outcome: Option<CheckinOutcome>,
        keyword: Option<&str>,
    ) -> Result<usize> {
        let keyword = keyword
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty());
        let mut removed = 0usize;
        for key in self.store.list(RECORD_PREFIX)? {
            let Some(bucket) = self.store.get::<Vec<CheckinRecord>>(&key)? else {
                continue;
            };
            let bucket_len = bucket.len();
            let kept: Vec<CheckinRecord> = bucket
                .into_iter()
                .filter(|r| {
                    !(provider.map(|p| r.provider == p).unwrap_or(false)
                        && account_id.map(|a| r.account_id == a).unwrap_or(false)
                        && outcome.map(|o| r.outcome == o).unwrap_or(false)
                        && keyword.as_ref().is_none_or(|k| {
                            r.account_name.to_lowercase().contains(k)
                                || r.account_id.to_lowercase().contains(k)
                                || r.provider.to_lowercase().contains(k)
                                || r.message.to_lowercase().contains(k)
                        }))
                })
                .collect();
            removed += bucket_len.saturating_sub(kept.len());
            if kept.is_empty() {
                self.store.delete(&key)?;
            } else {
                self.store.put(&key, &kept)?;
            }
        }
        Ok(removed)
    }

    /// 删掉超过保留期的日桶。
    ///
    /// 台账只在 `append_record` 里写入，没有别的地方会删旧桶；不清理的话
    /// 存储会随天数无限增长。调度每轮顺手调一次（30 秒一次，代价只是列一次
    /// 键前缀），保证长时间运行也不膨胀。
    ///
    /// `keep_days = 0`（默认）= 永久保留，不 prune。
    pub fn prune_records(&self) -> Result<usize> {
        let keep_days = self.settings().keep_days;
        if keep_days <= 0 {
            return Ok(0);
        }
        let today = Self::now_tz();
        let cutoff = (today - chrono::Duration::days(keep_days))
            .format("%Y-%m-%d")
            .to_string();
        let mut removed = 0usize;
        for key in self.store.list(RECORD_PREFIX)? {
            // 键形如 `checkin:rec:YYYY-MM-DD`，日期是最后 10 个字符
            let date = key.rsplit(':').next().unwrap_or("");
            if date.len() == 10 && date < cutoff.as_str() {
                self.store.delete(&key)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// 今天某账号某 provider 的最后一条记录（用于「今日是否已签」）。
    pub fn last_today(&self, account_id: &str, provider: &str) -> Result<Option<CheckinRecord>> {
        let records = self.recent_records(1, Some(provider), Some(account_id), None, None)?;
        Ok(records.into_iter().next())
    }

    /// 台账按 provider 汇总（今日）：`provider → (完成数, 总数)`。
    pub fn today_summary(&self) -> Result<HashMap<String, (usize, usize)>> {
        let records = self.recent_records(1, None, None, None, None)?;
        let mut map: HashMap<String, (usize, usize)> = HashMap::new();
        for r in records {
            let entry = map.entry(r.provider.clone()).or_insert((0, 0));
            entry.1 += 1;
            if r.ok {
                entry.0 += 1;
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> Arc<FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-checkin-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(FileStore::new(dir))
    }

    fn today() -> String {
        chrono::Utc::now()
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .format("%Y-%m-%d")
            .to_string()
    }

    #[test]
    fn schedule_upsert_and_remove() {
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        assert!(store.schedules().is_empty());

        let list = store
            .upsert_schedule(CheckinSchedule {
                id: String::new(),
                label: "每日".into(),
                enabled: true,
                time: "09:00".into(),
                time_end: String::new(),
                randomize: false,
                retry_limit: default_retry_limit(),
                retry_gap_minutes: default_retry_gap_minutes(),
                makeup_time: String::new(),
                providers: vec!["loomy".into()],
                provider_times: HashMap::new(),
                last_fired_date: String::new(),
                last_fired_providers: HashMap::new(),
                last_result: String::new(),
                last_ok: 0,
                last_keepalive: 0,
                last_failed: 0,
            })
            .unwrap();
        assert_eq!(list.len(), 1);
        let id = list[0].id.clone();
        assert!(!id.is_empty());

        // 同 id 更新而不是新增
        let mut updated = list[0].clone();
        updated.time = "10:30".into();
        let list = store.upsert_schedule(updated).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].time, "10:30");

        store.remove_schedule(&id).unwrap();
        assert!(store.schedules().is_empty());
    }

    #[test]
    fn records_append_filter_and_clear() {
        let store = CheckinStore::new(temp_store());
        let date = today();
        for (provider, ok) in [("loomy", true), ("kuku", false), ("loomy", true)] {
            store
                .append_record(CheckinRecord {
                    ts: chrono::Utc::now().timestamp(),
                    date: date.clone(),
                    account_id: "a1".into(),
                    account_name: "账号 1".into(),
                    provider: provider.into(),
                    source: "manual".into(),
                    kind: "manual".into(),
                    outcome: if ok {
                        CheckinOutcome::Success
                    } else {
                        CheckinOutcome::Failed
                    },
                    ok,
                    kept_active: false,
                    message: String::new(),
                    error: String::new(),
                })
                .unwrap();
        }
        assert_eq!(
            store
                .recent_records(1, None, None, None, None)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            store
                .recent_records(1, Some("loomy"), None, None, None)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .recent_records(1, None, None, Some(CheckinOutcome::Failed), None)
                .unwrap()
                .len(),
            1
        );
        // keyword 大小写不敏感、命中账号名 / 说明
        assert_eq!(
            store
                .recent_records(1, None, None, None, Some("账号 1"))
                .unwrap()
                .len(),
            3
        );
        assert!(store
            .recent_records(1, None, None, None, Some("x-nomatch"))
            .unwrap()
            .is_empty());
        assert_eq!(store.clear_records(1).unwrap(), 3);
        assert!(store
            .recent_records(1, None, None, None, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn outcome_ok_semantics() {
        assert!(CheckinOutcome::Success.is_ok());
        assert!(CheckinOutcome::Already.is_ok());
        assert!(!CheckinOutcome::Pending.is_ok());
        assert!(!CheckinOutcome::Failed.is_ok());
        // Captcha 不是「今天完成了」——前端要提示去求解验证码
        assert!(!CheckinOutcome::Captcha.is_ok());
        assert_eq!(CheckinOutcome::Captcha.as_str(), "captcha");
    }

    #[test]
    fn captcha_outcome_serde_roundtrip() {
        // 台账持久化：写 "captcha" → 读回仍是 Captcha（别落成 Failed）
        let json = serde_json::to_string(&CheckinOutcome::Captcha).unwrap();
        assert_eq!(json, "\"captcha\"");
        let back: CheckinOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CheckinOutcome::Captcha);
    }

    #[test]
    fn record_kind_and_error_serde_default_and_double_write() {
        // 存量记录没有 kind/error 两个键 → 反序列化回落空串（零迁移）
        let legacy: CheckinRecord = serde_json::from_str(
            r#"{"ts":1,"date":"2026-01-01","account_id":"a","account_name":"n",
                "provider":"loomy","source":"schedule","outcome":"success","ok":true,
                "message":"ok"}"#,
        )
        .unwrap();
        assert_eq!(legacy.kind, "");
        assert_eq!(legacy.error, "");
        // 新写：kind 与 source 同值双写，error 记失败原文
        let rec = CheckinRecord {
            ts: 1,
            date: "2026-01-01".into(),
            account_id: "a".into(),
            account_name: "n".into(),
            provider: "loomy".into(),
            source: "makeup".into(),
            kind: "makeup".into(),
            outcome: CheckinOutcome::Captcha,
            ok: false,
            kept_active: false,
            message: "需要验证码".into(),
            error: "需要验证码".into(),
        };
        assert_eq!(rec.kind, rec.source);
    }
}
