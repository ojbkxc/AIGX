//! 能力需求掩码（#40 路由多规则 §1「能力过滤」的**唯一**标签归一与判定处）。
//!
//! ## 为什么要有这一层
//!
//! 「这个模型能不能吃图 / 能不能调工具 / 能不能吃视频」在本仓有三处各自成
//! 词表的来源：
//!
//! | 来源 | 产出函数 | 词表 |
//! |---|---|---|
//! | 上游目录归一 | `account_session::catalog::pick_capabilities` | `vision` / `tool` / `video` / `reasoning` / `embedding` / `audio` |
//! | models.dev 补空位 + 管理员覆盖 | `model::metadata::resolve_with_sources` | `vision` / `video` / **`tools`**（复数）/ `reasoning` … |
//! | 管理员能力白名单 | `api::admin::model_manage::valid_capability` | 两套都认 |
//!
//! 同一个「支持工具调用」在不同来源里叫 `tool` 与 `tools`。选路判定若各自
//! 写一遍字符串比较，就会出现「目录说支持、选路说不支持」这类**无声**分裂
//! （形态分歧的教训见 #31）。故这里收敛成唯一的归一函数：所有来源的标签都
//! 经 [`CapabilityMask::from_tags`] 变成掩码，判定只在掩码上做。
//!
//! ## 只关心路由需求的三位
//!
//! 掩码只保留 `vision`（图像**输入**）/ `tool`（工具调用）/ `video`（视频输入）
//! 三位 —— 正好是 `RequestNeeds{tools, images, video}` 的映射目标。其余标签
//! （`chat` / `embedding` / `rerank` / `image` / `audio` / `reasoning`）**不参与**
//! 判定：`image`（文生图）与 `vision`（图生文）是两回事，归到同一位会让
//! 「请求带图」被路由到文生图模型。

/// 能力位掩码（3 位；`EMPTY` = 不需要任何能力，也 = 「已知但三位全无」）。
///
/// `Copy` + 单字节：可自由在热路径上传递与比较，无分配。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapabilityMask(u8);

impl CapabilityMask {
    /// 无需求（调用方据此短路整条过滤路径）。
    pub const EMPTY: Self = Self(0);
    /// 图像输入（目录/models.dev 的 `vision`；**不是** `image` 文生图）。
    pub const VISION: Self = Self(1);
    /// 工具调用（目录的 `tool`，models.dev 的 `tools` —— 两者归一到这里）。
    pub const TOOL: Self = Self(2);
    /// 视频输入。
    pub const VIDEO: Self = Self(4);

    /// 不需要任何能力。
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// 并集（聚合用：账号目录里「任何一个模型能做什么」）。
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `self` 是否覆盖 `required` 的**全部**位。
    ///
    /// `required` 为 `EMPTY` 时恒真（无需求 → 任何标签集都满足）。
    pub const fn covers(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// 单个标签归一（大小写不敏感；不参与判定的标签返回 `None`）。
    ///
    /// `tool` / `tools` 同义 —— 这是本仓唯一允许的标签同义映射，别在别处再写。
    pub fn from_tag(tag: &str) -> Option<Self> {
        if tag.eq_ignore_ascii_case("vision") {
            Some(Self::VISION)
        } else if tag.eq_ignore_ascii_case("tool") || tag.eq_ignore_ascii_case("tools") {
            Some(Self::TOOL)
        } else if tag.eq_ignore_ascii_case("video") {
            Some(Self::VIDEO)
        } else {
            None
        }
    }

    /// 一组标签 → 掩码（不参与判定的标签忽略）。
    pub fn from_tags<I, S>(tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut mask = Self::EMPTY;
        for tag in tags {
            if let Some(bit) = Self::from_tag(tag.as_ref()) {
                mask = mask.union(bit);
            }
        }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_normalization_covers_both_vocabularies() {
        // 目录词表（单数 tool）与 models.dev/管理面词表（复数 tools）归一到位
        assert_eq!(CapabilityMask::from_tag("tool"), Some(CapabilityMask::TOOL));
        assert_eq!(
            CapabilityMask::from_tag("tools"),
            Some(CapabilityMask::TOOL)
        );
        assert_eq!(
            CapabilityMask::from_tag("Vision"),
            Some(CapabilityMask::VISION),
            "上游目录里见过大小写混写"
        );
        assert_eq!(
            CapabilityMask::from_tag("VIDEO"),
            Some(CapabilityMask::VIDEO)
        );
        // `image`（文生图）/ `reasoning` 等不参与路由需求判定
        assert_eq!(CapabilityMask::from_tag("image"), None);
        assert_eq!(CapabilityMask::from_tag("chat"), None);
        assert_eq!(CapabilityMask::from_tag(""), None);
    }

    #[test]
    fn from_tags_unions_and_ignores_the_rest() {
        let tags: Vec<String> = ["chat", "tools", "reasoning", "vision"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mask = CapabilityMask::from_tags(&tags);
        assert!(mask.covers(CapabilityMask::TOOL));
        assert!(mask.covers(CapabilityMask::VISION));
        assert!(!mask.covers(CapabilityMask::VIDEO));
        assert_eq!(mask, CapabilityMask::TOOL.union(CapabilityMask::VISION));
    }

    #[test]
    fn covers_is_subset_semantics() {
        let both = CapabilityMask::TOOL.union(CapabilityMask::VISION);
        // 要两位：只有其中一位不够
        assert!(!CapabilityMask::TOOL.covers(both));
        assert!(both.covers(both));
        assert!(both.covers(CapabilityMask::TOOL));
        // 无需求恒满足（含「已知但三位全无」的 EMPTY 标签集）
        assert!(CapabilityMask::EMPTY.covers(CapabilityMask::EMPTY));
        assert!(CapabilityMask::EMPTY.is_empty());
        assert!(!CapabilityMask::VIDEO.is_empty());
    }
}
