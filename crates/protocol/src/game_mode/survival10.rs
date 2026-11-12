//! 聪明人小镇·十日生存：旁观者可以看到库存，Agent 只能收到属于自己的私有信息。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Survival10Phase {
    Waiting,
    Day,
    Speech,
    Vote,
    Night,
    Jury,
    Over,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survival10Seat {
    pub seat: u32,
    pub id: String,
    pub name: String,
    /// 仅旁观者视角显示，禁止放入其他人的 Agent Context。
    pub rice: u32,
    /// 今日已经结算入账的有效劳动分钟；旁观者看得见，其他人不据此得知对方是否领过米。
    #[serde(default)]
    pub worked_minutes: f64,
    /// 此刻在本人分配到的田块上已经劳动了多少分钟；包含尚未到下一分钟结算的那一小段，中断后回到
    /// 已入账的 `worked_minutes`。
    #[serde(default)]
    pub labor_progress: f64,
    /// 此刻正在本人分配到的田块上执行持续的劳动动作。离开、聊天或做别的事时立即为 false。
    #[serde(default)]
    pub working_now: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub farm: Option<String>,
    pub hungry_streak: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_day: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survival10Line {
    pub seat: u32,
    pub text: String,
    pub at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survival10Ballot {
    pub voter: u32,
    /// 为防止抢先投票者影响其他人，揭票前为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survival10View {
    pub day: u32,
    pub phase: Survival10Phase,
    pub public_rice: u32,
    pub produced_today: u32,
    #[serde(default)]
    pub registered_today: u32,
    pub seats: Vec<Survival10Seat>,
    pub notice: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    pub lines: Vec<Survival10Line>,
    pub ballots: Vec<Survival10Ballot>,
    pub revealed: bool,
    pub history: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Survival10Context {
    pub day: u32,
    pub phase: String,
    pub me: String,
    pub goal: String,
    pub public_rice: u32,
    pub my_rice: u32,
    #[serde(default)]
    pub worked_minutes: u32,
    #[serde(default)]
    pub registered_today: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub farm: Option<String>,
    pub hungry_streak: u8,
    pub seats: String,
    pub known: Vec<String>,
    pub history: Vec<String>,
    pub now: String,
    pub actions: Vec<String>,
}
