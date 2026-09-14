//! 工单系统 — 用户提交问题、管理员回复/关闭。
//!
//! 参照 v2board 的工单语义：
//! - 用户只能看/回复自己的工单，关闭后不可回复。
//! - 回复顺序约束：连续两条来自同一方会拒绝（等对方回复）。
//! - `reply_status` 0 = 待客服回复（最后一条来自用户）、1 = 待用户回复（最后一条来自管理员）。
//! - 管理员回复会重新打开已关闭工单（status 归 0）。
//!
//! 持久化使用 FileStore KV：`ticket:{id}` 存工单、`ticket_msg:{id}` 存消息。

use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 工单状态：0 = 待处理（打开），1 = 已关闭
pub const STATUS_OPEN: i32 = 0;
pub const STATUS_CLOSED: i32 = 1;

/// 回复状态：0 = 待客服回复，1 = 待用户回复
pub const REPLY_WAIT_ADMIN: i32 = 0;
pub const REPLY_WAIT_USER: i32 = 1;

/// 工单记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub user_id: String,
    pub subject: String,
    /// 优先级：0 低 / 1 中 / 2 高
    #[serde(default)]
    pub level: i32,
    /// 0 = 打开，1 = 已关闭
    #[serde(default = "default_status")]
    pub status: i32,
    /// 0 = 待客服回复，1 = 待用户回复
    #[serde(default)]
    pub reply_status: i32,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_status() -> i32 {
    STATUS_OPEN
}

/// 工单消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketMessage {
    pub id: String,
    pub ticket_id: String,
    pub user_id: String,
    pub message: String,
    #[serde(default)]
    pub created_at: i64,
}

pub struct TicketStore {
    store: Arc<FileStore>,
    by_id: RwLock<HashMap<String, Ticket>>,
    /// ticket_id -> 消息（按创建时间升序）
    messages: RwLock<HashMap<String, Vec<TicketMessage>>>,
}

impl TicketStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_id: RwLock::new(HashMap::new()),
            messages: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> Result<()> {
        let mut by_id = self.by_id.write();
        let mut messages = self.messages.write();
        by_id.clear();
        messages.clear();
        for key in self.store.list("ticket:")? {
            if let Some(t) = self.store.get::<Ticket>(&key)? {
                by_id.insert(t.id.clone(), t);
            }
        }
        for key in self.store.list("ticket_msg:")? {
            if let Some(m) = self.store.get::<TicketMessage>(&key)? {
                messages.entry(m.ticket_id.clone()).or_default().push(m);
            }
        }
        for list in messages.values_mut() {
            list.sort_by_key(|m| (m.created_at, m.id.clone()));
        }
        Ok(())
    }

    fn persist_ticket(&self, t: &Ticket) -> Result<()> {
        self.store.put(&format!("ticket:{}", t.id), t)
    }

    fn persist_message(&self, m: &TicketMessage) -> Result<()> {
        self.store.put(&format!("ticket_msg:{}", m.id), m)
    }

    /// 创建工单并写入首条消息。返回工单。
    pub fn create(
        &self,
        user_id: &str,
        subject: &str,
        level: i32,
        message: &str,
    ) -> Result<Ticket> {
        let now = chrono::Utc::now().timestamp();
        let ticket = Ticket {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            subject: subject.to_string(),
            level,
            status: STATUS_OPEN,
            reply_status: REPLY_WAIT_ADMIN,
            created_at: now,
            updated_at: now,
        };
        let msg = TicketMessage {
            id: uuid::Uuid::new_v4().to_string(),
            ticket_id: ticket.id.clone(),
            user_id: user_id.to_string(),
            message: message.to_string(),
            created_at: now,
        };
        self.persist_ticket(&ticket)?;
        self.persist_message(&msg)?;
        self.by_id.write().insert(ticket.id.clone(), ticket.clone());
        self.messages
            .write()
            .entry(ticket.id.clone())
            .or_default()
            .push(msg);
        Ok(ticket)
    }

    pub fn get(&self, id: &str) -> Option<Ticket> {
        self.by_id.read().get(id).cloned()
    }

    /// 用户工单列表（按 updated_at 倒序）
    pub fn list_by_user(&self, user_id: &str) -> Vec<Ticket> {
        let mut list: Vec<Ticket> = self
            .by_id
            .read()
            .values()
            .filter(|t| t.user_id == user_id)
            .cloned()
            .collect();
        list.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
        list
    }

    /// 全部工单（按 updated_at 倒序）
    pub fn list_all(&self) -> Vec<Ticket> {
        let mut list: Vec<Ticket> = self.by_id.read().values().cloned().collect();
        list.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
        list
    }

    pub fn messages(&self, ticket_id: &str) -> Vec<TicketMessage> {
        self.messages.read().get(ticket_id).cloned().unwrap_or_default()
    }

    /// 最后一条消息
    fn last_message(&self, ticket_id: &str) -> Option<TicketMessage> {
        self.messages(ticket_id).into_iter().last()
    }

    /// 回复工单。返回工单快照。
    ///
    /// - 已关闭工单不可回复（管理员通过 admin 回复可重新打开）。
    /// - 连续两条来自同一方（last_message.user_id == user_id）拒绝。
    pub fn reply(&self, ticket_id: &str, user_id: &str, message: &str) -> Result<Ticket> {
        let ticket = self
            .get(ticket_id)
            .ok_or_else(|| anyhow::anyhow!("ticket not found"))?;
        if ticket.status == STATUS_CLOSED {
            anyhow::bail!("ticket is closed");
        }
        if let Some(last) = self.last_message(ticket_id) {
            if last.user_id == user_id {
                anyhow::bail!("please wait for the other party to reply");
            }
        }
        self.append_message(ticket, user_id, message, false)
    }

    /// 管理员回复：可重新打开已关闭工单，无「等待对方」限制。
    pub fn reply_by_admin(&self, ticket_id: &str, admin_id: &str, message: &str) -> Result<Ticket> {
        let ticket = self
            .get(ticket_id)
            .ok_or_else(|| anyhow::anyhow!("ticket not found"))?;
        self.append_message(ticket, admin_id, message, true)
    }

    /// 追加消息并更新工单的 reply_status / status / updated_at。
    fn append_message(
        &self,
        mut ticket: Ticket,
        user_id: &str,
        message: &str,
        is_admin: bool,
    ) -> Result<Ticket> {
        let now = chrono::Utc::now().timestamp();
        let msg = TicketMessage {
            id: uuid::Uuid::new_v4().to_string(),
            ticket_id: ticket.id.clone(),
            user_id: user_id.to_string(),
            message: message.to_string(),
            created_at: now,
        };
        // is_admin 回复 = 回复方不是工单发起人 → 待用户回复；否则待客服回复
        ticket.reply_status = if user_id != ticket.user_id {
            REPLY_WAIT_USER
        } else {
            REPLY_WAIT_ADMIN
        };
        if is_admin {
            ticket.status = STATUS_OPEN;
        }
        ticket.updated_at = now;
        self.persist_message(&msg)?;
        self.persist_ticket(&ticket)?;
        self.by_id.write().insert(ticket.id.clone(), ticket.clone());
        self.messages
            .write()
            .entry(ticket.id.clone())
            .or_default()
            .push(msg);
        Ok(ticket)
    }

    /// 关闭工单
    pub fn close(&self, ticket_id: &str) -> Result<Ticket> {
        let mut ticket = self
            .get(ticket_id)
            .ok_or_else(|| anyhow::anyhow!("ticket not found"))?;
        ticket.status = STATUS_CLOSED;
        ticket.updated_at = chrono::Utc::now().timestamp();
        self.persist_ticket(&ticket)?;
        self.by_id.write().insert(ticket.id.clone(), ticket.clone());
        Ok(ticket)
    }

    /// 删除用户的全部工单及消息（用户删除时清理，对齐 v2board delUser）。
    pub fn delete_by_user(&self, user_id: &str) {
        let ids: Vec<String> = self
            .by_id
            .read()
            .values()
            .filter(|t| t.user_id == user_id)
            .map(|t| t.id.clone())
            .collect();
        let mut messages = self.messages.write();
        let mut by_id = self.by_id.write();
        for id in &ids {
            by_id.remove(id);
            if let Some(list) = messages.remove(id) {
                for m in &list {
                    let _ = self.store.delete(&format!("ticket_msg:{}", m.id));
                }
            }
            let _ = self.store.delete(&format!("ticket:{}", id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> TicketStore {
        TicketStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn create_and_reply() {
        let s = store();
        let t = s.create("u1", "求助", 1, "连不上").unwrap();
        assert_eq!(t.status, STATUS_OPEN);
        assert_eq!(s.messages(&t.id).len(), 1);

        // 用户连续两条应被拒
        assert!(s.reply(&t.id, "u1", "再补充").is_err());
        // 管理员回复成功
        let t2 = s.reply(&t.id, "admin", "已处理").unwrap();
        assert_eq!(t2.reply_status, REPLY_WAIT_USER);
        // 用户再回复成功
        let t3 = s.reply(&t.id, "u1", "谢谢").unwrap();
        assert_eq!(t3.reply_status, REPLY_WAIT_ADMIN);
    }

    #[test]
    fn close_and_admin_reopen() {
        let s = store();
        let t = s.create("u1", "问题", 0, "内容").unwrap();
        let closed = s.close(&t.id).unwrap();
        assert_eq!(closed.status, STATUS_CLOSED);
        // 已关闭用户不可回复
        assert!(s.reply(&t.id, "u1", "追问").is_err());
        // 管理员回复重新打开
        let reopened = s.reply_by_admin(&t.id, "admin", "补答").unwrap();
        assert_eq!(reopened.status, STATUS_OPEN);
    }

    #[test]
    fn list_and_delete_by_user() {
        let s = store();
        s.create("u1", "a", 0, "1").unwrap();
        s.create("u1", "b", 0, "2").unwrap();
        s.create("u2", "c", 0, "3").unwrap();
        assert_eq!(s.list_by_user("u1").len(), 2);
        assert_eq!(s.list_all().len(), 3);
        s.delete_by_user("u1");
        assert_eq!(s.list_all().len(), 1);
        assert_eq!(s.messages(&s.list_all()[0].id).len(), 1);
    }
}