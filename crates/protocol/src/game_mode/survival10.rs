//! 聪明人小镇·十日生存：旁观者可以看到库存，Agent 只能收到属于自己的私有信息。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Survival10Disaster {
    Rats,
    Freeze {
        #[serde(default = "freeze_days")]
        days: u32,
    },
    Flood,
    Drought {
        #[serde(default = "drought_days")]
        days: u32,
    },
}

fn freeze_days() -> u32 {
    2
}
fn drought_days() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survival10DisasterNotice {
    pub id: u32,
    pub at: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_day: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Survival10Phase {
    Waiting,
    Day,
    Gather,
    Speech,
    Vote,
    LastWords,
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
pub struct Survival10Farewell {
    pub seat: u32,
    pub at: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u32>,
    pub skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Survival10Ballot {
    pub voter: u32,
    /// 为防止抢先投票者影响其他人，揭票前为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u32>,
    /// 投票超时后由系统补投；与本人主动提交的票在揭票后分开显示。
    #[serde(default)]
    pub automatic: bool,
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
    #[serde(default)]
    pub farewells: Vec<Survival10Farewell>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disasters: Vec<Survival10DisasterNotice>,
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
    /// 当日劳动达标所需分钟，按真实有效劳动累计；旧档缺省时为八小时。
    #[serde(default = "default_labor_minutes")]
    pub labor_required_minutes: u32,
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

fn default_labor_minutes() -> u32 {
    480
}
