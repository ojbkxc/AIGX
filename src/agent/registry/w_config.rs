//! 批2 并行单元 U-C（#7 组C：config_set 族 + update/upgrade + backup export + subscription 读侧）的工具声明文件 —— **文件独占**：仅本单元 owner 编辑。
//!
//! 现在为空片段；`tools()` 待 U-C 填充 `td(...)` 声明。assembler 在
//! [`super::build_tools`] 已一次性串接本文件，**owner 不回改 `mod.rs`**。
//! 执行体复用 `super::*`（`td` / `RiskLevel` / `admin` / helper 全已可见）。

use super::*;

pub(super) fn tools() -> Vec<ToolDef> {
    // U-C：批2 工具在此追加（每条一个 td(...)）。
    Vec::new()
}
