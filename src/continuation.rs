//! **长度截断的自动续写**：上游这一轮说到一半就停了，网关自己把它问完
//! （对照 Agent2API `core/upstream/continuation.rs`）。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 上游的 turn 会**中途收尾**：输出预算用尽（`finish_reason: length` 一族）、
//! 连接被掐断、或者模型把整轮都花在思考通道里、正文只吐了一两个词。客户端
//! 拿到的是**半截答案**，而它没法区分「模型就说这么多」与「被腰斩」。
//!
//! 做法与上游/参考实现（Trae2api-cn `continuation.py`）一致：判出「这一轮像
//! 被截断」，把**已经写出的那半截**当 assistant 回合回放，追一句「请继续输出」
//! 再打一次 —— 新内容接在同一条客户端响应后面，客户端不用自己拼。
//!
//! ── 判据：六条（顺序与上游逐条对应，唯一的一份清单）──────────
//!   1. **工具轮永不续**（最高优先级）：tool_calls 的执行权在客户端，续写
//!      等于伪造一次工具调用；
//!   2. `finish_reason` 属于 length 族（8 个上游写法，别名归一在各桥
//!      [`crate::bridge::is_length_finish`] 完成，判据只看 [`FinishReason::Length`]）；
//!   3. 正文为空但有思考：整轮烧在思考通道里，该再问一次（两者都空 → 交给
//!      既有空答复兜底链，不归本模块）；
//!   4. 代码围栏 ``` 数量为**奇数**（数整篇，未闭合是硬证据）；
//!   5. 末 100 字符括号失衡（先给「散文里零星括号」留 +2 容忍，只认「走完
//!      仍未闭合」，落单闭合符按噪声跳过 —— 上游实测踩到的误判点）；
//!   6. 末 100 字符悬挂尾部（`xxx,` / `xxx...` / `xxx:` 等），或「长思考
//!      + 极短正文且不以句末符号收尾」。
//!
//! ── 停机条件（两道）────────────────────────────────────────
//!   - 轮数上限 `usage.auto_continue_max_rounds`（1~8，默认 3）；
//!   - **相似度闸**：新续的正文与上一轮逐字符位置相似度 > 0.5 即停 —— 防
//!     「模型每次在同一位置吐同一段」的死循环。
//!
//! ── 判据读整体不读本轮（上游实测钉死的一条）──────────────────
//! 结构性判据（未闭合 ```、括号失衡）问的是「**答案**写完了没有」，而续写
//! 场景里的答案是跨轮拼接的整体：上一轮留开的那道 ``` 正是这一轮关掉的。
//! 按本轮自己那段判会把「已经补完」读成「又断了」，白花配额并往答案尾巴
//! 接重复。`finish` 与 `has_tool_calls` 例外：那是**本轮的事实**。
//!
//! ── 代价 ────────────────────────────────────────────────────
//! 多轮 = 多次上游往返：每轮 prompt 含前几轮全部历史，真实成本按轮**求和**
//! （[`sum_usage`]），计费与请求日志拿到的因此是总量。续写轮重新走选路，
//! 可能落到另一个账号/渠道（开开关 = 允许这次请求的内容出现在多个上游上）。
//!
//! ── 默认关 ──────────────────────────────────────────────────
//! `usage.auto_continue`（默认 false）。关着时调用侧**根本不包装**，下发
//! 字节与接入前逐字一致。

use futures::StreamExt;
use std::sync::Arc;

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatFormat, ChatMessage,
    ChatResponse, FinishReason, Role, UsageStats,
};

/// 悬挂尾部的**单字符**集合（rstrip 之后看末字符；上游的 8 条正则等价化成
/// 集合判定，`//` 是「以 `/` 结尾」的子集，合并）。
const DANGLING_ENDING_CHARS: &[char] = &[',', '|', '｜', '\\', '/', '#', '-', '–', '—', ':', '：'];

/// 句末符号集合（上游 `_SENTENCE_END_RE` 字符类逐字对应；含中文右引号/右
/// 括号闭合形态，漏了会把「说完了」判成半截）。
const SENTENCE_ENDING_CHARS: &[char] = &[
    '.', '!', '?', '。', '！', '？', ']', ')', '}', '`', '"', '\'', '”', '’', '』', '」',
];

/// 括号配对扫描的回看长度（上游 `body[-100:]`）。
const TAIL_SCAN_CHARS: usize = 100;

/// 「极短正文」上限（上游 `_FRAGMENT_TEXT_LIMIT`）。
const FRAGMENT_TEXT_MAX_CHARS: usize = 40;

/// 「长思考」下限（上游 `_FRAGMENT_REASONING_MIN`）。
const FRAGMENT_REASONING_MIN_CHARS: usize = 200;

/// 思考与正文的倍数（上游 `_FRAGMENT_REASONING_RATIO`）。按现有常量这条
/// **永不生效**（正文 ≤40 ⇒ 门槛 ≤160 < 思考下限 200），但它是这条判据的
/// 定义的一部分：改任何一条常量都会让它重新承重。
const FRAGMENT_REASONING_RATIO: usize = 4;

/// 相似度停机阈值（**严格大于**才停：0.5 更像「接着写了不同的话」）。
const SIMILARITY_STOP: f64 = 0.5;

/// 续写指令（上游 `CONTINUE_INSTRUCTION` 逐字照抄；这条文本会进上游请求体）。
pub const CONTINUE_INSTRUCTION: &str =
    "请继续输出，从你中断的地方继续，不要重复已经写过的内容，不要重新开始。";

/// 「整轮只有思考、正文为空」时的指令（上游同名常量，保持英文：读者是模型）。
pub const REASONING_ONLY_INSTRUCTION: &str = "Your previous response contained only \
    thinking/reasoning with no actual output. Provide the actual response now - either \
    text content or a tool call.";

/// 一轮上游答复的**判据输入**（两条链路都折成它再交给 [`looks_truncated`]）。
#[derive(Clone, Debug, Default)]
pub struct TurnParts {
    /// 可见正文（`message.content` / 各帧 `delta.content` 拼接）
    pub text: String,
    /// 思考内容
    pub reasoning: String,
    /// 本轮出现过工具调用
    pub has_tool_calls: bool,
    /// 本轮 `finish_reason`（None = 上游没给终帧）
    pub finish: Option<FinishReason>,
}

/// 这一轮像不像**说到一半就停了**（六条判据见模块头）。
pub fn looks_truncated(parts: &TurnParts) -> bool {
    // 1. 工具轮永不续（在 length 判定之前）
    if parts.has_tool_calls {
        return false;
    }
    // 2. 上游自己说「预算用尽」
    if parts.finish == Some(FinishReason::Length) {
        return true;
    }
    let body = parts.text.as_str();
    if body.trim().is_empty() {
        // 3. 正文为空：有思考就再问一次；两者都空交给既有的空答复兜底链
        return !parts.reasoning.trim().is_empty();
    }
    // 4. 代码围栏未闭合（数整篇）
    if unclosed_code_fence(body) {
        return true;
    }
    let tail = last_chars(body, TAIL_SCAN_CHARS);
    // 5. 末段括号失衡
    if unbalanced_brackets(&tail) {
        return true;
    }
    // 6a. 悬挂尾部
    if dangling_tail(&tail) {
        return true;
    }
    // 6b. 长思考 + 极短正文且不以句末符号收尾
    stopped_after_thinking(body, &parts.reasoning)
}

/// 整轮只有思考、没有可见正文（决定追哪一条指令用）。
pub fn looks_reasoning_only(parts: &TurnParts) -> bool {
    !parts.has_tool_calls && parts.text.trim().is_empty() && !parts.reasoning.trim().is_empty()
}

/// 逐字符**位置**相似度（上游 `similarity` 逐字对应）：两者较短长度为窗口，
/// 数同位置相同字符的比例。不是编辑距离、不是前缀长度 —— 它便宜且对
/// 「复读同一段开头」敏感，而那正是死循环的形状。
pub fn similarity(left: &str, right: &str) -> f64 {
    let overlap = std::cmp::min(left.chars().count(), right.chars().count());
    if overlap == 0 {
        return 0.0;
    }
    let same = left
        .chars()
        .zip(right.chars())
        .filter(|(a, b)| a == b)
        .count();
    same as f64 / overlap as f64
}

/// 续出来的这一轮是不是上一轮的**复读**（超过阈值即停）。
pub fn is_repeat(previous: &str, next: &str) -> bool {
    similarity(previous, next) > SIMILARITY_STOP
}

/// 把消息列表改写成「续写这一轮」的形状：已写出的半截当 assistant 回合
/// 回放（**原文不 trim**，否则「从你中断的地方继续」对不上上游自己的语气），
/// 再追一句 user 指令。正文为空时不造空的 assistant 回合。
pub fn append_continuation(messages: &mut Vec<ChatMessage>, partial: &str, reasoning_only: bool) {
    if !partial.trim().is_empty() {
        messages.push(ChatMessage::assistant(partial));
    }
    let instruction = if reasoning_only {
        REASONING_ONLY_INSTRUCTION
    } else {
        CONTINUE_INSTRUCTION
    };
    messages.push(user_message(instruction));
}

/// 构造一条纯文本 user 消息（ChatMessage 的 user 构造器不在 bridge 层，
/// 续写指令只需要 content 一个字段）。
fn user_message(text: &str) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: Some(text.to_string()),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    }
}

/// 从非流式 ChatResponse 读判据输入。
pub fn parts_of_response(resp: &ChatResponse) -> TurnParts {
    TurnParts {
        text: resp.message.content_str().to_string(),
        reasoning: resp.message.reasoning.clone().unwrap_or_default(),
        has_tool_calls: resp
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty()),
        finish: Some(resp.finish_reason),
    }
}

/// 把各轮的**可见正文**接成一条（`concat` 不加分隔：上游被腰斩处没有换行，
/// 插空格或换行会改坏 markdown 与代码块的拼接位置）。
pub fn join_text(rounds: &[String]) -> String {
    rounds.concat()
}

/// 把「到目前为止、客户端会看到的整体」组装成判据输入。
///
/// `finish` 与 `has_tool_calls` 取**本轮**（本轮的事实），text/reasoning 用
/// 「已拼各轮 + 本轮」的整体（理由见模块头）。
fn verdict_of(
    accumulated_text: &str,
    accumulated_reasoning: &str,
    any_tool_calls: bool,
    current: &TurnParts,
) -> TurnParts {
    TurnParts {
        text: accumulated_text.to_string(),
        reasoning: accumulated_reasoning.to_string(),
        has_tool_calls: any_tool_calls || current.has_tool_calls,
        finish: current.finish,
    }
}

/// 跨轮 usage 合并：四项**全部求和**（每轮都是真实上游往返，第 N 轮的
/// prompt 里装着前 N-1 轮的全部内容，真实成本是求和不是覆盖）。
pub fn sum_usage(rounds: &[UsageStats]) -> UsageStats {
    rounds.iter().fold(UsageStats::default(), |mut acc, round| {
        acc.prompt_tokens += round.prompt_tokens;
        acc.completion_tokens += round.completion_tokens;
        acc.total_tokens += round.total_tokens;
        acc.cached_prompt_tokens += round.cached_prompt_tokens;
        acc.cache_creation_tokens += round.cache_creation_tokens;
        acc.cache_read_tokens += round.cache_read_tokens;
        acc
    })
}

/// 流式路径的判据输入累积器：从**下发的 chunk** 攒出 [`TurnParts`]。
///
/// 读 chunk 层（`ReasoningCoalescer`/keepalive 过滤之后）而不是桥的原始字节：
/// 否则换行保活帧会被算进正文（干扰悬挂尾部判据）、思考数两遍。
#[derive(Clone, Debug, Default)]
pub struct TurnAccumulator {
    parts: TurnParts,
    /// 本轮是否已经看到终帧（`finish_reason` 非 None 的 chunk）
    saw_finish: bool,
    /// 本轮是否出现过流内错误（chunk 流的 Err）
    saw_error: bool,
    /// 本轮看到的最后一份 usage（上游可能多帧补发，后到的即最终值；跨轮再求和）
    usage: Option<UsageStats>,
}

impl TurnAccumulator {
    /// 吃一个 chunk；返回它**是不是**终止类（终帧或错误帧 —— 调用侧据此决定
    /// 「压住不发」还是「照常下发」）。
    pub fn push_chunk(&mut self, chunk: &Result<ChatChunk, BridgeError>) -> bool {
        let Ok(chunk) = chunk else {
            self.saw_error = true;
            return true;
        };
        if let Some(stats) = chunk.usage {
            self.usage = Some(stats);
        }
        let mut terminal = false;
        if let Some(reason) = chunk.finish_reason {
            self.parts.finish = Some(reason);
            self.saw_finish = true;
            terminal = true;
        }
        if let Some(text) = &chunk.delta.content {
            self.parts.text.push_str(text);
        }
        if let Some(text) = &chunk.delta.reasoning {
            self.parts.reasoning.push_str(text);
        }
        if chunk
            .delta
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        {
            self.parts.has_tool_calls = true;
        }
        terminal
    }

    pub fn parts(&self) -> &TurnParts {
        &self.parts
    }

    pub fn saw_error(&self) -> bool {
        self.saw_error
    }

    /// 本轮 token 数（没有 usage 帧时全 0：宁可不记也不猜）。
    pub fn usage(&self) -> UsageStats {
        self.usage.unwrap_or_default()
    }

    /// 本轮是否产出了任何内容（正文与思考都空 = 没有可续的东西；判据读的是
    /// 累积文本，第一轮之后不拦会一路问到轮数上限、每轮白花一次往返）。
    pub fn produced_anything(&self) -> bool {
        !self.parts.text.trim().is_empty() || !self.parts.reasoning.trim().is_empty()
    }
}

/// 流式续写的整体循环（判据 → 是否再来一轮）。`rounds` 为已完成的轮数。
///
/// 返回「要不要续下一轮」；`repeat` 与「本轮无产出」在这里拦（上游同两处
/// 条件），轮数与开关上限由调用侧持有。
// 参数是续写判据的完整集合，与上游 wants_another_round 的入参一一对应，
// 不为规避参数计数拆散语义。
#[allow(clippy::too_many_arguments)]
pub fn wants_another_round(
    joined_text: &str,
    joined_reasoning: &str,
    any_tool_calls: bool,
    last_round_text: Option<&str>,
    current: &TurnParts,
    produced: bool,
    rounds_done: usize,
    max_rounds: usize,
) -> bool {
    if rounds_done >= max_rounds || !produced {
        return false;
    }
    if current.has_tool_calls {
        return false;
    }
    if let Some(previous) = last_round_text {
        if is_repeat(previous, &current.text) {
            return false;
        }
    }
    let verdict = verdict_of(joined_text, joined_reasoning, any_tool_calls, current);
    looks_truncated(&verdict)
}

/// 非流式续写的同类判断（判据输入与流式完全同源，见 [`verdict_of`]）。
pub fn wants_another_round_response(parts: &TurnParts, previous_round_text: Option<&str>) -> bool {
    if parts.has_tool_calls {
        return false;
    }
    if let Some(previous) = previous_round_text {
        if is_repeat(previous, &parts.text) {
            return false;
        }
    }
    looks_truncated(parts)
}

/// ``` 围栏数量为奇数（未闭合是硬证据）。
fn unclosed_code_fence(text: &str) -> bool {
    text.matches("```").count() % 2 == 1
}

/// 取末尾至多 `n` 个**字符**（不是字节：中文按字节切会切出半个字）。
fn last_chars(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

/// 末段括号是否失衡：先按**计数差**（opens > closes + 2，给散文里零散括号
/// 留容忍），再配对走查，**只认「走完仍未闭合」**——窗口左边界切在一对括号
/// 中间出现的落单闭合符按噪声跳过（抄上游/参考实现会把它也点亮，完整答案
/// 被误判半截的代价比漏判重）。
fn unbalanced_brackets(tail: &str) -> bool {
    let opens = tail
        .chars()
        .filter(|c| matches!(c, '[' | '{' | '('))
        .count();
    let closes = tail
        .chars()
        .filter(|c| matches!(c, ']' | '}' | ')'))
        .count();
    if opens > closes + 2 {
        return true;
    }
    let mut stack: Vec<char> = Vec::new();
    for c in tail.chars() {
        match c {
            '[' | '{' | '(' => stack.push(c),
            c @ (')' | ']' | '}')
                if matching_opener(c).is_some_and(|opener| stack.last() == Some(&opener)) =>
            {
                stack.pop();
            }
            _ => {}
        }
    }
    !stack.is_empty()
}

fn matching_opener(closer: char) -> Option<char> {
    match closer {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        _ => None,
    }
}

/// rstrip 之后是否落在悬挂尾部形态（省略号单独判 —— **单个句点不算截断**，
/// 正常句子以 `.` 收尾是常态）。
fn dangling_tail(tail: &str) -> bool {
    let trimmed = tail.trim_end();
    if trimmed.ends_with("...") {
        return true;
    }
    trimmed
        .chars()
        .last()
        .is_some_and(|c| DANGLING_ENDING_CHARS.contains(&c))
}

/// 「长思考 + 极短正文且正文没说完」（模型把整轮预算烧在思考通道）。
fn stopped_after_thinking(text: &str, reasoning: &str) -> bool {
    let visible = text.trim();
    let thinking = reasoning.trim();
    let visible_chars = visible.chars().count();
    if visible_chars == 0 || visible_chars > FRAGMENT_TEXT_MAX_CHARS {
        return false;
    }
    let thinking_chars = thinking.chars().count();
    if thinking_chars < FRAGMENT_REASONING_MIN_CHARS {
        return false;
    }
    if thinking_chars < visible_chars * FRAGMENT_REASONING_RATIO {
        return false;
    }
    !ends_with_sentence_mark(visible)
}

fn ends_with_sentence_mark(visible: &str) -> bool {
    visible
        .chars()
        .last()
        .is_some_and(|c| SENTENCE_ENDING_CHARS.contains(&c))
}

/// 配置口径（续写**次数**，1~8）→ 含首轮的**总轮数**。
///
/// 单独抽成纯函数是为了能被钉住：上游曾因把两个口径当成同一个，设 1 直接
/// 不续、设 3 只续 2 轮。
pub fn total_rounds_of(continuations: u64) -> usize {
    continuations.clamp(1, 8) as usize + 1
}

/// 读开关，回答「这条请求要不要走续写包装」。`None` 时调用侧**连包装都不
/// 做** —— 开关关着也要包一层就多一次 channel 往返与每 chunk 一次判据扫描，
/// 0 收益不付。
pub fn plan() -> Option<usize> {
    if !crate::config::auto_continue_enabled() {
        return None;
    }
    Some(total_rounds_of(crate::config::auto_continue_max_rounds()))
}

/// 终止帧及其后帧的暂存上限（1 MiB）。超过即放行全部并放弃续写 ——
/// 宁可不续，也不无界占内存。
const MAX_HELD_BYTES: usize = 1024 * 1024;

/// 粗估一个 chunk 的正文体积（判据与续写都不读工具参数，只为闸住暂存内存）。
fn estimate_chunk_bytes(chunk: &ChatChunk) -> usize {
    let mut n = 0usize;
    if let Some(text) = &chunk.delta.content {
        n += text.len();
    }
    if let Some(text) = &chunk.delta.reasoning {
        n += text.len();
    }
    n
}

/// **流式**续写包装：透传上游流，判出截断时压住终止帧（及其后的补发帧），
/// 半截原文回放进上下文再开一轮，新内容接在同一条流后面。
///
/// 只在 [`plan`] 给出 Some 时被调用（关着零开销）。语义与上游
/// `continuation.stream()` 对齐：
///   - 终止帧压住不发 —— 客户端先看到「半截」再看到续写内容，finish 只出
///     最终一次；
///   - 续写轮**复用同一桥与渠道**（内容连贯优先于换路再试）；建流失败则
///     把压住的终止帧与错误帧原样放行，客户端拿到的是「带错误的完整前半」；
///   - 客户端断连（send 失败）即刻终止后台任务，上游流随之 drop。
pub fn wrap_stream(
    first: ChatChunkStream,
    bridge: Arc<dyn Bridge>,
    mut req: ChatFormat,
    ctx: BridgeContext,
    max_rounds: usize,
) -> ChatChunkStream {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<ChatChunk, BridgeError>>(64);
    tokio::spawn(async move {
        let mut stream = first;
        let mut rounds_done = 1usize;
        // 跨轮整体（判据读整体，见模块头）；本轮文本单独记一份给复读闸对比。
        let mut joined_text = String::new();
        let mut joined_reasoning = String::new();
        let mut any_tool_calls = false;
        let mut last_round_text: Option<String> = None;
        loop {
            let mut acc = TurnAccumulator::default();
            let mut held: Vec<Result<ChatChunk, BridgeError>> = Vec::new();
            let mut held_bytes = 0usize;
            let mut holding = false;
            // 暂存超上限 / 客户端断连时置真：此后帧一律直通，不再续。
            let mut no_more = false;
            while let Some(item) = stream.next().await {
                // 判据照常吃满（即便不再续，也不影响正确性，省一个分支）。
                let terminal = acc.push_chunk(&item);
                if no_more {
                    if tx.send(item).await.is_err() {
                        return;
                    }
                    continue;
                }
                if terminal || holding {
                    holding = true;
                    held_bytes += item.as_ref().map(estimate_chunk_bytes).unwrap_or(64);
                    held.push(item);
                    if held_bytes > MAX_HELD_BYTES {
                        no_more = true;
                        for h in held.drain(..) {
                            if tx.send(h).await.is_err() {
                                return;
                            }
                        }
                    }
                } else if tx.send(item).await.is_err() {
                    return;
                }
            }
            let parts = acc.parts().clone();
            let mut cur_joined = joined_text.clone();
            cur_joined.push_str(&parts.text);
            let mut cur_reasoning = joined_reasoning.clone();
            cur_reasoning.push_str(&parts.reasoning);
            let want = !no_more
                && !acc.saw_error()
                && wants_another_round(
                    &cur_joined,
                    &cur_reasoning,
                    any_tool_calls,
                    last_round_text.as_deref(),
                    &parts,
                    acc.produced_anything(),
                    rounds_done,
                    max_rounds,
                );
            if !want {
                // 自然停止 / 判不住 / 到上限：放行压住的终止帧，收尾。
                for h in held.drain(..) {
                    if tx.send(h).await.is_err() {
                        return;
                    }
                }
                return;
            }
            // 续一轮：丢终止帧（最终轮的 finish 才是客户端该看到的），
            // 半截原文当 assistant 回合回放 + 追「请继续」。
            drop(held);
            append_continuation(&mut req.messages, &parts.text, looks_reasoning_only(&parts));
            rounds_done += 1;
            last_round_text = Some(parts.text.clone());
            joined_text = cur_joined;
            joined_reasoning = cur_reasoning;
            any_tool_calls = any_tool_calls || parts.has_tool_calls;
            match bridge.chat_stream(&req, &ctx).await {
                Ok(s) => stream = s,
                Err(e) => {
                    tracing::warn!("auto-continue(流式) 续写轮建流失败: {e}");
                    // 前半已流出的内容照常收完：补一个错误帧（SSE error 事件）。
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
        }
    });
    tokio_stream::wrappers::ReceiverStream::new(rx).boxed()
}

/// **非流式**续写循环：逐轮 forward，正文拼接，usage 求和。
///
/// 与流式共用同一套判据输入（[`wants_another_round`]）；续写轮失败时
/// **保留已拿到的内容**返回（半截也好过报错，与上游同语义）。
/// 没续过（`rounds_done == 1`）时响应原样返回，一个字节都不动。
pub async fn continue_non_streaming(
    bridge: Arc<dyn Bridge>,
    mut req: ChatFormat,
    ctx: &BridgeContext,
    mut response: ChatResponse,
    max_rounds: usize,
) -> ChatResponse {
    let mut current = parts_of_response(&response);
    let mut joined_text = current.text.clone();
    let mut joined_reasoning = current.reasoning.clone();
    let mut any_tool_calls = current.has_tool_calls;
    let mut last_round_text: Option<String> = None;
    let mut rounds_done = 1usize;
    let mut usages = vec![response.usage];
    loop {
        let produced = !current.text.trim().is_empty() || !current.reasoning.trim().is_empty();
        if !wants_another_round(
            &joined_text,
            &joined_reasoning,
            any_tool_calls,
            last_round_text.as_deref(),
            &current,
            produced,
            rounds_done,
            max_rounds,
        ) {
            break;
        }
        append_continuation(
            &mut req.messages,
            &current.text,
            looks_reasoning_only(&current),
        );
        let prev_round_text = std::mem::take(&mut current.text);
        match bridge.chat(&req, ctx).await {
            Ok(resp) => {
                usages.push(resp.usage);
                current = parts_of_response(&resp);
                last_round_text = Some(prev_round_text);
                joined_text.push_str(&current.text);
                joined_reasoning.push_str(&current.reasoning);
                any_tool_calls = any_tool_calls || current.has_tool_calls;
                rounds_done += 1;
            }
            Err(e) => {
                tracing::warn!("auto-continue(非流式) 续写轮失败: {e}，以已得内容返回");
                break;
            }
        }
    }
    if rounds_done > 1 {
        response.message.content = Some(joined_text);
        if !joined_reasoning.is_empty() {
            response.message.reasoning = Some(joined_reasoning);
        }
        response.usage = sum_usage(&usages);
        if let Some(finish) = current.finish {
            response.finish_reason = finish;
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(text: &str, reasoning: &str, finish: Option<FinishReason>) -> TurnParts {
        TurnParts {
            text: text.to_string(),
            reasoning: reasoning.to_string(),
            has_tool_calls: false,
            finish,
        }
    }

    /// 正常答复的五种「看着像半截但其实写完了」，一条都不许点亮。
    #[test]
    fn complete_answers_are_not_flagged() {
        for text in [
            "答案是 42。",
            "Totals: 3 items, 2 shipped, 1 returned.",
            "```rust\nfn main() {}\n```",
            "配置在 `~/.qoder-cn/settings.json`，改完重启即可",
            "- 第一步：打开面板\n- 第二步：点「刷新模型」",
        ] {
            assert!(
                !looks_truncated(&parts(text, "", Some(FinishReason::Stop))),
                "正常答复被判成截断：{text:?}"
            );
        }
    }

    /// 判据 1：工具轮永不续，**即使** finish_reason 是 length。
    #[test]
    fn tool_call_turns_are_never_continued() {
        let mut with_calls = parts("我把文件读完之后", "", Some(FinishReason::Length));
        with_calls.has_tool_calls = true;
        assert!(!looks_truncated(&with_calls));
        assert!(looks_truncated(&parts(
            "我把文件读完之后",
            "",
            Some(FinishReason::Length)
        )));
    }

    /// 判据 2：length 枚举点亮；Stop/ContentFilter 不点亮。
    #[test]
    fn length_finish_counts_as_truncation() {
        assert!(looks_truncated(&parts(
            "就这样",
            "",
            Some(FinishReason::Length)
        )));
        assert!(!looks_truncated(&parts(
            "就这样",
            "",
            Some(FinishReason::ToolCalls)
        )));
    }

    /// 判据 3：正文空 + 有思考要续；两者都空**不续**。
    #[test]
    fn empty_visible_text_depends_on_reasoning_channel() {
        assert!(looks_truncated(&parts(
            "",
            "先分析题目，再考虑边界情况。",
            None
        )));
        assert!(!looks_truncated(&parts("   ", "", None)));
    }

    /// 判据 4：围栏奇数点亮，闭合的不点亮。
    #[test]
    fn unclosed_code_fence_is_hard_evidence() {
        assert!(looks_truncated(&parts(
            "看这段：\n```python\ndef f():\n    return 1",
            "",
            None
        )));
        assert!(!looks_truncated(&parts(
            "看这段：\n```python\ndef f():\n    return 1\n```",
            "",
            None
        )));
    }

    /// 判据 5：只认「走完仍未闭合」，落单闭合符按噪声跳过。
    #[test]
    fn bracket_checks_end_with_open_group_not_stray_closer() {
        assert!(!looks_truncated(&parts("先跑 (npm test) 再提交", "", None)));
        assert!(looks_truncated(&parts("返回 {a: [1, 2}", "", None)));
        assert!(looks_truncated(&parts("调用 compute(a, b", "", None)));
        assert!(
            !looks_truncated(&parts("这里有个坑) 以及另一个)", "", None)),
            "窗口切出来的落单右括号不该点亮"
        );
        assert!(looks_truncated(&parts("嵌套 ( ( ( 还没关", "", None)));
    }

    /// 判据 6a：悬挂尾部集合；单个句点不算。
    #[test]
    fn dangling_tails_flagged_but_period_is_not() {
        for text in [
            "总价包括：",
            "第一行 |",
            "等等...",
            "路径是 src\\",
            "注释 #",
            "下一项 -",
            "结果是 :",
            "表格单元 ｜",
        ] {
            assert!(
                looks_truncated(&parts(text, "", None)),
                "悬挂尾部没抓到: {text:?}"
            );
        }
        assert!(!looks_truncated(&parts("结果是 42.", "", None)));
    }

    /// 判据 6b：长思考 + 极短正文；反照三条各有分工。
    #[test]
    fn long_think_with_stopped_fragment_needs_more() {
        let thinking = "分".repeat(FRAGMENT_REASONING_MIN_CHARS);
        assert!(looks_truncated(&parts("好的", &thinking, None)));
        assert!(!looks_truncated(&parts(
            &"好的，".repeat(20),
            &thinking,
            None
        )));
        assert!(!looks_truncated(&parts("好的", "短思考", None)));
        assert!(!looks_truncated(&parts("收到。", &thinking, None)));
    }

    /// 判据读「拼接后的整体」：第一轮点亮，第二轮补完后的整体必须熄灭。
    #[test]
    fn verdict_reads_stitched_answer_not_last_round() {
        let first = parts(
            "示例代码：\n```python\ndef solve():\n    return 42\n",
            "",
            Some(FinishReason::Stop),
        );
        assert!(looks_truncated(&first));
        let second = parts("```\n上面的函数返回 42。", "", Some(FinishReason::Stop));
        let stitched = verdict_of(&(first.text.clone() + &second.text), "", false, &second);
        assert!(
            !looks_truncated(&stitched),
            "拼接后围栏已闭合，不许再打一轮"
        );
        let opening = verdict_of(&first.text.clone(), "", false, &first);
        assert!(looks_truncated(&opening), "首轮判据必须带上本轮自己");
    }

    /// 停机闸：复读停、正好 0.5 不停。
    #[test]
    fn repeat_gate_is_strictly_above_half() {
        assert!((similarity("abcd", "abcd") - 1.0).abs() < f64::EPSILON);
        assert_eq!(0.0, similarity("", "abcd"));
        assert!(is_repeat("接着写", "接着写下去"));
        assert_eq!(0.5, similarity("abcd", "abxy"));
        assert!(!is_repeat("abcd", "abxy"));
    }

    /// 回放形态：半截当 assistant 回合 + user 指令，顺序与内容都对。
    #[test]
    fn continuation_appends_partial_then_instruction() {
        let mut messages = vec![user_message("写一首诗")];
        append_continuation(&mut messages, "床前明月光，", false);
        assert_eq!(3, messages.len());
        assert_eq!(Role::Assistant, messages[1].role);
        assert_eq!("床前明月光，", messages[1].content_str(), "回放用原文");
        assert_eq!(Role::User, messages[2].role);
        assert!(messages[2].content_str().contains("请继续输出"));
    }

    /// 空正文不造空 assistant 回合，且换成思考专用指令。
    #[test]
    fn empty_partial_skips_assistant_turn() {
        let mut messages = vec![user_message("继续")];
        append_continuation(&mut messages, "   ", true);
        assert_eq!(2, messages.len());
        assert!(messages[1].content_str().contains("thinking/reasoning"));
    }

    /// 配置口径（续写次数）→ 总轮数（含首轮）。
    #[test]
    fn continuation_count_becomes_total_rounds() {
        assert_eq!(total_rounds_of(1), 2, "设 1 = 续一轮，不是关掉");
        assert_eq!(total_rounds_of(3), 4);
        assert_eq!(total_rounds_of(8), 9);
        assert_eq!(total_rounds_of(0), 2, "越界夹紧");
    }

    /// 跨轮求和：真实成本是各轮之和（每轮 prompt 含全部历史）。
    #[test]
    fn usage_sums_across_rounds() {
        let rounds = [
            UsageStats {
                prompt_tokens: 10,
                completion_tokens: 9,
                total_tokens: 19,
                cached_prompt_tokens: 4,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
            UsageStats {
                prompt_tokens: 20,
                completion_tokens: 5,
                total_tokens: 25,
                cached_prompt_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        ];
        let merged = sum_usage(&rounds);
        assert_eq!(30, merged.prompt_tokens);
        assert_eq!(14, merged.completion_tokens);
        assert_eq!(44, merged.total_tokens);
        assert_eq!(4, merged.cached_prompt_tokens);
    }
}
