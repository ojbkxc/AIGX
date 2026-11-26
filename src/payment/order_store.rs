//! 订单存储 — 基于 FileStore 的 TopUpOrder 持久化。

use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use super::TopUpOrder;
use crate::storage::FileStore;

pub struct OrderStore {
    store: Arc<FileStore>,
    by_no: RwLock<HashMap<String, TopUpOrder>>,
}

impl OrderStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_no: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> Result<()> {
        let keys = self.store.list("order:")?;
        let mut by_no = self.by_no.write();
        by_no.clear();
        for key in &keys {
            if let Some(o) = self.store.get::<TopUpOrder>(key)? {
                by_no.insert(o.trade_no.clone(), o);
            }
        }
        Ok(())
    }

    pub fn insert(&self, order: &TopUpOrder) -> Result<()> {
        self.store
            .put(&format!("order:{}", order.trade_no), order)?;
        self.by_no
            .write()
            .insert(order.trade_no.clone(), order.clone());
        Ok(())
    }

    pub fn get(&self, trade_no: &str) -> Option<TopUpOrder> {
        self.by_no.read().get(trade_no).cloned()
    }

    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn complete(&self, trade_no: &str) -> Result<Option<TopUpOrder>> {
        let mut by_no = self.by_no.write();
        let order = match by_no.get_mut(trade_no) {
            Some(o) => o,
            None => return Ok(None),
        };
        if order.status != "pending" {
            return Ok(Some(order.clone()));
        }
        order.status = "paid".into();
        order.paid_time = Some(chrono::Utc::now().timestamp());
        let snapshot = order.clone();
        drop(by_no);
        self.store.put(&format!("order:{trade_no}"), &snapshot)?;
        Ok(Some(snapshot))
    }

    /// 原子完成订单（CAS：仅 pending → paid 转换成功才返回订单）。
    ///
    /// B01 修复：原回调流程为 get(订单) → add_quota → complete 三步非原子，
    /// notify 与 return 并发到达时同一订单会被处理两次（双倍入账）。
    /// 本方法在写锁内完成状态判定与转换：仅当订单存在且当前为 pending 时
    /// 才置为 paid 并持久化，返回订单快照供入账；否则返回 None 表示
    /// 订单已被处理过（或不存在），调用方绝不能再次加款。
    pub fn complete_if_pending(&self, trade_no: &str) -> Option<TopUpOrder> {
        let mut by_no = self.by_no.write();
        let order = by_no.get_mut(trade_no)?;
        if order.status != "pending" {
            // 已处理（幂等）或不存在有效 pending 状态，拒绝二次入账
            return None;
        }
        order.status = "paid".into();
        order.paid_time = Some(chrono::Utc::now().timestamp());
        let snapshot = order.clone();
        drop(by_no);
        if let Err(e) = self.store.put(&format!("order:{trade_no}"), &snapshot) {
            tracing::error!("Failed to persist order {trade_no} completion: {e}");
            // 持久化失败：回退内存状态，避免出现"内存已 paid 但磁盘 pending"
            // 的不一致（下次重启后仍可重新入账，宁可重试不可丢单）
            if let Some(o) = self.by_no.write().get_mut(trade_no) {
                o.status = "pending".into();
                o.paid_time = None;
            }
            return None;
        }
        Some(snapshot)
    }

    /// 手动补单入账失败后回滚：paid → pending（允许重试补单）。
    pub fn reopen(&self, trade_no: &str) -> anyhow::Result<()> {
        let mut by_no = self.by_no.write();
        let order = by_no
            .get_mut(trade_no)
            .ok_or_else(|| anyhow::anyhow!("order not found"))?;
        order.status = "pending".into();
        order.paid_time = None;
        let snapshot = order.clone();
        drop(by_no);
        self.store
            .put(&format!("order:{trade_no}"), &snapshot)
            .map_err(|e| anyhow::anyhow!("persist failed: {e}"))?;
        Ok(())
    }

    /// 删除订单（管理面用）。
    pub fn delete(&self, trade_no: &str) -> bool {
        let removed = self.by_no.write().remove(trade_no).is_some();
        if removed {
            let _ = self.store.delete(&format!("order:{trade_no}"));
        }
        removed
    }

    pub fn list_by_user(&self, user_id: &str) -> Vec<TopUpOrder> {
        let mut list: Vec<TopUpOrder> = self
            .by_no
            .read()
            .values()
            .filter(|o| o.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.create_time));
        list
    }

    pub fn list_all(&self) -> Vec<TopUpOrder> {
        let mut list: Vec<TopUpOrder> = self.by_no.read().values().cloned().collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.create_time));
        list
    }

    /// 分页 + 关键字搜索（管理端全量订单）。
    ///
    /// 对齐 new-api PageInfo：返回 (items, total)；keyword 匹配 trade_no/user_id/支付方式。
    pub fn list_paged(
        &self,
        keyword: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> (Vec<TopUpOrder>, usize) {
        let all = self.list_all();
        let kw = keyword.unwrap_or("").trim().to_lowercase();
        let filtered: Vec<TopUpOrder> = if kw.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|o| {
                    o.trade_no.to_lowercase().contains(&kw)
                        || o.user_id.to_lowercase().contains(&kw)
                        || o.payment_method.to_lowercase().contains(&kw)
                })
                .collect()
        };
        let total = filtered.len();
        let page = page.max(1);
        let page_size = page_size.max(1);
        let start = (page - 1) * page_size;
        let items = if start >= total {
            Vec::new()
        } else {
            let end = (start + page_size).min(total);
            filtered[start..end].to_vec()
        };
        (items, total)
    }

    /// 用户自己的订单分页 + 关键字搜索。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn list_paged_by_user(
        &self,
        user_id: &str,
        keyword: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> (Vec<TopUpOrder>, usize) {
        let mut all = self.list_by_user(user_id);
        let kw = keyword.unwrap_or("").trim().to_lowercase();
        if !kw.is_empty() {
            all.retain(|o| {
                o.trade_no.to_lowercase().contains(&kw)
                    || o.payment_method.to_lowercase().contains(&kw)
            });
        }
        let total = all.len();
        let page = page.max(1);
        let page_size = page_size.max(1);
        let start = (page - 1) * page_size;
        let items = if start >= total {
            Vec::new()
        } else {
            let end = (start + page_size).min(total);
            all[start..end].to_vec()
        };
        (items, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(dir: &TempDir) -> OrderStore {
        OrderStore::new(Arc::new(FileStore::new(dir.path().to_path_buf())))
    }

    fn order(no: &str, user: &str, status: &str, created: i64) -> TopUpOrder {
        TopUpOrder {
            trade_no: no.into(),
            user_id: user.into(),
            amount: 10,
            money: 1.0,
            quota: 100,
            payment_method: "epay".into(),
            status: status.into(),
            create_time: created,
            paid_time: None,
        }
    }

    #[test]
    fn insert_get_and_reload_from_disk() {
        let dir = TempDir::new().unwrap();
        {
            let s = store(&dir);
            s.insert(&order("t1", "u1", "pending", 100)).unwrap();
            assert_eq!(s.get("t1").unwrap().status, "pending");
            assert!(s.get("missing").is_none());
        }
        // 新实例从磁盘重建索引，订单不丢
        let s2 = store(&dir);
        let got = s2.get("t1").expect("persisted order must reload");
        assert_eq!(got.user_id, "u1");
        assert_eq!(got.quota, 100);
    }

    /// CAS 完成：第一次 pending→paid 返回订单，第二次一律 None（防止回调并发双倍入账）。
    #[test]
    fn complete_if_pending_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let s = store(&dir);
        s.insert(&order("t1", "u1", "pending", 100)).unwrap();

        let first = s.complete_if_pending("t1").expect("first must complete");
        assert_eq!(first.status, "paid");
        assert!(first.paid_time.is_some());

        assert!(
            s.complete_if_pending("t1").is_none(),
            "second must be rejected"
        );
        assert!(s.complete_if_pending("nope").is_none(), "missing order");

        // paid 状态已落盘
        let s2 = store(&dir);
        assert_eq!(s2.get("t1").unwrap().status, "paid");
    }

    #[test]
    fn complete_if_pending_rejects_non_pending() {
        let dir = TempDir::new().unwrap();
        let s = store(&dir);
        s.insert(&order("t1", "u1", "expired", 100)).unwrap();
        assert!(s.complete_if_pending("t1").is_none());
        assert_eq!(s.get("t1").unwrap().status, "expired");
    }

    /// 补单失败回滚：reopen 把 paid 拉回 pending，可再次 CAS 成功。
    #[test]
    fn reopen_resets_to_pending_for_retry() {
        let dir = TempDir::new().unwrap();
        let s = store(&dir);
        s.insert(&order("t1", "u1", "pending", 100)).unwrap();
        assert!(s.complete_if_pending("t1").is_some());

        s.reopen("t1").unwrap();
        let after = s.get("t1").unwrap();
        assert_eq!(after.status, "pending");
        assert!(after.paid_time.is_none());
        assert!(s.complete_if_pending("t1").is_some());

        assert!(s.reopen("missing").is_err());
    }

    #[test]
    fn delete_removes_memory_and_disk() {
        let dir = TempDir::new().unwrap();
        let s = store(&dir);
        s.insert(&order("t1", "u1", "pending", 100)).unwrap();
        assert!(s.delete("t1"));
        assert!(s.get("t1").is_none());
        assert!(!s.delete("t1"));
        // 重启后仍不存在
        assert!(store(&dir).get("t1").is_none());
    }

    #[test]
    fn list_paged_filters_and_sorts_desc() {
        let dir = TempDir::new().unwrap();
        let s = store(&dir);
        for (i, (no, user, method)) in [
            ("t1", "u1", "epay"),
            ("t2", "u1", "stripe"),
            ("t3", "u2", "epay"),
            ("t4", "u2", "stripe"),
            ("t5", "u1", "epay"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut o = order(no, user, "pending", (i as i64 + 1) * 10);
            o.payment_method = method.into();
            s.insert(&o).unwrap();
        }

        // 无关键字：按 create_time 倒序
        let (items, total) = s.list_paged(None, 1, 10);
        assert_eq!(total, 5);
        let nos: Vec<_> = items.iter().map(|o| o.trade_no.as_str()).collect();
        assert_eq!(nos, ["t5", "t4", "t3", "t2", "t1"]);

        // 关键字按 user_id 过滤（大小写不敏感）
        let (items, total) = s.list_paged(Some("U1"), 1, 10);
        assert_eq!(total, 3);
        assert!(items.iter().all(|o| o.user_id == "u1"));

        // 分页：size=2
        let (p1, total) = s.list_paged(None, 1, 2);
        assert_eq!((p1.len(), total), (2, 5));
        assert_eq!(p1[0].trade_no, "t5");
        let (p3, _) = s.list_paged(None, 3, 2);
        assert_eq!(p3.len(), 1);
        assert_eq!(p3[0].trade_no, "t1");
        let (p4, _) = s.list_paged(None, 4, 2);
        assert!(p4.is_empty());
    }

    #[test]
    fn list_by_user_returns_only_that_user_desc() {
        let dir = TempDir::new().unwrap();
        let s = store(&dir);
        s.insert(&order("a", "u1", "pending", 10)).unwrap();
        s.insert(&order("b", "u2", "paid", 30)).unwrap();
        s.insert(&order("c", "u1", "paid", 20)).unwrap();

        let list = s.list_by_user("u1");
        let nos: Vec<_> = list.iter().map(|o| o.trade_no.as_str()).collect();
        assert_eq!(nos, ["c", "a"]);
    }
}
