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
}
