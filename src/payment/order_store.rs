//! 订单存储 — 基于 FileStore 的 TopUpOrder 持久化。

use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;
use super::TopUpOrder;

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
        self.store.put(&format!("order:{}", order.trade_no), order)?;
        self.by_no.write().insert(order.trade_no.clone(), order.clone());
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
