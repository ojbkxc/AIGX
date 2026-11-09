//! 批2 并行单元 U-E（#22：proxy_pool_manage + ticket_ops）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 现在为空片段；`tools()` 待 U-E 填充 `td(...)` 声明。assembler 在
//! [`super::build_tools`] 已一次性串接本文件，**owner 不回改 `mod.rs`**。
//! 执行体复用 `super::*`（`td` / `RiskLevel` / `admin` / helper 全已可见）。

use super::*;

pub(super) fn tools() -> Vec<ToolDef> {
    // U-E：批2 工具在此追加（每条一个 td(...)）。
    Vec::new()
}
