//! 实时剧本：只追加的结构化流水与玩家查询。剧本只给玩家看，不写进村民记忆，也不进给模型的题面。
//! 条目按世界内顺序编号；私密条目记下属于谁的私密内容，界面据此在「仅公开」时隐藏。
use serde::{Deserialize, Serialize};

/// 一条剧本是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManuscriptKind {
    Speech,
    Thought,
    Action,
    /// 已确认的玩法选择：报名、夜间计划、选票等；“登记了”不等于“做到了”
    Choice,
    Announcement,
    World,
    Director,
}

/// 谁看得见：公开的进了世界或公开场合；私密的是私聊、心声、后台裁判事实等，只有部分人或没人知道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManuscriptVisibility {
    Public,
    Private,
}

/// 怎么说：台词和公告保留原话；心声标成自述；动作结果说明成没成。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManuscriptTone {
    Statement,
    Thought,
    Result,
    Announcement,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManuscriptEntry {
    /// 世界内连续编号，从 1 开始；断线续读与去重都看它
    pub seq: u64,
    pub world_id: String,
    /// 模拟分钟；排序以它为准，不用消息到达时间
    pub sim_minutes: f64,
    pub clock: String,
    pub kind: ManuscriptKind,
    pub visibility: ManuscriptVisibility,
    pub tone: ManuscriptTone,
    /// 说话、经历或行动的村民编号；公告与世界事件没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    /// 原文或世界确认的结果；心声是本人的自述，不是事实
    pub text: String,
    /// 这段私密内容属于谁；公开条目为空
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<String>,
    /// 来源编号：聊天的 `chat:<交谈>:<序号>`、经历的 `fact:<人物>:<事件>`、动作编号等；用于去重与追溯
    pub source: String,
}

/// 最近一页：entries 新的在后；cursor 是当前已写到的最大编号（没有条目时为 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManuscriptPage {
    pub world_id: String,
    pub entries: Vec<ManuscriptEntry>,
    pub cursor: u64,
}

/// 模型整理的剧情概览。它只是对实录的归纳；每条判断用 `sources` 指回条目的 seq，
/// 不替代世界确认的台词与结果。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManuscriptSummary {
    /// 概括到哪一条实录（含）；没有新剧情时与上一版相同。
    pub through_seq: u64,
    pub through_clock: String,
    /// 最近发生了什么
    pub situation: String,
    /// 有明确分歧、承诺或利益冲突的人和事
    #[serde(default)]
    pub conflicts: Vec<Vec<String>>,
    /// 承诺了但还不能确认已经兑现的事
    #[serde(default)]
    pub promises: Vec<Vec<String>>,
    /// 仍未得到证实的问题
    #[serde(default)]
    pub questions: Vec<Vec<String>>,
}
