//! 批2 并行单元 U-A（#5 组A：routing/models/ip/logs/cache 写侧工具）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 现在为空片段；`tools()` 待 U-A 填充 `td(...)` 声明。assembler 在
//! [`super::build_tools`] 已一次性串接本文件，**owner 不回改 `mod.rs`**。
//! 执行体复用 `super::*`（`td` / `RiskLevel` / `admin` / helper 全已可见）。

use super::*;

pub(super) fn tools() -> Vec<ToolDef> {
    // U-A：批2 工具在此追加（每条一个 td(...)）。
    Vec::new()
}
