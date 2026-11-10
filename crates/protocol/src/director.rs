//! 导演：玩家的输入框与小镇之间的会话。导演就是玩家本人，读得到村民的目标、心声和私聊，
//! 但只能经内核执行世界修改（投放道具、发通知、贴公告），不直接改写任何村民的 Agent 状态。
//! 与村民 Agent 同一纪律：模型提议，世界确认。
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// 会话里的一条消息。玩家的话与导演的回应分开存，回应里带这一轮实际执行了什么。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorEntry {
    /// 玩家下达这条指令时的世界标识；换世界后旧对话不再送给模型。
    pub world_id: String,
    pub player: String,
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub effects: Vec<DirectorEffect>,
    /// 这一轮模型没给正文、调用全部成功时的袖珍总结也放这里；失败原因同样写进来。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 一次工具调用的结果，给玩家看的一句话与失败类别。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorEffect {
    /// 工具名，如 `place_prop`。
    pub tool: String,
    pub ok: bool,
    /// 成功写做了什么，失败写原因。
    pub text: String,
    /// 失败类别：参数、世界规则、被拒或因世界已变而过时；成功时不出现。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<DirectorErrorKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectorErrorKind {
    /// 参数缺项、类型不对，或工具名不存在。
    InvalidArguments,
    /// 内核规则不允许（地点不存在、没有空位、道具定义不在库里）。
    World,
    /// 导演自己的规则不允许（越权、超出每次调用上限、数值超范围）。
    Rejected,
    /// 发出后才等的这段时间里世界变了，改变世界的调用退回，要求重新观察。
    Stale,
}

/// 导演面板的状态与最近一轮。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorState {
    pub enabled: bool,
    /// 正在等模型回复。
    pub thinking: bool,
    /// 最近这么几轮对话，新的在后。
    pub entries: Vec<DirectorEntry>,
    /// 还没到点的事件与它们的时刻，给面板右侧的时间线。
    pub pending: Vec<DirectorPending>,
    /// 面板上显示的世界时钟。
    pub clock: String,
}

/// 一条待触发的导演事件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectorPending {
    pub at: f64,
    pub clock: String,
    pub locale: String,
    pub text: String,
}

/// 随世界存档保存的导演会话：只放数据，供 host 持久化与重启后接续。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorSave {
    /// 对话属于哪个世界；换世界标识后旧对话不再参与。
    #[serde(default)]
    pub world_id: String,
    #[serde(default)]
    pub entries: VecDeque<DirectorEntry>,
    /// 更早对话的累积摘要。
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub next_round: u64,
}
