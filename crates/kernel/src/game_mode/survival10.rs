//! 聪明人小镇：七人、十日、21 袋公共米粮、每天最多产 4 袋。
//! 回合裁判只通过 `mode_action` 增减比赛用米，不能让通用 Props 创造比赛资源。
//! 私密信件与夜间目击只进入有关人员的 Journal/Context，不进入公共 World log。
use super::Rules;
use crate::error::{KernelError, Result};
use crate::{World, DAY};
use protocol::{
    EventInfo, EventSpeech, FactKind, GameModeAction, GameModeContext, GameModeEvent, GameModeView,
    ManuscriptKind, ManuscriptVisibility, Survival10Context, Survival10Disaster,
    Survival10DisasterNotice, Survival10Farewell, Survival10Line, Survival10Phase as Phase,
    Survival10Seat, Survival10View,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DAWN: f64 = 480.0;
const DINNER: f64 = 1080.0;
const MEETING: f64 = 1110.0;
/// 白天结算给模型留的时间：醒来（或上一晚结束后）先广播一次，再等这么久才统一结算。
/// 这段时间世界照 gap 速度走，host 一旦有人排队等模型就停钟，七个人按顺序回话都来得及；
/// 全员已经交了白天动作也不提前结算，给还想发悄悄话的人留出时间。
const DAY_NOTICE: f64 = 180.0;
const SPEECH_LIMIT: f64 = 12.0;
const VOTE_LIMIT: f64 = 25.0;
const JURY_LIMIT: f64 = 40.0;
const EPS: f64 = 1e-6;
/// 公共粮仓在地图上的地点与干粮袋动作：玩法里的 `public_rice` 每次变动都写进这只袋子的余量，
/// 地图上看到的就是账上的数。地点来自预制小镇的 `market`（`market_1`）。
const GRANARY: &str = "market_1";
const GRANARY_ACTION: &str = "take";
/// 地图上的真实取粮动作。`take` / `ledger` 只是玩法同步到道具上的账本数字，不能当实际行动。
const COLLECT_ACTION: &str = "collect";
const COLLECT_MINUTES: f64 = 10.0;
const PANTRY_PREFIX: &str = "user:s10-home_pantry_v2_";
const PANTRY_ACTION: &str = "ledger";
/// 十日生存里每天需要完成的劳动时长：随时可以开始，按当天真实有效劳动累计八小时，不要求准时到田。
const WORK_REQUIRED: f64 = 480.0;
const MAX_WORKERS: usize = 4;
/// 今日工作从第一次到田里干活开始记；来过农田后，人离开就停表，回来接着累计。
/// 农田是任何带 `labor` 的日常动作所在地点，不依赖地图处所编号。
const MAX_TEXT: usize = 400;
const NIGHT_REMINDER: &str =
    "夜间行动将在一小时内结束：还没有提交睡觉、偷公仓、偷别人家或守家；不提交就本夜没有行动。";
const LEGACY_NIGHT_REMINDER: &str =
    "夜间行动还有一个多小时结束：还没有提交睡觉、偷公仓、偷别人家或守家；不提交就本夜没有行动。";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Seat {
    id: String,
    name: String,
    rice: u32,
    hungry_streak: u8,
    out: Option<String>,
    out_day: Option<u32>,
    private_notes: Vec<String>,
    #[serde(default)]
    last_guard_day: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum DayOrder {
    #[default]
    Work,
    Take,
}

impl Default for DayAttempt {
    fn default() -> Self {
        Self {
            order: DayOrder::default(),
            worked: 0.0,
            reached: false,
            claimed: false,
            locked: false,
            collected: false,
            _interrupted: false,
            farm: None,
            harvested: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct DayAttempt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    farm: Option<String>,
    #[serde(default)]
    harvested: bool,
    #[serde(default)]
    order: DayOrder,
    /// 工作：实际有效劳动的时长（可以带不足一分钟的零头）。劳动中不能交谈；主动结束劳动
    /// 后在白天内仍可回到田里继续累计，入账和八小时判定只看其中已满的整分钟。
    #[serde(default)]
    worked: f64,
    /// 工作：今天到过农田。按普通动作下地，或已提交工作并走到农田都算。
    #[serde(default)]
    reached: bool,
    /// 领米：真正完成了粮仓领取动作。到账后就已从公仓扣除，不因之后离开而追回。
    #[serde(default)]
    claimed: bool,
    /// 工作或领米的机会一旦登记就不再改选，即使动作后来中断也不能改领另一种。
    #[serde(default)]
    locked: bool,
    /// 粮仓的一次 `取粮` 已经完成过；同一个动作结束后的余留时间不能重复记账。
    #[serde(default)]
    collected: bool,
    /// 只用于忽略曾短暂被写出的旧字段；新存档不再写它。
    #[serde(default, skip_serializing)]
    _interrupted: bool,
}

/// 旧存档把 day_orders 写成 `{座号: "work" | "take"}`，这里同时读两种写法；
/// 旧的选择只说明登记过，仍要重新走真实劳动或领取动作，不凭旧存档补发粮食。
impl<'de> Deserialize<'de> for DayAttempt {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Old(DayOrder),
            Current(Current),
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Current {
            #[serde(default)]
            farm: Option<String>,
            #[serde(default)]
            harvested: bool,
            #[serde(default)]
            order: DayOrder,
            #[serde(default)]
            worked: f64,
            #[serde(default)]
            reached: bool,
            #[serde(default)]
            claimed: bool,
            #[serde(default)]
            locked: bool,
            #[serde(default)]
            collected: bool,
            /// 只为了读回曾短暂写出 `interrupted` 的存档；整日失效的旧判定已经删除。
            #[serde(default, rename = "interrupted")]
            _interrupted: bool,
        }
        match Stored::deserialize(deserializer)? {
            Stored::Old(order) => Ok(DayAttempt {
                order,
                locked: true,
                ..DayAttempt::default()
            }),
            Stored::Current(c) => Ok(DayAttempt {
                farm: c.farm,
                harvested: c.harvested,
                order: c.order,
                worked: c.worked,
                reached: c.reached,
                claimed: c.claimed,
                locked: c.locked,
                collected: c.collected,
                _interrupted: false,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum NightStage {
    /// 只是登记了今夜选择，还没有完成对应的真实动作。
    #[default]
    Chosen,
    /// 人进了自己家的床，正在睡；醒来后不再是有效睡觉。
    Sleeping,
    /// 人此刻在别人卧室里翻私粮袋。
    Entering(usize),
    /// 已点名要偷谁家，正在找那家的私粮袋。
    Searching(usize),
    /// 人正在公仓取米。
    Taking,
    /// 人回到自家并保持清醒守夜；离开或睡着就失效。
    Guarding,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct NightAttempt {
    #[serde(default)]
    order: NightOrder,
    #[serde(default)]
    stage: NightStage,
    /// 守家时亲眼看到的入室者；只进本人的私密经历。
    seen: Vec<usize>,
    /// 本次行动已经完成并结算，不能再在结算时重复取粮。
    #[serde(default)]
    settled: bool,
    /// 旧存档的入室计时字段；现在只在真实私粮袋动作完成时设置，不凭到场或旧计时结算。
    #[serde(default)]
    entry_ready_at: Option<f64>,
    /// 上床睡觉的动作自然完成过；之后离开床或做别的事只算“睡过”，不再算睡到清晨。
    #[serde(default)]
    slept: bool,
    /// 刚才是否在床上；用来发现离开床或睡眠被打断。
    #[serde(default)]
    on_bed: bool,
    /// 本夜上过床后又离开；即使当时已经自然睡完，也不再算睡到清晨。
    #[serde(default)]
    sleep_left: bool,
    #[serde(default)]
    collided: Vec<usize>,
    #[serde(default)]
    collision_reported: bool,
    #[serde(default)]
    collision_notified: bool,
}

/// 旧夜行动曾存成 `"steal_home"`、`{"steal_home": 2}` 这类枚举写法；新档是结构。
/// 两种都读入，但旧档的登记不视为已完成，不会凭存档补发粮食。
impl<'de> Deserialize<'de> for NightAttempt {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Old(NightOrder),
            Current(Current),
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Current {
            #[serde(default)]
            order: NightOrder,
            #[serde(default)]
            stage: NightStage,
            #[serde(default)]
            seen: Vec<usize>,
            #[serde(default)]
            settled: bool,
            #[serde(default)]
            entry_ready_at: Option<f64>,
            #[serde(default)]
            slept: bool,
            #[serde(default)]
            on_bed: bool,
            #[serde(default)]
            sleep_left: bool,
            #[serde(default)]
            collided: Vec<usize>,
            #[serde(default)]
            collision_reported: bool,
            #[serde(default)]
            collision_notified: bool,
        }
        match Stored::deserialize(deserializer)? {
            Stored::Old(order) => Ok(NightAttempt {
                order,
                stage: NightStage::Chosen,
                ..NightAttempt::default()
            }),
            Stored::Current(c) => Ok(NightAttempt {
                order: c.order,
                stage: c.stage,
                seen: c.seen,
                settled: c.settled,
                entry_ready_at: c.entry_ready_at,
                slept: c.slept,
                on_bed: c.on_bed,
                sleep_left: c.sleep_left,
                collided: c.collided,
                collision_reported: c.collision_reported,
                collision_notified: c.collision_notified,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum NightOrder {
    Sleep,
    StealPublic,
    StealHome(usize),
    #[default]
    Guard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Whisper {
    from: usize,
    to: usize,
    sent_day: u32,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WhisperStatus {
    Pending,
    Delivered,
    RecipientEliminated,
    GameEnded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WhisperRecord {
    id: u64,
    from: String,
    from_name: String,
    to: String,
    to_name: String,
    sent_day: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_at: Option<f64>,
    content: String,
    delivery_at: f64,
    status: WhisperStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SupplyChange {
    at: f64,
    prop: String,
    action: String,
    before: u32,
    after: u32,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DisasterRecord {
    notice: Survival10DisasterNotice,
    event: Survival10Disaster,
    public_before: u32,
    public_after: u32,
    private_losses: BTreeMap<usize, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ballot {
    target: usize,
    /// 这一票的超时来源；主动提交和系统代投在揭票后必须能分辨。
    #[serde(default)]
    automatic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Survival10 {
    seed: u32,
    first_day: u32,
    day: u32,
    phase: Phase,
    #[serde(deserialize_with = "deserialize_until")]
    until: f64,
    public_rice: u32,
    produced_today: u32,
    consumed: u32,
    seats: Vec<Seat>,
    day_orders: BTreeMap<usize, DayAttempt>,
    night_orders: BTreeMap<usize, NightAttempt>,
    #[serde(default)]
    night_reminded: Vec<usize>,
    whispers_used: Vec<usize>,
    pending_whispers: Vec<Whisper>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    whisper_archive: Vec<WhisperRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    supply_changes: Vec<SupplyChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    disasters: Vec<DisasterRecord>,
    ballots: BTreeMap<usize, Ballot>,
    turn: Option<usize>,
    speakers: Vec<usize>,
    #[serde(default)]
    runoff_candidates: Option<Vec<usize>>,
    #[serde(default)]
    speech_round: u64,
    lines: Vec<Survival10Line>,
    revealed: bool,
    notice: String,
    history: Vec<String>,
    last_public_stolen: u32,
    last_hungry: Vec<usize>,
    winner: Option<usize>,
    /// 已经出局、还在等陈词的人；必须优先于其他阶段切换结算。
    #[serde(default)]
    departures: Vec<usize>,
    /// 旧档曾在进入陈词前记录“去路”；现由队列和存活人数决定下一步。保留字段只为读回旧档，
    /// 新档不再写它。
    #[serde(default)]
    resume_phase: Option<Phase>,
    #[serde(default)]
    farewells: Vec<Survival10Farewell>,
    /// 夜间偷粮动作真实发生的取粮区间；转账前先看同目标是否存在重叠。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    theft_windows: Vec<TheftWindow>,
    /// 粮仓所在地点。真实小镇固定是 `market_1`；测试地图没有这个地点时由测试改成自己的。
    #[serde(default = "default_granary_locale")]
    granary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TheftWindow {
    seat: usize,
    day: u32,
    target: String,
    /// 最先登记、并且确实参与撞车的其他人；旧档没有这一项时由 seat 推出。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    others: Vec<usize>,
    /// 这次取粮动作真正开始和结束的世界时间。
    from: f64,
    to: f64,
}

fn default_granary_locale() -> String {
    GRANARY.to_string()
}

fn deserialize_until<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(0.0))
}

impl Survival10 {
    fn crop_failure(&self, day: u32) -> bool {
        self.disasters.iter().any(|record| {
            record
                .notice
                .first_day
                .zip(record.notice.last_day)
                .is_some_and(|(first, last)| (first..=last).contains(&day))
        })
    }

    fn disaster_status(&self) -> String {
        self.disasters
            .iter()
            .filter(|record| record.notice.last_day.is_some_and(|last| last >= self.day))
            .map(|record| record.notice.text.as_str())
            .collect::<Vec<_>>()
            .join("；")
    }

    fn trigger_disaster(
        &mut self,
        world: &mut World,
        event: &Survival10Disaster,
    ) -> Result<String> {
        if self.phase == Phase::Over || self.living().is_empty() {
            return Err(KernelError::action("本局已结束，不能触发天灾"));
        }
        let period = match event {
            Survival10Disaster::Freeze { days } | Survival10Disaster::Drought { days } => {
                let first = self.day + 1;
                let remaining = 10u32.saturating_sub(self.day);
                if self.day >= 10 || !(1..=2).contains(days) || *days > remaining {
                    return Err(KernelError::action(
                        format!(
                            "绝收须持续1至2个比赛日，从下一比赛日开始且不能超过第10天；本局还剩{remaining}个比赛日"
                        ),
                    ));
                }
                Some((first, first + days - 1))
            }
            _ => None,
        };
        let board = world
            .topo
            .props()
            .iter()
            .filter(|prop| prop.bulletin_board && !prop.withdrawn)
            .min_by_key(|prop| (prop.id != "user:s10-disaster-board", &prop.id))
            .map(|prop| (prop.id.clone(), prop.locale.clone()))
            .ok_or_else(|| KernelError::action("没有可用公告栏，天灾未执行；请先放置公告栏"))?;
        let id = self.disasters.len() as u32 + 1;
        let document = format!("user:s10-disaster-{}-{}-{id}", self.seed, self.first_day);
        if world.topo.prop(&document).is_some()
            || world.contents().documents.contains_key(&document)
        {
            return Err(KernelError::action("天灾公告编号已存在，事件未执行"));
        }
        let mut random_state = u64::from(self.seed) ^ (u64::from(id) << 32) ^ 0x9e3779b97f4a7c15;
        let mut draw = |count: u32| -> u32 {
            loop {
                random_state ^= random_state << 13;
                random_state ^= random_state >> 7;
                random_state ^= random_state << 17;
                let value = (random_state >> 32) as u32;
                if value >= count.wrapping_neg() % count {
                    return value % count;
                }
            }
        };
        let before = self.public_rice;
        let mut losses = BTreeMap::new();
        let (after, text) = match event {
            Survival10Disaster::Rats => {
                let loss = (3 + draw(8)).min(before);
                (
                    before - loss,
                    format!(
                        "鼠患：公仓损失{loss}袋粮，剩余{}袋；这是天灾损耗，不是偷粮。",
                        before - loss
                    ),
                )
            }
            Survival10Disaster::Flood => {
                let mut candidates = self.living();
                let count = (1 + draw(3) as usize).min(candidates.len());
                for _ in 0..count {
                    let index = draw(candidates.len() as u32) as usize;
                    let seat = candidates.swap_remove(index);
                    losses.insert(seat, self.seats[seat].rice);
                }
                let loss = before / 3;
                (before - loss, format!("洪涝：公仓腐坏{loss}袋粮，剩余{}袋；部分住宅受灾，私人损失不公开。这是天灾损耗，不是偷粮。", before - loss))
            }
            Survival10Disaster::Freeze { .. } | Survival10Disaster::Drought { .. } => {
                let (first, last) = period.expect("绝收已校验日期");
                let name = if matches!(event, Survival10Disaster::Freeze { .. }) {
                    "雪灾冻结"
                } else {
                    "旱灾"
                };
                let recovery = if last == 10 {
                    "本局结束前不再恢复".into()
                } else {
                    format!("第{}天恢复正常产粮", last + 1)
                };
                (before, format!("{name}预警：第{first}至{last}比赛日农田彻底绝收，报名和劳动均不产粮；{recovery}。请自行安排领米和储粮，晚餐照常扣粮；仍可自愿报名劳动，报名后不能改领米。"))
            }
        };
        let board_locale = board.1.clone();
        let text = format!("{} · {text}", world.clock());
        world.create_content(
            document.clone(),
            board.1,
            format!("赛事天灾公告{id}"),
            "赛事裁判".into(),
            text.clone(),
            Some(board.0),
        )?;
        self.public_rice = after;
        for (&seat, &loss) in &losses {
            self.seats[seat].rice = 0;
            self.remember(
                world,
                seat,
                format!(
                    "洪涝浸坏了你的私粮，损失{loss}袋，当前0袋。此私人损失未向其他参赛者公开。"
                ),
            );
        }
        self.sync_all_supplies(world);
        // 公开公告只说“部分住宅受灾”；给玩家的剧本另记裁判全知结果，不写进任何人的记忆。
        let victims: Vec<_> = losses
            .iter()
            .map(|(&seat, &loss)| format!("{}家私粮{loss}袋清零", self.label(seat)))
            .collect();
        let outcome = if victims.is_empty() {
            format!("裁判全知：本场{event:?}没有住宅私粮损失。")
        } else {
            format!(
                "裁判全知：住宅受灾 {}/{} 户——{}；这些私人损失未向其他参赛者公开。",
                victims.len(),
                self.living().len(),
                victims.join("、")
            )
        };
        world.manuscript_fact(
            ManuscriptKind::Announcement,
            ManuscriptVisibility::Private,
            None,
            Some(&board_locale),
            outcome,
            losses
                .keys()
                .map(|&seat| self.seats[seat].id.clone())
                .collect(),
            format!(
                "event:{}-disaster-{id}",
                (world.minutes() * 60.0).round() as i64
            ),
        );
        self.disasters.push(DisasterRecord {
            notice: Survival10DisasterNotice {
                id,
                at: world.minutes(),
                text: text.clone(),
                first_day: period.map(|period| period.0),
                last_day: period.map(|period| period.1),
            },
            event: event.clone(),
            public_before: before,
            public_after: after,
            private_losses: losses,
        });
        self.announce(world, text.clone(), false);
        self.announce_behind_the_bench(
            world,
            format!("赛事天灾公告（已张贴公告栏）：{text}"),
            format!(
                "event:{}-disaster-{id}-notice",
                (world.minutes() * 60.0).round() as i64
            ),
        );
        for seat in self.living() {
            self.remember(
                world,
                seat,
                format!(
                    "赛事公告送达：{text}（已张贴公告栏；不代表你亲眼看见灾害或已经到场阅读。）"
                ),
            );
            world.notify(
                &self.seats[seat].id,
                format!("赛事公告栏发布天灾通知：{text}"),
            );
        }
        Ok(format!("已执行并张贴公告、通知在场参赛者：{text}"))
    }

    pub(super) fn open(world: &mut World, seed: u32) -> Result<Self> {
        Self::migrate_collect_actions(world);
        let mut ids: Vec<&String> = world
            .actors
            .iter()
            .filter(|(_, a)| !a.is_avatar() && a.present() && !a.needs.body.dead)
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        if ids.len() != 7 {
            return Err(KernelError::action(format!(
                "聪明人小镇固定七位参赛者，当前有效居民为 {} 位",
                ids.len()
            )));
        }
        let seats = ids
            .iter()
            .map(|id| Seat {
                id: (*id).clone(),
                name: world.actors[*id].name.clone(),
                rice: 1,
                hungry_streak: 0,
                out: None,
                out_day: None,
                private_notes: Vec::new(),
                last_guard_day: None,
            })
            .collect();
        let now = world.minutes();
        let today = (now / DAY).floor() as u32;
        let first_day = if crate::time_of_day(now) < DAWN {
            today
        } else {
            today + 1
        };
        let mut mode = Self {
            seed: seed.max(1),
            first_day,
            day: 0,
            phase: Phase::Waiting,
            until: f64::from(first_day) * DAY + DAWN,
            public_rice: 21,
            produced_today: 0,
            consumed: 0,
            seats,
            day_orders: BTreeMap::new(),
            night_orders: BTreeMap::new(),
            night_reminded: Vec::new(),
            whispers_used: Vec::new(),
            pending_whispers: Vec::new(),
            whisper_archive: Vec::new(),
            supply_changes: Vec::new(),
            disasters: Vec::new(),
            ballots: BTreeMap::new(),
            turn: None,
            speakers: Vec::new(),
            runoff_candidates: None,
            speech_round: 0,
            lines: Vec::new(),
            revealed: false,
            notice: "七位居民即将进入十日生存实验。".into(),
            history: Vec::new(),
            last_public_stolen: 0,
            last_hungry: Vec::new(),
            winner: None,
            departures: Vec::new(),
            resume_phase: None,
            farewells: Vec::new(),
            theft_windows: Vec::new(),
            granary: GRANARY.to_string(),
        };
        mode.sync_all_supplies(world);
        for seat in mode.living() {
            mode.remember(
                world,
                seat,
                "本局开局配给每人1袋私粮，仅新局配给一次；第一天晚餐照常消耗1袋。".into(),
            );
        }
        Ok(mode)
    }

    /// 旧小镇存档保留了当时的取粮动作定义。取粮不抢操作位，由玩法在完成时统一判定撞车；
    /// 新局与读档都走这里，避免旧动作让通用占用检查替玩法决定先后。
    fn migrate_collect_actions(world: &mut World) {
        let mut migrated = false;
        for prop in world.topo.props_mut().filter(|prop| !prop.withdrawn) {
            if !prop.verbs.iter().any(|verb| {
                verb.action.as_ref().is_some_and(|action| {
                    action.mode_managed
                        && matches!(action.id.as_str(), GRANARY_ACTION | PANTRY_ACTION)
                })
            }) {
                continue;
            }
            for verb in &mut prop.verbs {
                let Some(action) = verb.action.as_mut() else {
                    continue;
                };
                if action.id == COLLECT_ACTION && !action.concurrent {
                    action.concurrent = true;
                    migrated = true;
                }
            }
        }
        if migrated {
            world.affordances.rebuild(&world.topo);
        }
    }

    /// 旧档直接装回玩法时也要完成与新局相同的动作迁移。更早的守家记录没有冷却字段，
    /// 只有在当天夜里已经登记守家时才把它补成当天，避免重启一次就绕过下一晚的限制；
    /// 补的是已经发生过的登记，不会给任何新局或旧档补粮。
    fn restore(&mut self, world: &mut World) {
        Self::migrate_collect_actions(world);
        if self.phase == Phase::Over {
            self.end_participation(world);
            return;
        }
        let guard_day = self
            .night_orders
            .iter()
            .filter(|(_, attempt)| attempt.order == NightOrder::Guard)
            .map(|(&seat, _)| seat)
            .collect::<Vec<_>>();
        for seat in guard_day {
            if self
                .seats
                .get(seat)
                .is_some_and(|s| s.last_guard_day.is_none())
            {
                self.seats[seat].last_guard_day = Some(self.day);
            }
        }
    }
    fn interrupt_theft_colliders(&mut self, world: &mut World) {
        let collided = self
            .night_orders
            .iter()
            .filter(|(_, attempt)| attempt.collision_reported && !attempt.collided.is_empty())
            .map(|(&seat, _)| seat)
            .collect::<Vec<_>>();
        for seat in collided {
            let id = self.seats[seat].id.clone();
            if !self.night_orders[&seat].collision_notified {
                let names = self.night_orders[&seat]
                    .collided
                    .iter()
                    .map(|&other| self.label(other))
                    .collect::<Vec<_>>()
                    .join("、");
                let place = match self.night_orders[&seat].order {
                    NightOrder::StealHome(target) => {
                        format!("{}的住宅私粮袋旁", self.label(target))
                    }
                    _ => "公共粮仓取粮区".into(),
                };
                self.remember(world, seat, format!("你在{place}与{names}偷粮撞车，所有撞车者本夜偷粮失败，没有转移粮食，也不能重试。这是你的亲历，不是公开消息。"));
                self.night_orders.get_mut(&seat).unwrap().collision_notified = true;
                // 撞车是几户亲历者之间的半私密结果；观众需要看清谁和谁撞上，镇上其他人仍未知道。
                let mut people: Vec<_> = std::iter::once(seat)
                    .chain(self.night_orders[&seat].collided.iter().copied())
                    .collect();
                people.sort_unstable();
                people.dedup();
                let names = people
                    .iter()
                    .map(|&person| self.label(person))
                    .collect::<Vec<_>>()
                    .join("、");
                let who: Vec<_> = people
                    .iter()
                    .map(|&person| self.seats[person].id.clone())
                    .collect();
                let actor = self.seats[seat].id.clone();
                world.manuscript_fact(
                    ManuscriptKind::Action,
                    ManuscriptVisibility::Private,
                    Some(&actor),
                    None,
                    format!("{names}在{place}偷粮撞车：所有撞车者本夜都没有转移粮食。"),
                    who.clone(),
                    format!(
                        "collision:{}-{}-{}",
                        self.seed,
                        self.day,
                        people
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                );
            }
            if self
                .theft_open_window(world, seat, world.minutes())
                .is_some()
            {
                world.interrupt_actions(&id, "偷粮撞车，本夜行动已消耗");
            }
        }
    }

    fn at(&self, day: u32, minute: f64) -> f64 {
        f64::from(self.first_day + day.saturating_sub(1)) * DAY + minute
    }
    fn living(&self) -> Vec<usize> {
        (0..self.seats.len())
            .filter(|&i| self.seats[i].out.is_none())
            .collect()
    }
    /// 不分配列表的存活人数：`advance` 每拍都会问，不跟着玩家数增加临时分配。
    fn live_count(&self) -> usize {
        self.seats.iter().filter(|seat| seat.out.is_none()).count()
    }
    fn jurors(&self) -> Vec<usize> {
        (0..self.seats.len())
            .filter(|&i| self.seats[i].out.is_some())
            .collect()
    }
    fn label(&self, i: usize) -> String {
        format!("{}号{}", i + 1, self.seats[i].name)
    }
    fn seat_of(&self, id: &str) -> Option<usize> {
        self.seats.iter().position(|s| s.id == id)
    }
    fn target(&self, action: &GameModeAction) -> Result<usize> {
        let raw = action
            .target
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| KernelError::action("必须填写 target（对方座号）"))?;
        // 先认整串（座号、人物编号、姓名），再认开头的座号；「英格丽德」这类以数字起的名字
        // 不能被截成「2 号」。
        let exact = raw
            .parse::<usize>()
            .ok()
            .filter(|&n| n >= 1 && n <= self.seats.len())
            .map(|n| n - 1)
            .or_else(|| self.seats.iter().position(|s| s.id == raw || s.name == raw));
        if let Some(i) = exact {
            return Ok(i);
        }
        // 模型常写「2号」「第3位」「2 号甲」：取开头连续的阿拉伯数字。
        let digits: String = raw.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits
            .parse::<usize>()
            .ok()
            .filter(|&n| n >= 1 && n <= self.seats.len())
            .map(|n| n - 1)
            .ok_or_else(|| KernelError::action(format!("无效 target {raw}")))
    }
    /// 地图上的公仓账本。预制镇把粮仓放在 `market_1`，实例编号随镇生成而不同，所以按玩法账本动作找道具，
    /// 不能假设 `market_1` 上那只要么不存在、要么就是唯一一只。旧地图没有账本时仍按地点同步。
    fn granary_ledger(&self, world: &World) -> Option<String> {
        world
            .topo
            .props()
            .iter()
            .filter(|p| !p.withdrawn && p.locale == self.granary)
            .find(|p| {
                p.verbs.iter().any(|v| {
                    v.action
                        .as_ref()
                        .is_some_and(|a| a.mode_managed && a.id == GRANARY_ACTION)
                })
            })
            .map(|p| p.id.clone())
    }
    fn granary_point(&self, world: &World) -> String {
        world
            .topo
            .prop(&self.granary_ledger(world).unwrap_or_default())
            .map_or_else(|| self.granary.clone(), |p| p.locale.clone())
    }
    /// 把账上的公仓米数写到地图上的干粮袋。地图上没有这只袋子（别的图、旧档）就什么也不做。
    fn sync_granary(&self, world: &mut World) {
        let ledger = self.granary_ledger(world);
        let _ = world.set_map_supply(
            ledger.as_deref(),
            &self.granary,
            GRANARY_ACTION,
            self.public_rice,
        );
    }
    fn sync_pantry(&self, world: &mut World, i: usize) {
        let id = format!("{PANTRY_PREFIX}{}", i + 1);
        let _ = world.set_map_supply(Some(&id), "", PANTRY_ACTION, self.seats[i].rice);
    }
    fn sync_all_supplies(&self, world: &mut World) {
        self.sync_granary(world);
        for i in 0..self.seats.len() {
            self.sync_pantry(world, i);
        }
    }
    fn remember(&mut self, world: &mut World, i: usize, text: String) {
        let now = world.minutes();
        self.seats[i].private_notes.push(text.clone());
        if self.seats[i].private_notes.len() > 128 {
            self.seats[i].private_notes.remove(0);
        }
        // 只通知本人；切勿使用 world.log 写入私密事实。
        let id = self.seats[i].id.clone();
        if world.actor(&id).is_some() {
            world.actor_mut(&id).journal.fact(
                now,
                FactKind::Observation,
                text,
                vec![format!("survival10:{}", self.seed)],
                None,
                None,
            );
        }
    }
    /// 只有玩家能读到、居民不另知情的结果：写进剧本时带当事人的经历原文，
    /// 但不改任何人的记忆。`open` 用于观众本就会在公开场合看到的事。
    fn outcome(&self, world: &mut World, seat: usize, text: String, source: String, open: bool) {
        world.manuscript_fact(
            ManuscriptKind::Action,
            if open {
                ManuscriptVisibility::Public
            } else {
                ManuscriptVisibility::Private
            },
            Some(&self.seats[seat].id),
            None,
            text,
            vec![self.seats[seat].id.clone()],
            source,
        );
    }
    /// 世界已经公开张贴的公告正文；不借条目改写居民记忆。
    fn announce_behind_the_bench(&self, world: &mut World, text: String, source: String) {
        world.manuscript_fact(
            ManuscriptKind::Announcement,
            ManuscriptVisibility::Public,
            None,
            None,
            text,
            Vec::new(),
            source,
        );
    }
    /// 只有旁观者能看到、没有向其他参赛者公开的结果：仍可筛选与导出，但「仅公开」不显示。
    fn announce_private_fact(&self, world: &mut World, text: String, source: String) {
        world.manuscript_fact(
            ManuscriptKind::Announcement,
            ManuscriptVisibility::Private,
            None,
            None,
            text,
            Vec::new(),
            source,
        );
    }
    /// 已确认的玩法选择：`open` 表示这件事镇上的人本来就会看到（登记白天行动），
    /// 否则只是玩家旁观可见，居民仍按各自的在场与目击规则知道。
    fn choice(&self, world: &mut World, seat: usize, text: String, source: String, open: bool) {
        world.manuscript_fact(
            ManuscriptKind::Choice,
            if open {
                ManuscriptVisibility::Public
            } else {
                ManuscriptVisibility::Private
            },
            Some(&self.seats[seat].id),
            None,
            text,
            vec![self.seats[seat].id.clone()],
            source,
        );
    }
    fn archive_whisper(&mut self, letter: &Whisper, sent_at: Option<f64>) -> u64 {
        let id = self
            .whisper_archive
            .last()
            .map_or(1, |record| record.id + 1);
        self.whisper_archive.push(WhisperRecord {
            id,
            from: self.seats[letter.from].id.clone(),
            from_name: self.seats[letter.from].name.clone(),
            to: self.seats[letter.to].id.clone(),
            to_name: self.seats[letter.to].name.clone(),
            sent_day: letter.sent_day,
            sent_at,
            content: letter.content.clone(),
            delivery_at: self.at(letter.sent_day + 1, DAWN),
            status: WhisperStatus::Pending,
            resolved_at: None,
        });
        id
    }
    fn close_whispers(&mut self, now: f64) {
        let pending = std::mem::take(&mut self.pending_whispers);
        for letter in pending {
            let archive_id = letter
                .archive_id
                .unwrap_or_else(|| self.archive_whisper(&letter, None));
            if let Some(record) = self
                .whisper_archive
                .iter_mut()
                .find(|record| record.id == archive_id)
            {
                record.status = if self.seats[letter.to].out.is_some() {
                    WhisperStatus::RecipientEliminated
                } else {
                    WhisperStatus::GameEnded
                };
                record.resolved_at = Some(now);
            }
        }
    }
    fn is_labor_spot(spot: &crate::affordance::Spot) -> bool {
        spot.labor
            || spot.action.as_ref().is_some_and(|a| {
                !a.mode_managed
                    && a.during
                        .iter()
                        .any(|e| matches!(e, crate::props::Effect::Labor))
            })
    }
    fn is_farm(world: &World, prop: &str) -> bool {
        world.topo.prop(prop).is_some_and(|p| {
            !p.withdrawn
                && p.definition.as_deref().is_some_and(|definition| {
                    definition.split('@').next() == Some("core:survival10-farm")
                })
                && world.inventory.on_map(&world.props, prop)
        })
    }
    fn farms(world: &World) -> Vec<String> {
        let mut farms: Vec<_> = world
            .topo
            .props()
            .iter()
            .filter(|prop| Self::is_farm(world, &prop.id))
            .map(|prop| prop.id.clone())
            .collect();
        farms.sort();
        farms.truncate(MAX_WORKERS);
        farms
    }
    fn registered_today(&self) -> usize {
        self.day_orders
            .values()
            .filter(|attempt| attempt.order == DayOrder::Work && attempt.locked)
            .count()
    }
    fn work_farm(&self, world: &World, seat: usize, requested: Option<&str>) -> Result<String> {
        if self.phase != Phase::Day || self.seats[seat].out.is_some() {
            return Err(KernelError::action("只有白天的在场参赛者可以报名劳动"));
        }
        let attempt = self.day_orders.get(&seat);
        if attempt.is_some_and(|attempt| attempt.order != DayOrder::Work) {
            return Err(KernelError::action("今天已选择领米，不能再报名劳动"));
        }
        if let Some(farm) = attempt.and_then(|attempt| attempt.farm.as_deref()) {
            if requested.is_some_and(|requested| requested != farm) {
                return Err(KernelError::action(format!("请使用分配给你的田块 {farm}")));
            }
            if !Self::is_farm(world, farm) {
                return Err(KernelError::action(
                    "分配给你的田块已不可用，今日名额仍保留",
                ));
            }
            if attempt.is_some_and(|attempt| Self::worked_minutes(attempt) + EPS >= WORK_REQUIRED) {
                return Err(KernelError::action(
                    "今天已完成八小时劳动，请等待白天结束时收成入公仓",
                ));
            }
            return Ok(farm.into());
        }
        if attempt.is_none() && self.registered_today() >= MAX_WORKERS {
            return Err(KernelError::action(
                "今日已有四人报名劳动，名额已满；你仍可选择领米",
            ));
        }
        Self::farms(world)
            .into_iter()
            .find(|farm| {
                requested.is_none_or(|requested| farm == requested)
                    && !self
                        .day_orders
                        .values()
                        .any(|attempt| attempt.farm.as_ref() == Some(farm))
            })
            .ok_or_else(|| KernelError::action("没有可报名的空闲田块，请选择其他田块或领米"))
    }
    fn register_work(
        &mut self,
        world: &World,
        seat: usize,
        requested: Option<&str>,
    ) -> Result<String> {
        let farm = self.work_farm(world, seat, requested)?;
        let attempt = self.day_orders.entry(seat).or_default();
        attempt.locked = true;
        attempt.farm = Some(farm.clone());
        Ok(farm)
    }
    fn assigned_labor(&self, world: &World, seat: usize, spot: &crate::affordance::Spot) -> bool {
        Self::is_farm(world, &spot.prop)
            && Self::is_labor_spot(spot)
            && self.day_orders.get(&seat).is_some_and(|attempt| {
                attempt.order == DayOrder::Work
                    && attempt.farm.as_deref() == Some(spot.prop.as_str())
            })
    }
    /// 人此刻是否真的在农田干活。劳动中不能交谈；空等、走到半路或做别的动作都不算劳动。
    fn field(&self, world: &World, i: usize, now: f64) -> bool {
        self.field_started(world, i, now).is_some()
    }
    /// 当前这一轮下地干活从哪一刻开始。田块、动作和位置都相符才给出起点，右上角与累计共用。
    fn field_started(&self, world: &World, i: usize, now: f64) -> Option<f64> {
        let a = world.actor(&self.seats[i].id)?;
        if a.current.interaction.is_some()
            || a.current.is_travel()
            || !matches!(a.current.kind, crate::world::ActKind::Timed { .. })
            || a.current.remaining(now).is_none_or(|m| m <= EPS)
        {
            return None;
        }
        let spot = crate::physiology::body_spot(world, a)?;
        let minutes = spot.minutes.max(EPS);
        (self.assigned_labor(world, i, spot)
            && spot.action.is_some()
            // 动作开始后超过它自己标称的时长，就不再是一段还能推进的劳动。允许上一片末尾
            // 刚开始的那一小段跨进下一片，但不把更早的一段留在场上继续计时。
            && now - a.current.started_at <= minutes + crate::activity::SLICE_MINUTES + EPS
            && world
                .topo
                .prop(&spot.prop)
                .is_some_and(|p| p.locale == a.locus.locale)
            && world.sees_prop(&a.id, &spot.prop))
        .then_some(a.current.started_at)
    }
    /// 这一天真正干满的整分钟。中断时留下、不够一分钟的零头不算，也不能靠零头凑够八小时。
    fn worked_minutes(attempt: &DayAttempt) -> f64 {
        (attempt.worked + EPS).floor().min(WORK_REQUIRED)
    }
    /// 右上角与田里读取同一份真实切片累计，劳动中显示零头，中断后显示已累计的整分钟。
    fn labor_progress(&self, world: &World, i: usize) -> f64 {
        self.day_orders
            .get(&i)
            .filter(|attempt| attempt.order == DayOrder::Work)
            .map_or(0.0, |attempt| {
                if self.field(world, i, world.minutes()) {
                    attempt.worked.min(WORK_REQUIRED)
                } else {
                    Self::worked_minutes(attempt)
                }
            })
    }
    fn is_sleep(spot: &crate::affordance::Spot) -> bool {
        spot.action
            .as_ref()
            .is_some_and(|a| a.completion == crate::props::Completion::Sleep)
    }
    /// 人此刻在床上睡觉（已经自然睡完仍留在床上也算）；离开床后，这段睡眠不再有效。
    fn in_own_bed(&self, world: &World, i: usize) -> bool {
        let Some(a) = world.actor(&self.seats[i].id) else {
            return false;
        };
        if a.current.interaction.is_some()
            || a.current.is_travel()
            || self.home_of(world, i).as_deref() != Some(a.locus.locale.as_str())
        {
            return false;
        }
        let spot = world.affordances.for_prop(
            a.current
                .action
                .as_ref()
                .and_then(|b| b.draft.prop_id.as_deref())
                .unwrap_or(""),
            &a.current.verb,
        );
        spot.is_some_and(|spot| {
            Self::is_sleep(spot)
                && !Self::is_labor_spot(spot)
                && world.sees_prop(&a.id, &spot.prop)
                && world
                    .prop_occupants(&spot.prop)
                    .iter()
                    .any(|id| id == &a.id)
        })
    }
    fn is_granary_collect(world: &World, spot: &crate::affordance::Spot) -> bool {
        spot.action
            .as_ref()
            .is_some_and(|action| action.id == COLLECT_ACTION)
            && world.topo.prop(&spot.prop).is_some_and(|prop| {
                prop.verbs.iter().any(|verb| {
                    verb.action
                        .as_ref()
                        .is_some_and(|action| action.mode_managed && action.id == GRANARY_ACTION)
                })
            })
    }
    fn check_collection(
        &self,
        world: &World,
        actor: &str,
        spot: &crate::affordance::Spot,
    ) -> Result<usize> {
        let seat = self
            .seat_of(actor)
            .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
        if self.seats[seat].out.is_some() {
            return Err(KernelError::action("你已出局，不能领取公仓米粮"));
        }
        if self.granary_ledger(world).as_deref() != Some(spot.prop.as_str()) {
            return Err(KernelError::action("这不是本局公共粮仓，请到指定粮仓取粮"));
        }
        match self.phase {
            Phase::Day => match self.day_orders.get(&seat) {
                Some(attempt) if attempt.order == DayOrder::Work => {
                    return Err(KernelError::action("今天已选择工作，不能再领米"));
                }
                Some(attempt) if attempt.claimed => {
                    return Err(KernelError::action("今天已经领取过一袋米，不能重复领取"));
                }
                Some(attempt) if attempt.order == DayOrder::Take => {}
                _ => {
                    return Err(KernelError::action(
                        "请先通过 take_rice 登记今日领米，再执行十分钟取粮",
                    ))
                }
            },
            Phase::Night => match self.night_orders.get(&seat) {
                Some(attempt) if attempt.order == NightOrder::StealPublic && !attempt.settled => {}
                _ => return Err(KernelError::action("今夜没有可执行的偷公仓行动")),
            },
            _ => return Err(KernelError::action("当前阶段不能在公共粮仓取粮")),
        }
        if self.public_rice == 0 {
            return Err(KernelError::action(
                "公共粮仓已经空了，没有米可取；本次没有领取成功",
            ));
        }
        Ok(seat)
    }
    fn is_pantry_collect(world: &World, spot: &crate::affordance::Spot) -> bool {
        spot.action
            .as_ref()
            .is_some_and(|action| action.id == COLLECT_ACTION)
            && world.topo.prop(&spot.prop).is_some_and(|prop| {
                prop.verbs.iter().any(|verb| {
                    verb.action
                        .as_ref()
                        .is_some_and(|action| action.mode_managed && action.id == PANTRY_ACTION)
                })
            })
    }
    fn check_home_collection(
        &self,
        world: &World,
        actor: &str,
        spot: &crate::affordance::Spot,
    ) -> Result<(usize, usize)> {
        let seat = self
            .seat_of(actor)
            .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
        if self.phase != Phase::Night || self.seats[seat].out.is_some() {
            return Err(KernelError::action(
                "私粮袋只能在夜间登记偷别人家后执行偷粮，不能当作白天领米",
            ));
        }
        let attempt = self
            .night_orders
            .get(&seat)
            .filter(|attempt| !attempt.settled)
            .ok_or_else(|| KernelError::action("今夜没有可执行的入室偷粮行动"))?;
        let NightOrder::StealHome(target) = attempt.order else {
            return Err(KernelError::action("今夜选择了其他行动，不能再偷别人家"));
        };
        if self.seats[target].out.is_some()
            || spot.prop != format!("{PANTRY_PREFIX}{}", target + 1)
            || world.topo.prop(&spot.prop).map(|prop| prop.locale.as_str())
                != self.home_of(world, target).as_deref()
        {
            return Err(KernelError::action("必须使用本夜指定目标住宅里的私粮袋"));
        }
        Ok((seat, target))
    }
    /// 守家是一个持续状态：在本人家、醒着，且不在吃饭、聊天、移动、取粮等不相容活动里。
    /// 偷窃发生的那一刻看这个函数，而不是天亮时再看人有没有赶回来。
    fn keeping_guard(&self, world: &World, i: usize) -> bool {
        let Some(a) = world.actor(&self.seats[i].id) else {
            return false;
        };
        if self.phase != Phase::Night
            || !self
                .night_orders
                .get(&i)
                .is_some_and(|n| n.order == NightOrder::Guard)
            || !world.awake(&a.id)
            || a.current.is_travel()
            || matches!(a.current.kind, crate::world::ActKind::Timed { .. })
            || a.current.is_instant()
            || !world.awake(&a.id)
            || a.current
                .participants
                .iter()
                .any(|member| member != &self.seats[i].id)
        {
            return false;
        }
        self.at_home(world, i)
    }
    fn at_granary(&self, world: &World, i: usize) -> bool {
        let granary = self.granary_point(world);
        world
            .actor(&self.seats[i].id)
            .is_some_and(|a| a.locus.locale == granary)
    }
    fn home_of(&self, world: &World, i: usize) -> Option<String> {
        world.actor(&self.seats[i].id)?.home.clone()
    }
    fn at_home(&self, world: &World, i: usize) -> bool {
        let home = self.home_of(world, i);
        world.actor(&self.seats[i].id).is_some_and(|a| {
            home.as_deref() == Some(a.locus.locale.as_str())
                && !a.current.is_travel()
                && !matches!(a.locus.at, protocol::At::Transit { .. })
        })
    }
    fn settle_home_entry(&mut self, world: &mut World, i: usize) {
        if self.phase != Phase::Night {
            return;
        }
        // 住宅取粮：完成那一刻看目标是否真在守家，再扣目标私粮；目击只写实际在目标住宅、醒着的人。
        let Some(n) = self.night_orders.get(&i) else {
            return;
        };
        let NightOrder::StealHome(t) = n.order else {
            return;
        };
        if n.settled || t >= self.seats.len() {
            return;
        }
        let Some(home) = self.home_of(world, t) else {
            return;
        };
        let entered = n
            .entry_ready_at
            .is_some_and(|at| at <= world.minutes() + EPS)
            && world.actor(&self.seats[i].id).is_some_and(|a| {
                a.locus.locale == home
                    && a.current.interaction.is_none()
                    && !a.current.is_travel()
                    && !a.needs.incapacitated()
            });
        if !entered {
            return;
        }
        let guarded = self.keeping_guard(world, t);
        let witnesses: Vec<usize> = (0..self.seats.len())
            .filter(|&j| j != i)
            .filter(|&j| {
                world.actor(&self.seats[j].id).is_some_and(|a| {
                    a.locus.locale == home
                        && !a.current.is_travel()
                        && !a.needs.incapacitated()
                        && world.awake(&a.id)
                })
            })
            .collect();
        if guarded {
            self.remember(
                world,
                i,
                format!("你进{}家打开私粮袋时，被对方守家阻止。", self.label(t)),
            );
            self.remember(
                world,
                t,
                format!(
                    "{}进你家打开私粮袋时，你正守在家里并阻止了对方。",
                    self.label(i)
                ),
            );
        } else if self.seats[t].rice > 0 {
            self.seats[t].rice -= 1;
            self.seats[i].rice += 1;
            self.sync_pantry(world, t);
            self.sync_pantry(world, i);
            self.remember(
                world,
                i,
                format!("你从{}家的私粮袋取得1袋米。", self.label(t)),
            );
            self.remember(
                world,
                t,
                "你家的私粮少了1袋；你当时没有看见是谁进来。".into(),
            );
        } else {
            self.remember(
                world,
                i,
                format!("你进了{}家，打开私粮袋，里面没有米。", self.label(t)),
            );
        }
        for j in witnesses {
            self.remember(
                world,
                j,
                format!(
                    "{}进{}家翻私粮袋时，你恰好也在那里，看见了这一幕；这不是公开消息。",
                    self.label(i),
                    self.label(t)
                ),
            );
        }
        if let Some(n) = self.night_orders.get_mut(&i) {
            n.stage = NightStage::Entering(t);
            n.settled = true;
            n.entry_ready_at = None;
        }
    }
    /// 同一公仓或同一家私粮袋的偷粮动作，只要真实取粮区间重叠，就全部撞车。
    /// 不按点击顺序或谁先拿到操作位决定；前一个已结束、后一个才开始不算，不同目标也不算。
    fn theft_target(&self, i: usize) -> Option<String> {
        match self.night_orders.get(&i)?.order {
            NightOrder::StealPublic => Some(format!("public:{}", self.granary)),
            NightOrder::StealHome(target) => Some(format!("home:{PANTRY_PREFIX}{}", target + 1)),
            _ => None,
        }
    }
    /// 已开始、尚未结束的偷粮动作也占用真实取粮区间。已结算失败的记录同样保留到本夜结束，
    /// 用来阻止重试；结算成功的不再与后来者重叠。
    fn theft_open_window(&self, world: &World, other: usize, now: f64) -> Option<(f64, f64)> {
        let attempt = self.night_orders.get(&other)?;
        if !matches!(
            attempt.order,
            NightOrder::StealPublic | NightOrder::StealHome(_)
        ) {
            return None;
        }
        let actor = world.actor(&self.seats[other].id)?;
        let prop = actor
            .current
            .action
            .as_ref()
            .and_then(|binding| binding.draft.prop_id.as_deref())?;
        let spot = world.affordances.for_prop(prop, &actor.current.verb)?;
        if !Self::is_granary_collect(world, spot) && !Self::is_pantry_collect(world, spot) {
            return None;
        }
        if Some(self.theft_target(other)?) != self.theft_target_by_spot(world, spot) {
            return None;
        }
        let remaining = actor.current.remaining(now)?;
        if remaining <= EPS {
            return None;
        }
        // 标称取粮动作从 started_at 开始，到 remaining 用完为止。世界按切片推进，调用时可能
        // 刚好跨过完成点；这仍是实际占用取粮区域的完整区间，而不是只看当前还剩多少。
        Some((actor.current.started_at, now + remaining))
    }
    fn theft_target_by_spot(
        &self,
        world: &World,
        spot: &crate::affordance::Spot,
    ) -> Option<String> {
        if Self::is_granary_collect(world, spot) {
            Some(format!("public:{}", self.granary))
        } else if Self::is_pantry_collect(world, spot) {
            Some(format!("home:{}", spot.prop))
        } else {
            None
        }
    }
    fn theft_windows_overlap(a: (f64, f64), b: (f64, f64)) -> bool {
        a.0 + EPS < b.1 && b.0 + EPS < a.1
    }
    /// 当前动作自身的时间窗，加上同一目标所有正在进行或已撞车的偷粮动作。
    /// 同一动作开始、完成时都会重算，因此结果不取决于谁先回执。
    fn overlapping_theft(&self, world: &World, i: usize, from: f64, to: f64) -> Vec<usize> {
        let Some(target) = self.theft_target(i) else {
            return Vec::new();
        };
        let current = (from, to);
        let mut others = Vec::new();
        for &other in self.night_orders.keys() {
            if other == i || self.theft_target(other).as_ref() != Some(&target) {
                continue;
            }
            let windows = std::iter::once(self.theft_open_window(world, other, world.minutes()))
                .chain(self.theft_windows.iter().filter_map(|window| {
                    (window.day == self.day && window.seat == other)
                        .then_some(Some((window.from, window.to)))
                }))
                .flatten();
            if windows
                .into_iter()
                .any(|window| Self::theft_windows_overlap(current, window))
                && !others.contains(&other)
            {
                others.push(other);
            }
        }
        others
    }
    /// 记下撞车和各自亲历，但不在只读的活动启动路径里中断别人；世界取回玩法后会按
    /// 这份记录结束仍在进行的动作。
    fn note_theft_collision(&mut self, i: usize, others: &[usize]) {
        if others.is_empty() {
            return;
        }
        for &first in std::iter::once(&i).chain(others) {
            let Some(attempt) = self.night_orders.get_mut(&first) else {
                continue;
            };
            for &other in std::iter::once(&i).chain(others) {
                if first != other && !attempt.collided.contains(&other) {
                    attempt.collided.push(other);
                }
            }
            attempt.collided.sort_unstable();
            attempt.settled = true;
            attempt.collision_reported = true;
            attempt.entry_ready_at = None;
        }
    }
    /// 夜间偷到手的米要在动作结束后统一结算：同时重叠的撞车者全部失败，不能让先完成的人先转账。
    fn finish_theft(
        &mut self,
        world: &mut World,
        seat: usize,
        spot: &crate::affordance::Spot,
    ) -> Result<Option<Vec<String>>> {
        let Some(attempt) = self.night_orders.get(&seat).cloned() else {
            return Err(KernelError::action("今夜没有可执行的偷粮行动"));
        };
        if attempt.settled {
            return Err(KernelError::action("本夜偷粮行动已经结束，不能重复取粮"));
        }
        let actor = self.seats[seat].id.clone();
        let target = self
            .theft_target(seat)
            .ok_or_else(|| KernelError::action("今夜没有可执行的偷粮行动"))?;
        let now = world.minutes();
        let (from, to) = world
            .actor(&actor)
            .map(|a| {
                let started = a.current.started_at;
                let end = a
                    .current
                    .remaining(started)
                    .map_or(started + COLLECT_MINUTES, |left| started + left);
                (started, end.max(now))
            })
            .unwrap_or((now - COLLECT_MINUTES, now));
        let others = self.overlapping_theft(world, seat, from, to);
        self.theft_windows.push(TheftWindow {
            seat,
            day: self.day,
            target: target.clone(),
            others: others.clone(),
            from,
            to,
        });
        if !others.is_empty() {
            // 本人正在结算，不中断自己的活动；返回错误后由通用完成路径只写一次失败回执。
            // 其他参与者仍在外面的动作由 `interrupt_after_complete` 马上打断。
            self.note_theft_collision(seat, &others);
            return Err(KernelError::action(
                "同一取粮目标有重叠的偷粮行动，撞车者全部失败，本夜不能重试",
            ));
        }
        let public = Self::is_granary_collect(world, spot);
        if public {
            if target != format!("public:{}", self.granary) {
                return Err(KernelError::action("必须使用本夜指定目标的粮仓"));
            }
            let before = self.seats[seat].rice;
            let answer = self.collect_rice(world, seat, &actor, spot)?;
            if self.seats[seat].rice > before {
                self.last_public_stolen += 1;
            }
            return Ok(answer);
        }
        let (checked, checked_target) = self.check_home_collection(world, &actor, spot)?;
        if checked != seat {
            return Err(KernelError::action("偷粮登记与本人不符"));
        }
        if target != format!("home:{PANTRY_PREFIX}{}", checked_target + 1)
            || target
                != format!(
                    "home:{PANTRY_PREFIX}{}",
                    match attempt.order {
                        NightOrder::StealHome(t) => t + 1,
                        _ => 0,
                    }
                )
        {
            return Err(KernelError::action("必须使用本夜指定目标住宅里的私粮袋"));
        }
        if !world.sees_prop(&actor, &spot.prop) || !world.awake(&actor) {
            return Err(KernelError::action("未在目标私粮袋旁清醒地完成偷粮"));
        }
        {
            let attempt = self.night_orders.get_mut(&seat).expect("已登记入室");
            attempt.stage = NightStage::Entering(checked_target);
            attempt.entry_ready_at = Some(now);
        }
        self.settle_home_entry(world, seat);
        Ok(Some(vec![format!(
            "入室偷粮已结算，个人私粮现有{}袋",
            self.seats[seat].rice
        )]))
    }

    /// 动作启动时先统一识别已经进行中的重叠偷粮：已有撞车就拒绝新的开始；
    /// 新动作确实与进行中的动作重叠，则连同旧动作一起失败，绝不让先完成者先拿米。
    /// 失败段写入窗口后仍保留到天亮，免得重开或重试绕过本夜限制。
    fn open_theft_window(
        &mut self,
        world: &World,
        seat: usize,
        _spot: &crate::affordance::Spot,
        from: f64,
    ) -> Result<()> {
        if self
            .night_orders
            .get(&seat)
            .is_some_and(|attempt| attempt.collision_reported)
        {
            return Err(KernelError::action("本夜偷粮撞车已经失败，不能重试"));
        }
        let to = from + COLLECT_MINUTES;
        let others = self.overlapping_theft(world, seat, from, to);
        if others.is_empty() {
            return Ok(());
        }
        // 新动作还没开始，调用方会因错误自然失败；已经在进行中的动作由世界入口在拿到
        // 错误后统一中断，避免玩法借用世界的可变状态。
        self.note_theft_collision(seat, &others);
        Err(KernelError::action(
            "同一取粮目标有重叠的偷粮行动，撞车者全部失败，本夜不能重试",
        ))
    }
    /// 取粮完成后唯一修改粮食账本的地方。调用前已验证登记、目标和真实完成动作。
    fn credit_collection(
        &mut self,
        world: &mut World,
        seat: usize,
        witnesses: Vec<usize>,
    ) -> Option<(u32, u32)> {
        if self.public_rice == 0 {
            return None;
        }
        let before = (self.public_rice, self.seats[seat].rice);
        self.public_rice -= 1;
        self.seats[seat].rice += 1;
        if self.phase == Phase::Day {
            let attempt = self.day_orders.get_mut(&seat)?;
            attempt.collected = true;
            attempt.claimed = true;
            self.announce(
                world,
                format!(
                    "{}在公仓领走1袋米，公仓剩{}袋。",
                    self.label(seat),
                    self.public_rice
                ),
                false,
            );
        } else {
            let attempt = self.night_orders.get_mut(&seat)?;
            attempt.stage = NightStage::Taking;
            attempt.settled = true;
            attempt.entry_ready_at = None;
            self.remember(
                world,
                seat,
                "你完成了公仓取粮，1袋米已放进自己的私粮。".into(),
            );
            for witness in witnesses {
                self.remember(
                    world,
                    witness,
                    format!(
                        "{}在公仓取米时，你恰好也在公仓，看见了这件事；这不是公开消息。",
                        self.label(seat)
                    ),
                );
            }
        }
        self.sync_pantry(world, seat);
        self.sync_granary(world);
        Some(before)
    }
    /// 白天领米和夜间偷公仓共用的真实完成校验与到账；夜间撞车已在完成前统一判定。
    fn collect_rice(
        &mut self,
        world: &mut World,
        seat: usize,
        actor: &str,
        spot: &crate::affordance::Spot,
    ) -> Result<Option<Vec<String>>> {
        let granary = self.granary_point(world);
        if !world.actor(actor).is_some_and(|actor| {
            actor.locus.locale == granary
                && !actor.current.is_travel()
                && actor.current.interaction.is_none()
        }) || !world.sees_prop(actor, &spot.prop)
        {
            return Err(KernelError::action(
                "没有在公共粮仓完成取粮，本次没有领取成功",
            ));
        }
        self.check_collection(world, actor, spot)?;
        let witnesses = if self.phase == Phase::Night {
            self.living()
                .into_iter()
                .filter(|&other| other != seat)
                .filter(|&other| {
                    world.actor(&self.seats[other].id).is_some_and(|actor| {
                        actor.locus.locale == granary
                            && !actor.current.is_travel()
                            && !actor.needs.incapacitated()
                            && world.awake(&actor.id)
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        self.credit_collection(world, seat, witnesses)
            .ok_or_else(|| KernelError::action("公共粮仓已经空了，没有领取成功"))?;
        Ok(Some(vec![format!(
            "已取回1袋米，个人私粮现有{}袋，公仓剩余{}袋。",
            self.seats[seat].rice, self.public_rice
        )]))
    }
    /// 按真实状态推进夜间阶段：上床、醒来、守家失效都即时反映，结算只看动作完成时刻留下的结果。
    fn track_night(&mut self, world: &mut World) {
        if self.phase != Phase::Night {
            return;
        }
        let living = self.living();
        for &i in &living {
            let Some(n) = self.night_orders.get(&i).cloned() else {
                continue;
            };
            match n.order {
                NightOrder::Sleep => {
                    if self.in_own_bed(world, i) {
                        let done = world.actor(&self.seats[i].id).is_some_and(|a| {
                            a.current
                                .remaining(world.minutes())
                                .is_none_or(|m| m <= EPS)
                        });
                        if let Some(n) = self.night_orders.get_mut(&i) {
                            n.stage = NightStage::Sleeping;
                            n.on_bed = true;
                            n.slept |= done;
                        }
                    } else if n.on_bed || n.stage == NightStage::Sleeping {
                        let interrupted = !n.slept;
                        if let Some(n) = self.night_orders.get_mut(&i) {
                            // 睡过之后离开床也不再是“睡到清晨”；失败原因不能依赖下一拍
                            // 还能不能看见人是否在床上。
                            n.slept = false;
                            n.on_bed = false;
                            n.stage = NightStage::Chosen;
                            n.sleep_left = true;
                        }
                        if interrupted {
                            self.remember(
                                world,
                                i,
                                "你的睡眠中断了；本夜不再补发另一种行动。".into(),
                            );
                        }
                    }
                }
                NightOrder::Guard => {
                    let stage = if self.keeping_guard(world, i) {
                        NightStage::Guarding
                    } else {
                        NightStage::Chosen
                    };
                    if let Some(n) = self.night_orders.get_mut(&i) {
                        n.stage = stage;
                    }
                }
                NightOrder::StealHome(t) => {
                    if n.settled {
                        continue;
                    }
                    let target_home = self.home_of(world, t);
                    let can_enter = world.topo.props().iter().any(|p| {
                        Some(p.locale.as_str()) == target_home.as_deref()
                            && p.verbs.iter().any(|v| {
                                v.action
                                    .as_ref()
                                    .is_some_and(|a| a.mode_managed && a.id == PANTRY_ACTION)
                            })
                    });
                    let here = can_enter
                        && world.actor(&self.seats[i].id).is_some_and(|a| {
                            target_home.as_deref() == Some(a.locus.locale.as_str())
                                && !a.current.is_travel()
                                && !matches!(a.locus.at, protocol::At::Transit { .. })
                        });
                    if !here {
                        if let Some(n) = self.night_orders.get_mut(&i) {
                            if n.entry_ready_at.is_some() {
                                n.stage = NightStage::Searching(t);
                                n.entry_ready_at = None;
                            }
                        }
                        continue;
                    }
                }
                NightOrder::StealPublic => {
                    let at = self.at_granary(world, i);
                    if let Some(n) = self.night_orders.get_mut(&i) {
                        if n.stage == NightStage::Chosen && at {
                            n.stage = NightStage::Taking;
                        }
                    }
                }
            }
        }
    }
    /// 到现场后要主动做的动作；不能只把人带到地点。`Goto` 的终点本身就是可停留的处所，
    /// 所以用不带动作组的 `Untracked`，到了由模型继续观察并执行普通动作。
    fn guide(&self, world: &mut World, i: usize, goal: crate::intent::Intent) {
        let id = self.seats[i].id.clone();
        if world.actor(&id).is_none() {
            return;
        }
        let now = world.minutes();
        let actor = world.actor_mut(&id);
        if actor.current.is_travel()
            || actor.current.action.is_some()
            || actor.current.interaction.is_some()
            || actor.player_controlled()
            || actor.free_walk.is_some()
        {
            return;
        }
        actor.plan.clear();
        actor
            .plan
            .push_back(crate::action_state::QueuedAction::Untracked(goal));
        actor.actions.revision = actor.actions.revision.wrapping_add(1);
        if actor.current.is_idle() {
            actor.current =
                crate::activity::Activity::instant_stay(&id, now, actor.locus.locale.clone());
        }
    }
    fn call(&self, world: &mut World, i: usize, msg: &str) {
        let id = self.seats[i].id.clone();
        if world.actor(&id).is_none() {
            return;
        }
        // 农田劳动只能由本人主动结束；其他正式行动开始会中止正在进行的交谈。
        if world.active_chat(&id).is_some() {
            world.interrupt_actions(&id, "十日生存实验的正式行动开始");
        }
        world.notify(&id, msg.to_string());
    }
    fn announce(&mut self, world: &mut World, msg: String, wake: bool) {
        self.say(world, msg, wake, false);
    }
    /// 观众会读到的关键事实：公告正文不改，自动整理按固定前缀识别投票、淘汰、天灾和终局。
    fn key(&mut self, world: &mut World, msg: String, wake: bool) {
        self.say(world, msg, wake, true);
    }
    fn say(&mut self, world: &mut World, msg: String, wake: bool, key: bool) {
        self.notice = msg.clone();
        self.history.push(msg.clone());
        if self.history.len() > 160 {
            self.history.remove(0);
        }
        world.log(
            EventInfo::default(),
            format!("【十日生存】{}{msg}", if key { "【关键剧情】" } else { "" }),
        );
        if wake {
            for i in self.living() {
                self.call(world, i, &msg);
            }
        }
    }
    fn rotated(&self, mut people: Vec<usize>, tag: u32) -> Vec<usize> {
        // 固定种子 + 天数，可复现的结算优先级，避免模型响应速度决定资源归属。
        people.sort_by_key(|&i| {
            let mut x = self.seed ^ tag.wrapping_mul(0x9e3779b9) ^ (i as u32 + 1);
            x ^= x >> 16;
            x = x.wrapping_mul(0x7feb352d);
            x ^= x >> 15;
            x = x.wrapping_mul(0x846ca68b);
            x ^ (x >> 16)
        });
        people
    }
    fn start_day(&mut self, world: &mut World) {
        self.day += 1;
        self.theft_windows.clear();
        for &i in &self.living() {
            world.interrupt_actions(&self.seats[i].id, "新的一天开始，结束昨夜或昨日的活动");
        }
        self.day_orders.clear();
        self.night_orders.clear();
        self.night_reminded.clear();
        self.prepare_day(world);
        self.sync_all_supplies(world);
        self.phase = Phase::Day;
        let now = world.minutes();
        self.until = self.at(self.day, DINNER).max(now + DAY_NOTICE);
        self.whispers_used.clear();
        self.ballots.clear();
        self.revealed = false;
        self.produced_today = 0;
        self.lines.clear();
        let mut pending = std::mem::take(&mut self.pending_whispers);
        for letter in pending.drain(..) {
            let archive_id = letter
                .archive_id
                .unwrap_or_else(|| self.archive_whisper(&letter, None));
            if let Some(record) = self
                .whisper_archive
                .iter_mut()
                .find(|record| record.id == archive_id)
            {
                record.status = if self.seats[letter.to].out.is_none() {
                    WhisperStatus::Delivered
                } else {
                    WhisperStatus::RecipientEliminated
                };
                record.resolved_at = Some(now);
            }
            let from = self.label(letter.from);
            // 目标出局时信件不再投递。
            if self.seats[letter.to].out.is_none() {
                self.remember(
                    world,
                    letter.to,
                    format!(
                        "第{}天清晨收到{}在第{}天的悄悄话：「{}」",
                        self.day, from, letter.sent_day, letter.content
                    ),
                );
                self.call(
                    world,
                    letter.to,
                    "你收到一封昨天发出的悄悄话，请阅读自己的经历。",
                );
                self.outcome(
                    world,
                    letter.to,
                    format!(
                        "{}在第{}天收到{}在第{}天发出的悄悄话：「{}」（只有收信人读到）",
                        self.label(letter.to),
                        self.day,
                        from,
                        letter.sent_day,
                        letter.content
                    ),
                    format!("whisper:{archive_id}-delivered"),
                    false,
                );
            } else {
                self.outcome(
                    world,
                    letter.from,
                    format!(
                        "{}在第{}天发给{}的悄悄话没有投递：收信人已出局（正文仍封存于信件档案）。",
                        self.label(letter.from),
                        letter.sent_day,
                        self.label(letter.to)
                    ),
                    format!("whisper:{archive_id}-recipient-out"),
                    false,
                );
            }
        }
        let hungry = self
            .last_hungry
            .iter()
            .map(|&i| self.label(i))
            .collect::<Vec<_>>();
        let news = if self.day == 1 {
            String::new()
        } else {
            format!(
                "昨夜公共粮仓被秘密取走{}袋；昨晚没吃上饭：{}。",
                self.last_public_stolen,
                if hungry.is_empty() {
                    "无人".into()
                } else {
                    hungry.join("、")
                }
            )
        };
        self.announce(world, format!(
            "第{}/10天开始。公仓{}袋。{}白天每人工作或领米二选一，公共农田四个田块，每天最多四人报名，先到先得；村民仍按八小时劳动规则行动，后台产粮继续按当日有效报名每人一袋结算入公仓，并可额外发送一条次日送达的悄悄话。",
            self.day,self.public_rice,news
        ), true);
        let status = self.disaster_status();
        if !status.is_empty() {
            for seat in self.living() {
                world.notify(&self.seats[seat].id, format!("赛事灾情提醒：{status}"));
            }
        }
    }
    /// 公开可见的路线提示：只提醒还在等决定的人去地图上能找到的粮仓/农田；
    /// 已经提交的人不被改选，也不会被代替移动。提示本身不产生粮食。
    fn prepare_day(&self, world: &mut World) {
        if self.crop_failure(self.day) {
            for seat in self.living() {
                world.notify(&self.seats[seat].id, "赛事公告提醒：今日天灾绝收，报名和劳动都不产粮；请自主考虑领米和已有储备，晚餐照常扣粮。仍可自愿报名，但报名后不能改领米。".into());
            }
        }
        let Some(field) = Self::farms(world)
            .first()
            .and_then(|id| world.topo.prop(id))
            .map(|p| (p.locale.clone(), world.topo.locale_name(&p.locale)))
        else {
            return;
        };
        let (field_id, field_name) = field;
        let granary_point = self.granary_point(world);
        let granary_name = world.topo.locale_name(&granary_point);
        for &i in &self.living() {
            let id = self.seats[i].id.clone();
            world.notify(
                &id,
                format!(
                    "第{}天：工作或领米二选一。农田在 {field_id}（{field_name}），粮仓在 {granary_point}（{granary_name}）。想工作：先报名取得四个名额之一，再到分配的田块下地干活，仍按实际有效劳动累计八小时安排劳动，后台产粮按报名结算，不要求准时到田；劳动中不能交谈，邀请或接受交谈会被拒绝，须先主动结束劳动；结束劳动后仍可在白天回田继续累计，名额不转让、进度不过夜。想领米：先登记 take_rice，再前往粮仓执行十分钟取粮。提交选择本身不给本人私粮。",
                    self.day
                )
            );
        }
    }
    fn resolve_day_and_dinner(&mut self, world: &mut World) {
        let living = self.living();
        // 白天领取动作已在完成时到账；晚餐这里只做收获和结果说明。
        let qualified: Vec<_> = living
            .iter()
            .copied()
            .filter(|&i| {
                self.day_orders
                    .get(&i)
                    .is_some_and(|d| d.order == DayOrder::Work && d.locked && !d.harvested)
            })
            .collect();
        for &i in &qualified {
            self.day_orders.get_mut(&i).expect("已登记工作").harvested = true;
            if self.crop_failure(self.day) {
                self.remember(
                    world,
                    i,
                    "今日天灾绝收，劳动报名结算为0袋；不是劳动进度不足，不补发收成。".into(),
                );
                continue;
            }
            if (self.produced_today as usize) < MAX_WORKERS {
                self.public_rice += 1;
                self.produced_today += 1;
                self.sync_granary(world);
                self.remember(
                    world,
                    i,
                    "你今天报名劳动，后台按报名结算1袋米进入公仓，不增加你的私粮。".into(),
                );
                self.announce(
                    world,
                    format!("{}的劳动报名结算1袋米，公仓新增1袋。", self.label(i)),
                    false,
                );
            } else {
                self.announce(
                    world,
                    format!(
                        "{}劳动满八小时，但受每日四袋产量上限限制，本日没有新增收成。",
                        self.label(i)
                    ),
                    false,
                );
                self.remember(
                    world,
                    i,
                    "今日产粮已达四袋上限，本次报名未新增收成。".into(),
                );
            }
        }
        // 领取已在动作完成时逐笔记账；晚餐不再按提交状态或天亮时站的位置重复发米。
        for i in self.living() {
            match self.day_orders.get(&i) {
                None => self.remember(
                    world,
                    i,
                    "你今天没有提交白天行动，所以没有工作也没有领米。".into(),
                ),
                Some(d) if d.order == DayOrder::Take && !d.claimed => {
                    self.remember(
                        world,
                        i,
                        "你今天选择领米，但没有完成粮仓取粮动作，没有领到米。".into(),
                    );
                }
                _ => {}
            }
        }
        let mut hungry = Vec::new();
        let mut dinner = Vec::new();
        for i in living {
            if self.seats[i].rice > 0 {
                self.seats[i].rice -= 1;
                self.sync_pantry(world, i);
                self.seats[i].hungry_streak = 0;
                self.consumed += 1;
                dinner.push(format!(
                    "{}消耗晚餐1袋，私粮剩余{}袋",
                    self.label(i),
                    self.seats[i].rice
                ));
                self.remember(
                    world,
                    i,
                    format!("你晚餐消耗1袋米，剩余{}袋。", self.seats[i].rice),
                );
            } else {
                self.seats[i].hungry_streak += 1;
                dinner.push(format!(
                    "{}没有米可吃，已连续{}晚未进食",
                    self.label(i),
                    self.seats[i].hungry_streak
                ));
                hungry.push(i);
                self.remember(world, i, "你晚餐没有米可吃。".into());
            }
        }
        // 晚饭是全镇同时发生的结算；旁观者看得到谁吃上了，私粮余额仍只在全知剧本里。
        world.manuscript_fact(
            ManuscriptKind::Action,
            ManuscriptVisibility::Private,
            None,
            None,
            format!(
                "裁判全知：第{}天晚餐——{}；余额只给旁观者看，不向其他人公开。",
                self.day,
                dinner.join("；")
            ),
            self.living()
                .into_iter()
                .map(|i| self.seats[i].id.clone())
                .collect(),
            format!("dinner:{}-{}-{}", self.seed, self.first_day, self.day),
        );
        self.last_hungry = hungry.clone();
        // 晚饭阶段已经确定的结果先记下；出局依次结算，轮到离场陈词时再按队列处理。
        // 进入 LastWords 之后 `maybe_advance` 不再按原截止重复结算晚饭。
        let starving: Vec<_> = hungry
            .into_iter()
            .filter(|&i| self.seats[i].hungry_streak >= 2)
            .collect();
        for i in starving {
            self.eliminate(world, i, "连续两晚未进食");
        }
        if self.has_departures() {
            // 这一带淘汰完还要走陈词；留给真实离场动作，不能被原晚餐截止在同一分钟重复结算。
            self.until = world.minutes() + DAY_NOTICE;
            self.begin_departures(world);
        } else if self.living().len() <= 1 {
            self.finish_single(world);
        } else if self.living().len() == 2 {
            self.start_jury(world);
        } else if self.day % 2 == 0 {
            // 第十天也必须先完成第五次篝火会投票；投票结束后才进入陪审团。
            self.start_gather(world);
        } else {
            self.start_night(world);
        }
    }
    fn eliminate(&mut self, world: &mut World, i: usize, reason: &str) {
        if self.seats[i].out.is_some() {
            return;
        }
        self.seats[i].out = Some(reason.to_string());
        self.seats[i].out_day = Some(self.day);
        let returned = std::mem::take(&mut self.seats[i].rice);
        self.sync_pantry(world, i);
        self.public_rice += returned;
        self.sync_granary(world);
        // 公粮账本的增量是全镇可见的；出局者的私粮具体余额只留给旁观剧本，不进「仅公开」。
        self.announce_private_fact(
            world,
            format!(
                "裁判全知：{}出局前持有私粮{returned}袋，已全部归入公仓（现有{}袋）。",
                self.label(i),
                self.public_rice
            ),
            format!("eliminate:{}-{}-{i}", self.seed, self.day),
        );
        world.interrupt_actions(&self.seats[i].id, "已淘汰，等待离场陈词");
        self.departures.push(i);
        // 投票、夜晚、遴选、陪审团和离场陈词都可能在结算到点后把 new 截止推回当前时刻。
        // 还没交上来的物资和选票状态先收口；下一步由 departures 统一带路，不能等
        // 本拍剩余时间继续跑原阶段，否则会跳过后续出局者的陈词或在计划外开夜。
        self.until = world.minutes();
        self.key(
            world,
            format!(
                "{}出局（{}），其私粮{}袋归入公仓。",
                self.label(i),
                reason,
                returned
            ),
            true,
        );
        self.remember(
            world,
            i,
            format!(
                "你在第{}天出局，原因：{}。离场陈词后成为陪审员，剩两位候选人时参与终局投票。",
                self.day, reason
            ),
        );
    }
    /// 淘汰只进队列；等本轮的所有淘汰和胜负判定都收口后，才从这里进入陈词。
    /// 这里一次只开首位，其余出局者在 `next_farewell` 中按队列依次接上，不能被另一段
    /// 结算路径抢先生效。进入陈词前不再记录“原本还要去哪”，最后能走哪一步只由
    /// 陈词结束时的存活人数决定。
    fn begin_departures(&mut self, world: &mut World) -> bool {
        if self.departures.is_empty() {
            return false;
        }
        self.phase = Phase::LastWords;
        self.turn = None;
        self.next_farewell(world);
        true
    }
    /// 本轮结算是否已经产生待陈词的出局者。
    fn has_departures(&self) -> bool {
        !self.departures.is_empty()
    }
    /// 幂等：同一拍重入时仍等当前这位；已经结束并移出队列的人不会被再次点名。
    fn next_farewell(&mut self, world: &mut World) {
        if self.departures.is_empty() {
            self.turn = None;
            self.resume_after_farewell(world);
            return;
        }
        if let Some(waiting) = self.turn.filter(|seat| self.departures.contains(seat)) {
            if waiting != self.departures[0] {
                return;
            }
        }
        let seat = self.departures[0];
        self.turn = Some(seat);
        self.until = world.minutes() + 5.0;
        self.announce(world, format!("{}离场前可留下400字以内的陈词、预言或署名纸条，可指认一人；这只是本人的声明，不代表裁判认定。", self.label(seat)), true);
        self.call(world, seat, "请调用 mode_action action=farewell，text 填离场陈词或叙事遗物，target 可选指认座号。空 text 表示放弃。不能转赠粮食或创造道具。五分钟后未提交将自动离场。");
    }
    /// 陈词队列清空后只走这一处：按最终存活人数兑现终局、陪审或夜晚，
    /// 不让其他入口在陈词尚未走完时抢先判断胜负。
    fn resume_after_farewell(&mut self, world: &mut World) {
        self.resume_phase = None;
        match self.live_count() {
            0 | 1 => self.finish_single(world),
            2 => {
                self.start_jury(world);
            }
            _ => self.start_night(world),
        }
    }
    /// 只结束当前这位；等 `next_farewell` 选出下一人后，再按此时剩下的人数定去路。
    fn finish_farewell(&mut self, world: &mut World, text: Option<String>, target: Option<u32>) {
        let Some(seat) = self.turn.take() else {
            return;
        };
        if !self.departures.contains(&seat) {
            self.next_farewell(world);
            return;
        }
        let skipped = text.as_ref().is_none_or(|text| text.is_empty());
        let text = text.unwrap_or_default();
        self.farewells.push(Survival10Farewell {
            seat: seat as u32 + 1,
            at: world.minutes(),
            text: text.clone(),
            target,
            skipped,
        });
        if !skipped {
            let actor = self.seats[seat].id.clone();
            world.log(
                EventInfo::speech(
                    &actor,
                    &self.seats[seat].name,
                    EventSpeech {
                        occasion: "离场陈词（当事人声明）".into(),
                        seat: Some(seat as u32 + 1),
                        text: text.clone(),
                        public: true,
                    },
                ),
                format!(
                    "{}的离场陈词：{}（当事人声明，非裁判认定）",
                    self.label(seat),
                    text
                ),
            );
            let locale = world
                .actor(&actor)
                .map(|resident| resident.locus.locale.clone());
            for listener in self.living() {
                if world
                    .actor(&self.seats[listener].id)
                    .is_some_and(|resident| {
                        Some(&resident.locus.locale) == locale.as_ref()
                            && !resident.current.is_travel()
                            && world.awake(&resident.id)
                    })
                {
                    let now = world.minutes();
                    world.actor_mut(&self.seats[listener].id).journal.fact(
                        now,
                        FactKind::Speech,
                        format!(
                            "{}的离场陈词：{}（当事人声明，非裁判认定）",
                            self.label(seat),
                            text
                        ),
                        vec![actor.clone()],
                        None,
                        None,
                    );
                }
            }
        }
        self.announce(
            world,
            if skipped {
                format!("{}没有留下离场陈词。", self.label(seat))
            } else {
                format!(
                    "{}的离场声明{}：{}（仅为当事人说法，未经裁判证实；纸条不产生资源。）",
                    self.label(seat),
                    target.map_or(String::new(), |seat| format!("，指认{seat}号")),
                    text
                )
            },
            true,
        );
        self.departures.retain(|&pending| pending != seat);
        if self.departures.is_empty() {
            // 所有人先留在原处等自己的陈词；整队走完才集中离场，不能提前让后面的人
            // 消失在场景里，也不能让已经出局的人再走普通步行动作。
            for record in &self.farewells {
                let id = self.seats[record.seat as usize - 1].id.clone();
                world.depart(&id, "离场陈词结束，转为陪审员");
            }
        }
        self.next_farewell(world);
    }
    fn end_participation(&mut self, world: &mut World) {
        self.until = world.minutes();
        self.phase = Phase::Over;
        self.turn = None;
        self.resume_phase = None;
        for seat in self.departures.drain(..) {
            if !self
                .farewells
                .iter()
                .any(|record| record.seat as usize == seat + 1)
            {
                self.farewells.push(Survival10Farewell {
                    seat: seat as u32 + 1,
                    at: world.minutes(),
                    text: String::new(),
                    target: None,
                    skipped: true,
                });
            }
        }
        self.farewells.retain(|farewell| {
            self.seats
                .get(farewell.seat as usize - 1)
                .is_some_and(|seat| seat.out.is_some())
        });
        self.close_whispers(world.minutes());
        for seat in &self.seats {
            world.depart(&seat.id, "十日生存已结束，离开本局");
        }
    }
    fn meeting_point(&self, world: &World) -> Option<String> {
        world
            .topo
            .locales
            .values()
            .find(|locale| locale.name == "篝火广场")
            .or_else(|| world.topo.locales.get("plaza_1"))
            .or_else(|| {
                world.topo.locales.values().find(|locale| {
                    locale.kind == crate::topology::LocaleKind::Dwell
                        && locale.name.contains("广场")
                })
            })
            .map(|locale| locale.id.clone())
    }
    fn at_meeting(&self, world: &World, seat: usize) -> bool {
        self.meeting_point(world).is_some_and(|meeting| {
            world.actor(&self.seats[seat].id).is_some_and(|actor| {
                actor.locus.locale == meeting
                    && !actor.current.is_travel()
                    && world.awake(&actor.id)
            })
        })
    }
    fn start_gather(&mut self, world: &mut World) {
        self.phase = Phase::Gather;
        self.until = self.at(self.day, MEETING).max(world.minutes() + 30.0);
        if let Some(meeting) = self.meeting_point(world) {
            for seat in self.living() {
                let id = self.seats[seat].id.clone();
                if world
                    .actor(&id)
                    .is_none_or(|actor| actor.player_controlled() || actor.free_walk.is_some())
                {
                    continue;
                }
                world.interrupt_actions(&id, "晚餐已结算，前往篝火广场集合");
                let now = world.minutes();
                if let Some(actor) = world.actors.get_mut(&id) {
                    actor.plan.clear();
                    actor
                        .plan
                        .push_back(crate::action_state::QueuedAction::Untracked(
                            crate::intent::Intent::Goto {
                                locale: meeting.clone(),
                                station: None,
                            },
                        ));
                    actor.actions.revision = actor.actions.revision.wrapping_add(1);
                    if !actor.current.is_travel() {
                        actor.current = crate::activity::Activity::instant_stay(
                            &id,
                            now,
                            actor.locus.locale.clone(),
                        );
                    }
                }
            }
        }
        let destination = self.meeting_point(world).map_or_else(
            || "地图没有可用的篝火广场，未到场者不能远程发言或自主投票。".to_string(),
            |meeting| {
                format!(
                    "集合地点：{meeting}（{}）。",
                    world.topo.locale_name(&meeting)
                )
            },
        );
        self.announce(
            world,
            format!("晚餐已结算，请前往篝火广场集合，18:30晚会开始；若晚餐延后，集合仍留足30分钟。{destination}到场并保持清醒才能发言与自主投票；每人必须提交真实公开发言，想法不算发言，未完成不跳人；全部说完后统一记名投票，投票超时系统代投。"),
            true,
        );
    }
    fn start_speech(&mut self, world: &mut World) {
        self.phase = Phase::Speech;
        self.speech_round = 0;
        self.speakers = self.living();
        self.runoff_candidates = None;
        self.turn = None;
        self.lines.clear();
        self.next_speaker(world);
    }
    fn start_runoff_speech(&mut self, world: &mut World, candidates: Vec<usize>) {
        self.phase = Phase::Speech;
        self.speech_round = self.speech_round.saturating_add(1);
        self.speakers = candidates.clone();
        self.runoff_candidates = Some(candidates);
        self.turn = None;
        self.lines.clear();
        // 上一轮投票的截止已经过去；重投必须从当前时刻重新计时。沿用已经过去的截止会
        // 让世界停在原分钟反复开票和发言，永远走不出平票。
        self.until = world.minutes() + SPEECH_LIMIT;
        self.next_speaker(world);
    }
    fn next_speaker(&mut self, world: &mut World) {
        // 当次调用的 `advance` 已记过未发言，只有真正说完或被外部换人时才在这里补一句。
        if let Some(seat) = self.turn.take() {
            if !self.lines.iter().any(|line| line.seat == seat as u32 + 1) {
                self.remember(
                    world,
                    seat,
                    "这一轮篝火发言等待到截止时仍未提交公开发言，本轮没有你的原话。".into(),
                );
                world.log(
                    EventInfo::default(),
                    format!(
                        "{}在篝火发言截止前没有提交公开发言，本轮未留下原话。",
                        self.label(seat)
                    ),
                );
                world.interrupt_actions(&self.seats[seat].id, "篝火发言已到截止");
            }
        }
        if self.speakers.is_empty() {
            self.phase = Phase::Vote;
            self.ballots.clear();
            self.revealed = false;
            // 上一轮投票已经过了截止，新一轮必须从当前时刻重新计时。若沿用已经过去的
            // 截止，世界会停在原分钟反复开票和发言，永远走不出平票。
            self.until = world.minutes() + VOTE_LIMIT;
            let message = if let Some(candidates) = &self.runoff_candidates {
                let names = candidates
                    .iter()
                    .map(|&seat| self.label(seat))
                    .collect::<Vec<_>>()
                    .join("、");
                format!("平票候选人追加发言轮次结束，进入重投；全体在场参赛者须本人到场并保持清醒，25分钟内只能投给平票候选人（{names}），不能投自己，不能弃票，统一揭票，超时系统代投。")
            } else {
                format!("第{}天篝火会发言轮次结束，开始记名投票；须本人到场并保持清醒，不能弃票，25分钟内提交，统一揭票，超时系统代投。", self.day)
            };
            self.announce(world, message, true);
            return;
        }
        let i = self.speakers.remove(0);
        self.turn = Some(i);
        self.until = world.minutes() + SPEECH_LIMIT;
        // 当前发言人超时由 `advance` 统一轮转，且这里不能唤醒会重入世界的通告。
        if !self
            .history
            .last()
            .is_some_and(|notice| notice.contains("轮到"))
        {
            self.announce(world, format!("篝火会轮到{}发言。", self.label(i)), false);
        }
        let id = self.seats[i].id.clone();
        world.interrupt_actions(&id, "轮到篝火发言，请停止其他活动并公开说话");
        if let Some(meeting) = self.meeting_point(world) {
            self.guide(
                world,
                i,
                crate::intent::Intent::Goto {
                    locale: meeting,
                    station: None,
                },
            );
        }
        self.call(world, i, "请到篝火广场并保持清醒，必须用 mode_action action=speak、text=你说出口的原话提交本轮会议发言；thought、reason、沉默或只查看都不算发言。未说完不会跳人，全部发言后才投票。");
    }
    fn resolve_vote(&mut self, world: &mut World) {
        if self.living().len() <= 1 {
            self.finish_single(world);
            return;
        }
        if self.living().len() == 2 {
            self.start_jury(world);
            return;
        }
        // 超时缺票：按固定种子选择一位其他在场人，并标为系统代投。
        let living = self.living();
        for &i in &living {
            if !self.ballots.contains_key(&i) {
                let others = self.rotated(
                    living
                        .iter()
                        .copied()
                        .filter(|&j| {
                            j != i
                                && self
                                    .runoff_candidates
                                    .as_ref()
                                    .is_none_or(|candidates| candidates.contains(&j))
                        })
                        .collect(),
                    self.day * 37 + i as u32,
                );
                if let Some(&target) = others.first() {
                    self.ballots.insert(
                        i,
                        Ballot {
                            target,
                            automatic: true,
                        },
                    );
                    self.announce_private_fact(
                        world,
                        format!(
                            "{}没有提交选票，系统按既定规则代投给{}。",
                            self.label(i),
                            self.label(target)
                        ),
                        format!("vote:{}-{i}-automatic", self.day),
                    );
                } else {
                    // 陪审团只剩一个在途回合、没有可代投的对象时，不能让这张票静默消失。
                    self.remember(
                        world,
                        i,
                        "你未按时投票，系统代投也没有可选对象，本票作废。".into(),
                    );
                    self.outcome(
                        world,
                        i,
                        format!(
                            "{}：未按时投票，系统代投也没有可选对象，本票作废。",
                            self.label(i)
                        ),
                        format!("vote:{}-{i}-no-auto-target", self.day),
                        false,
                    );
                }
            }
        }
        self.revealed = true;
        let mut counts = BTreeMap::<usize, usize>::new();
        for (v, target, automatic) in self
            .ballots
            .iter()
            .map(|(&v, b)| (v, b.target, b.automatic))
            .collect::<Vec<_>>()
        {
            *counts.entry(target).or_default() += 1;
            self.announce(
                world,
                format!(
                    "{}记名投票给{}{}。",
                    self.label(v),
                    self.label(target),
                    if automatic { "（系统代投）" } else { "" }
                ),
                false,
            );
        }
        let tally = counts
            .iter()
            .map(|(&seat, &count)| format!("{} {count}票", self.label(seat)))
            .collect::<Vec<_>>()
            .join("、");
        if !tally.is_empty() {
            self.key(
                world,
                format!(
                    "第{}天{}揭票汇总：{tally}。",
                    self.day,
                    if self.runoff_candidates.is_some() {
                        "篝火会重投"
                    } else {
                        "篝火会"
                    }
                ),
                false,
            );
        }
        if let Some(max_count) = counts.values().copied().max() {
            let tied: Vec<usize> = counts
                .into_iter()
                .filter(|(_, n)| *n == max_count)
                .map(|(t, _)| t)
                .collect();
            if tied.len() > 1 {
                self.key(
                    world,
                    "最高票平票，平票候选人重新发言，随后全体在场参赛者仅在这些候选人中重新投票。"
                        .into(),
                    true,
                );
                self.start_runoff_speech(world, tied);
                return;
            }
            let target = tied[0];
            self.runoff_candidates = None;
            self.eliminate(world, target, "篝火会记名投票");
        }
        if self.has_departures() {
            self.begin_departures(world);
        } else if self.living().len() <= 1 {
            self.finish_single(world);
        } else if self.living().len() == 2 {
            self.start_jury(world);
        } else {
            self.start_night(world);
        }
    }
    fn start_jury(&mut self, world: &mut World) -> bool {
        self.phase = Phase::Jury;
        self.turn = None;
        self.runoff_candidates = None;
        // 各轮选票都不带轮次；进入陪审前必须清掉上一轮，否则剩下的几人会被当成已经投过。
        self.ballots.clear();
        self.revealed = false;
        self.until = self.at(self.day, DINNER).max(world.minutes() + JURY_LIMIT);
        self.announce(
            world,
            "出局者组成陪审团，为最后两人投票选出冠军；不能弃票。".into(),
            true,
        );
        for seat in self.jurors() {
            self.call(world, seat, "终局陪审投票开始，请调用 mode_action action=jury_vote，target 填仍在场候选人的座号。");
        }
        true
    }
    fn resolve_jury(&mut self, world: &mut World) {
        let jury = self.jurors();
        // 陪审投票同样不能弃票：超时的人按与篝火投票相同的固定规则代投，并留下可追溯的剧本事实。
        let candidates = self.living();
        for &i in &jury {
            if !self.ballots.contains_key(&i) {
                let others = self.rotated(
                    candidates.iter().copied().filter(|&j| j != i).collect(),
                    self.day * 43 + i as u32,
                );
                if let Some(&target) = others.first() {
                    self.ballots.insert(
                        i,
                        Ballot {
                            target,
                            automatic: true,
                        },
                    );
                    self.announce_private_fact(
                        world,
                        format!(
                            "{}没有提交陪审投票，系统按既定规则代投给{}。",
                            self.label(i),
                            self.label(target)
                        ),
                        format!("jury:{}-{i}-automatic", self.day),
                    );
                } else {
                    self.remember(
                        world,
                        i,
                        "你没有按时提交陪审团投票，系统也没有可选候选人，本票作废。".into(),
                    );
                    self.outcome(
                        world,
                        i,
                        format!(
                            "{}：未按时提交陪审投票，系统也没有可选候选人，本票作废。",
                            self.label(i)
                        ),
                        format!("jury:{}-{i}-no-auto-target", self.day),
                        false,
                    );
                }
            }
        }
        let mut counts = BTreeMap::<usize, usize>::new();
        // 终局会由 `finish` 先收口截止时间；揭票必须在此之前显式完成，否则视图会继续遮住票面。
        self.revealed = true;
        for (voter, target, automatic) in self
            .ballots
            .iter()
            .map(|(&voter, b)| (voter, b.target, b.automatic))
            .collect::<Vec<_>>()
        {
            *counts.entry(target).or_default() += 1;
            self.announce(
                world,
                format!(
                    "{}陪审投票给{}{}。",
                    self.label(voter),
                    self.label(target),
                    if automatic { "（系统代投）" } else { "" }
                ),
                false,
            );
        }
        // 正常一局至少有一位出局者组成陪审团，代投后一定有票；真正没有候选人或
        // 没有任何陪审员的旧档仍按“无人获胜”结算，不从候选人里按座位号发冠军。
        let winner = counts.values().copied().max().map(|max| {
            self.rotated(
                counts
                    .iter()
                    .filter(|(_, n)| **n == max)
                    .map(|(&t, _)| t)
                    .collect(),
                self.day * 23,
            )
            .into_iter()
            .next()
            .expect("有得票者")
        });
        if let Some(w) = winner {
            self.finish(
                world,
                winner,
                format!(
                    "陪审团选出{}为冠军；{}。",
                    self.label(w),
                    self.jury_tally(&counts)
                ),
            );
        } else {
            self.finish(world, None, "陪审团无有效选票，本局无人获胜。".into());
        }
    }
    fn jury_tally(&self, counts: &BTreeMap<usize, usize>) -> String {
        counts
            .iter()
            .map(|(&seat, &count)| format!("{} {count}票", self.label(seat)))
            .collect::<Vec<_>>()
            .join("、")
    }
    fn finish_single(&mut self, world: &mut World) {
        let winner = self.living().into_iter().next();
        let notice = if let Some(w) = winner {
            format!("{}成为最后的生存者。", self.label(w))
        } else {
            "没有参赛者存活，本局无人获胜。".into()
        };
        self.finish(world, winner, notice);
    }
    /// 判定赢家的当刻进入稳定终局：关闭全部未完成的玩法流程，所有人只离场这一次；
    /// 随后 `advance` 不再推进，保存重开也保持本相。
    fn finish(&mut self, world: &mut World, winner: Option<usize>, notice: String) {
        if self.phase == Phase::Over {
            return;
        }
        self.winner = winner;
        self.notice = notice.clone();
        self.phase = Phase::Over;
        self.end_participation(world);
        self.key(world, notice, true);
    }
    fn start_night(&mut self, world: &mut World) {
        self.resume_phase = None;
        self.phase = Phase::Night;
        self.night_orders.clear();
        self.night_reminded.clear();
        self.last_public_stolen = 0;
        self.until = self.at(self.day + 1, DAWN).max(world.minutes() + 60.0);
        self.announce(
            world,
            "夜间禁止白天领米和劳动；睡觉、偷公仓、偷别人家、守家四选一，每人仅一次。公仓的取粮动作此时只用于已登记的偷公仓，私粮袋只用于已登记目标的入室偷粮；同一目标的重叠偷粮动作会撞车，相关者全部失败且本夜不能重试。偷粮必须完成十分钟真实动作，单纯到场不会到账。".into(),
            true,
        );
        for &i in &self.living() {
            if self.seats[i].last_guard_day == self.day.checked_sub(1) {
                self.call(world, i, "你上一晚已经守家，今晚不能再次登记守家；仍可睡觉、偷公仓、偷别人家或什么都不做。");
            }
        }
    }
    /// 夜间截止前还没提交的人只收到一次点名提醒；不替他们选，也不补发粮食。
    fn check_night(&mut self, world: &mut World) {
        let now = world.minutes();
        let left = self.until - now;
        if !(0.0..=60.0).contains(&left) {
            return;
        }
        for &i in &self.living() {
            if !self.night_orders.contains_key(&i) && !self.night_reminded.contains(&i) {
                let id = self.seats[i].id.clone();
                let already_notified = world.actor(&id).is_some_and(|actor| {
                    actor
                        .journal
                        .events
                        .iter()
                        .rev()
                        .take_while(|event| event.at >= self.until - 60.0)
                        .any(|event| {
                            event.kind == FactKind::Notice
                                && (event.text.ends_with(NIGHT_REMINDER)
                                    || event.text.ends_with(LEGACY_NIGHT_REMINDER))
                        })
                });
                self.night_reminded.push(i);
                if !already_notified {
                    world.notify(&id, NIGHT_REMINDER.into());
                }
            }
        }
    }
    fn resolve_night(&mut self, world: &mut World) {
        for (&seat, attempt) in &self.night_orders {
            if attempt.order == NightOrder::Guard {
                self.seats[seat].last_guard_day = Some(self.day);
            }
        }
        // 取粮、偷窃、守家和目击都在动作完成那一刻结算；天亮这里只记录没有完成的登记。
        for &i in &self.living() {
            let Some(n) = self.night_orders.get(&i) else {
                let text = "你今夜没有提交夜间行动，因此什么也没做。";
                self.remember(world, i, text.into());
                self.outcome(
                    world,
                    i,
                    format!("{}：{text}", self.label(i)),
                    format!("night:{}-{i}-missing", self.day),
                    false,
                );
                continue;
            };
            let outcome = match n.order {
                NightOrder::Sleep if !n.slept => Some(if n.on_bed && !n.sleep_left {
                    "你登记了睡觉，天亮时还没有在床上睡完整场睡眠；本夜没有有效睡觉。"
                } else if n.sleep_left {
                    "你登记了睡觉，但已经起床离开；天亮时没有在床上睡到清晨，本夜没有有效睡觉。"
                } else {
                    "你登记了睡觉，但没有在自己住宅的床上完成一场睡眠；本夜没有有效睡觉。"
                }),
                NightOrder::Guard if !self.keeping_guard(world, i) => {
                    Some("你登记了守家，但昨夜没有一直保持在自己住宅醒着；守家保护失效。")
                }
                NightOrder::StealPublic if !n.settled => {
                    Some("你选择了偷公仓，但没有在粮仓完成取粮动作，什么也没拿到。")
                }
                NightOrder::StealHome(_) if !n.settled => {
                    Some("你选择了入室偷粮，但没有在目标私粮袋旁完成取粮动作，什么也没拿到。")
                }
                _ => None,
            };
            if let Some(text) = outcome {
                self.remember(world, i, text.into());
                self.outcome(
                    world,
                    i,
                    format!("{}：{text}", self.label(i)),
                    format!("night:{}-{i}-unfinished", self.day),
                    false,
                );
            }
        }
        if self.living().len() <= 1 {
            self.finish_single(world);
        } else if self.living().len() == 2 {
            self.start_jury(world);
        } else {
            // 新的一天：先清晨广播，再各自安排真实行动。
            self.start_day(world);
        }
    }
    fn maybe_advance(&mut self, world: &mut World) -> bool {
        if world.minutes() + EPS < self.until {
            return false;
        }
        match self.phase {
            Phase::Waiting => self.start_day(world),
            Phase::Day => self.resolve_day_and_dinner(world),
            Phase::Gather => self.start_speech(world),
            Phase::Speech => return false,
            Phase::Vote => self.resolve_vote(world),
            Phase::Night => self.resolve_night(world),
            Phase::Jury => self.resolve_jury(world),
            Phase::LastWords => self.finish_farewell(world, None, None),
            Phase::Over => return false,
        }
        true
    }
    fn actions(&self, i: usize) -> Vec<&'static str> {
        if self.phase == Phase::LastWords && self.turn == Some(i) {
            return vec!["farewell"];
        }
        if self.phase == Phase::Jury && self.seats[i].out.is_some() {
            return if self.ballots.contains_key(&i) {
                vec![]
            } else {
                vec!["jury_vote"]
            };
        }
        if self.seats[i].out.is_some() {
            return vec![];
        }
        match self.phase {
            Phase::Day => {
                let mut a = Vec::new();
                if self.day_orders.get(&i).is_none_or(|d| !d.locked) {
                    if self.registered_today() < MAX_WORKERS {
                        a.push("work");
                    }
                    a.push("take_rice");
                }
                if !self.whispers_used.contains(&i) {
                    a.push("whisper");
                }
                a
            }
            Phase::Speech if self.turn == Some(i) => vec!["speak"],
            Phase::Vote if !self.ballots.contains_key(&i) => vec!["vote"],
            Phase::Night if !self.night_orders.contains_key(&i) => {
                let mut actions = vec!["sleep", "steal_public", "steal_home"];
                if self.seats[i].last_guard_day != self.day.checked_sub(1) {
                    actions.push("guard_home");
                }
                actions
            }
            _ => vec![],
        }
    }
    /// 测试地图没有 `market_1` 时，把粮仓挪到测试地点；真实小镇不用。
    pub(crate) fn move_granary(&mut self, locale: &str) {
        self.granary = locale.to_string();
    }
    pub(crate) fn give_rice(&mut self, actor: &str, bags: u32) {
        if let Some(i) = self.seat_of(actor) {
            self.seats[i].rice = self.seats[i].rice.saturating_add(bags);
        }
    }
}

impl Rules for Survival10 {
    const ID: &'static str = "survival10";
    fn director_event(&mut self, world: &mut World, event: &GameModeEvent) -> Result<String> {
        let GameModeEvent::Survival10Disaster(event) = event;
        self.trigger_disaster(world, event)
    }
    fn allows_chat(&self, world: &World, actor: &str) -> bool {
        let Some(seat) = self.seat_of(actor) else {
            return true;
        };
        self.phase != Phase::Day || !self.field(world, seat, world.minutes())
    }
    fn restore(&mut self, world: &mut World) {
        self.restore(world);
    }
    fn offstage_action(&self, actor: &str) -> bool {
        (self.phase == Phase::LastWords && self.turn == self.seat_of(actor))
            || self.phase == Phase::Jury
                && self.seat_of(actor).is_some_and(|seat| {
                    self.seats[seat].out.is_some() && !self.ballots.contains_key(&seat)
                })
    }
    fn allows_decision(&self, actor: &str) -> bool {
        if self.phase == Phase::Over {
            return false;
        }
        let Some(seat) = self.seat_of(actor) else {
            return true;
        };
        if matches!(self.phase, Phase::LastWords | Phase::Jury) || self.seats[seat].out.is_some() {
            return !self.actions(seat).is_empty();
        }
        true
    }
    fn adjust_supply(
        &mut self,
        world: &mut World,
        prop: &str,
        action: &str,
        adjustment: crate::props::SupplyAdjustment,
        reason: &str,
    ) -> Result<Option<(u32, u32)>> {
        if self.granary_ledger(world).as_deref() != Some(prop) || action != GRANARY_ACTION {
            return Ok(None);
        }
        let before = self.public_rice;
        let after = adjustment.apply(before)?;
        if after > 1_000_000 {
            return Err(KernelError::action("比赛公粮不能超过100万袋"));
        }
        self.public_rice = after;
        self.supply_changes.push(SupplyChange {
            at: world.minutes(),
            prop: prop.into(),
            action: action.into(),
            before,
            after,
            reason: reason.into(),
        });
        self.sync_granary(world);
        Ok(Some((before, after)))
    }
    fn ended(&self, _world: &World) -> bool {
        self.phase == Phase::Over
    }
    fn settled(&self, world: &World) -> bool {
        // 集合截止（或会议、投票、陈词截止）与入夜同时跨过时，这一拍已经进入夜间。
        // 不能等到下一次调用才补一次夜间结算，否则新开的夜晚会被随后的日历时刻越过。
        self.phase == Phase::Night && world.minutes() + EPS >= self.until
    }
    fn track_interval(&mut self, world: &World, from: f64, to: f64) {
        // 白天劳动按真实世界时间累计；只认在农田的干活动作，走路、空等和别的事务都不算。
        if self.phase != Phase::Day || to <= from {
            return;
        }
        for &i in &self.living() {
            if world.actor(&self.seats[i].id).is_none() {
                continue;
            }
            let started = self.field_started(world, i, from);
            let at_end = self.field_started(world, i, to);
            let Some(started) = started.or(at_end) else {
                continue;
            };
            if started > from && at_end.is_none() {
                // 这一片开始时还没干活，片尾也停下来时，只认以来回都看得到的起点。
                continue;
            }
            let effort = to - started.clamp(from, to);
            if effort <= EPS {
                continue;
            }
            if let Some(attempt) = self.day_orders.get_mut(&i) {
                attempt.reached = true;
                if attempt.order == DayOrder::Work {
                    attempt.worked = (attempt.worked + effort).min(WORK_REQUIRED);
                }
            }
        }
    }
    fn boundary(&self, world: &World) -> f64 {
        if self.phase == Phase::Over {
            return f64::INFINITY;
        }
        // 边界正好落在这一分钟时先允许本分钟末尾的结算；下一片真正的零片长由
        // `advance` 消费并交给真实动作，不再以 0 分钟边界提前轮走发言人。
        let left = (self.until - world.minutes()).max(0.0);
        // 白天劳动和夜间看守、睡觉、收粮都按世界时间每分钟检查一次真实状态。
        // 先说、投的人数已经确认的阶段，本拍末尾已完成结算；再以 0 时长重入会
        // 让 22:00 的日历边界盖掉下一分钟，从那里原地卡住。
        if matches!(
            self.phase,
            Phase::Day
                | Phase::Gather
                | Phase::Night
                | Phase::Vote
                | Phase::Jury
                | Phase::LastWords
        ) {
            left.min(1.0)
        } else {
            left
        }
    }
    fn required_action(&self, _world: &World, actor: &str) -> Option<String> {
        let seat = self.seat_of(actor)?;
        if self.phase == Phase::LastWords && self.turn == Some(seat) {
            return Some("轮到你的离场陈词：调用 mode_action action=farewell，text 留言，target 可选指认；不想说可提交空 text。".into());
        }
        if self.phase == Phase::Jury
            && self.seats[seat].out.is_some()
            && !self.ballots.contains_key(&seat)
        {
            return Some(
                "你是陪审员，调用 mode_action action=jury_vote 为两位候选人之一投票。".into(),
            );
        }
        (self.phase == Phase::Speech && self.turn == self.seat_of(actor)).then(|| {
            "篝火会轮到你了：必须到场并保持清醒，调用 mode_action，action=speak，text填写公开说出口的原话。想法、reason、只查看或沉默不算发言；裁判会等待你的真实发言，不会代写。".into()
        })
    }
    fn speech(&self, actor: &str) -> Option<protocol::SpeechView> {
        let seat = self.seat_of(actor)? as u32 + 1;
        let (index, line) = self
            .lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, line)| line.seat == seat)?;
        Some(protocol::SpeechView {
            chat_id: format!(
                "survival10:{}:{}:{}",
                self.day,
                self.runoff_candidates.is_some(),
                self.speech_round
            ),
            seq: index as u64 + 1,
            at: line.at,
            text: line.text.clone(),
            line: crate::journal::first_sentence(&line.text),
        })
    }
    fn tempo(&self, _world: &World) -> f64 {
        if matches!(
            self.phase,
            Phase::Speech | Phase::Vote | Phase::Jury | Phase::LastWords
        ) {
            1.0
        } else {
            3.0
        }
    }
    fn advance(&mut self, world: &mut World) {
        if self.phase == Phase::Speech {
            // 截止时仍未开口的人先如实轮走；新阶段已经超过截止时，未发言的席位一并收口，
            // 不能在下一阶段里反复补走这个已经到期的会议。
            let seat = self.turn;
            if let Some(seat) = seat {
                if !self.lines.iter().any(|line| line.seat == seat as u32 + 1) {
                    self.remember(
                        world,
                        seat,
                        "这一轮篝火发言等待到截止时仍未提交公开发言，本轮没有你的原话。".into(),
                    );
                    world.log(
                        EventInfo::default(),
                        format!(
                            "{}在篝火发言截止时未提交公开发言，本轮没有原话记录。",
                            self.label(seat)
                        ),
                    );
                    world.interrupt_actions(&self.seats[seat].id, "篝火发言已到截止");
                }
            }
            self.next_speaker(world);
        }
        // 终局是稳定状态：不再同步晚上的补给、发提醒或推进下一步；判定赢家那一刻已统一收口。
        if self.phase == Phase::Over {
            return;
        }
        // 走到这里说明这一拍跨过了玩法截止；先处理本拍产生的出局和陈词，
        // 不能让随后的待陈词者被这一拍末尾的日历换日悄悄移出世界。
        if self.phase != Phase::LastWords && !self.departures.is_empty() {
            self.begin_departures(world);
            return;
        }
        self.sync_all_supplies(world);
        // 已出局、还在等陈词的人不能被这里直接开成陪审团；他们只在本轮陈词队列走完后
        // 由 `resume_after_farewell` 统一定去路。
        if self.phase != Phase::LastWords && !self.departures.is_empty() {
            self.begin_departures(world);
            return;
        }
        // 先处理本分钟的真实夜间状态；有人可能已不在床上，或结束守家失效了。
        // 若此时已经只剩一人存活，再由这里统一终局；不能把最后一人重开成陪审团。
        if self.phase == Phase::Night {
            self.track_night(world);
        }
        if self.live_count() <= 1 && self.departures.is_empty() {
            self.finish_single(world);
            return;
        } else if self.phase != Phase::Jury && self.live_count() == 2 && self.departures.is_empty()
        {
            self.start_jury(world);
        }
        if self.phase == Phase::Over {
            return;
        }
        if self.phase == Phase::Night {
            self.check_night(world);
        }
        for _ in 0..32 {
            if !self.maybe_advance(world) {
                break;
            }
            if self.phase == Phase::Over {
                break;
            }
        }
    }
    fn view(&self, world: &World) -> GameModeView {
        GameModeView::Survival10(Survival10View {
            day: self.day,
            phase: self.phase,
            public_rice: self.public_rice,
            produced_today: self.produced_today,
            registered_today: self.registered_today() as u32,
            seats: self
                .seats
                .iter()
                .enumerate()
                .map(|(i, s)| Survival10Seat {
                    seat: i as u32 + 1,
                    id: s.id.clone(),
                    name: s.name.clone(),
                    rice: s.rice,
                    farm: self
                        .day_orders
                        .get(&i)
                        .and_then(|attempt| attempt.farm.clone()),
                    worked_minutes: self
                        .day_orders
                        .get(&i)
                        .filter(|d| d.order == DayOrder::Work)
                        .map_or(0.0, Self::worked_minutes),
                    labor_progress: self.labor_progress(world, i),
                    working_now: self.field(world, i, world.minutes()),
                    hungry_streak: s.hungry_streak,
                    out: s.out.clone(),
                    out_day: s.out_day,
                })
                .collect(),
            notice: self.notice.clone(),
            until: (self.phase != Phase::Over && self.until.is_finite()).then_some(self.until),
            turn: self.turn.map(|i| i as u32 + 1),
            lines: self.lines.clone(),
            ballots: self
                .ballots
                .iter()
                .map(|(&i, b)| protocol::Survival10Ballot {
                    voter: i as u32 + 1,
                    target: self.revealed.then_some(b.target as u32 + 1),
                    automatic: b.automatic,
                })
                .collect(),
            revealed: self.revealed,
            history: self.history.iter().rev().take(16).cloned().collect(),
            winner: self.winner.map(|i| i as u32 + 1),
            farewells: self.farewells.clone(),
            disasters: self
                .disasters
                .iter()
                .map(|record| record.notice.clone())
                .collect(),
        })
    }
    fn context(&self, world: &World, actor: &str) -> Option<GameModeContext> {
        world.actor(actor)?;
        let i = self.seat_of(actor)?;
        let s = &self.seats[i];
        let others = self
            .seats
            .iter()
            .enumerate()
            .map(|(j, p)| {
                format!(
                    "{}号 {}：{}",
                    j + 1,
                    p.name,
                    if p.out.is_some() {
                        "已出局"
                    } else {
                        "在场"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("；");
        let actions = self.actions(i).into_iter().map(str::to_string).collect();
        let now = if self.phase == Phase::Over {
            match self.winner {
                Some(w) => format!(
                    "本局已经结束：{}获得冠军。终局不再接受玩法动作，也不会继续过夜。",
                    self.label(w)
                ),
                None => {
                    "本局已经结束，没有参赛者获胜；终局不再接受玩法动作，也不会继续过夜。".into()
                }
            }
        } else {
            format!("{} {}", self.notice, self.disaster_status())
                .trim()
                .to_string()
        };
        Some(GameModeContext::Survival10(Survival10Context {
            day:self.day,phase:format!("{:?}",self.phase),
            me:self.label(i),
            goal:if s.out.is_some() {
                "你已出局，自己的生存目标未完成；如获陪审投票资格，按实际经历评判终局候选人，不能假定自己仍在场或能复活。"
            } else {
                "最高目标是自己保持参赛资格，活到第十天终局；先保自己的晚餐与留场机会，主动自救，不以情义、秩序或体面为由主动牺牲。生存基础上再争取陪审团认可与冠军。价值观影响求生方式，不可凭空假定他人的秘密。"
            }.into(),
            public_rice:self.public_rice,my_rice:s.rice,
            registered_today:self.registered_today() as u32,
            farm:self.day_orders.get(&i).and_then(|attempt| attempt.farm.clone()),
            worked_minutes:self.day_orders.get(&i).filter(|d| d.order == DayOrder::Work).map_or(0, |d| Self::worked_minutes(d) as u32),
            labor_required_minutes:WORK_REQUIRED as u32,
            hungry_streak:s.hungry_streak,
            seats:others,known:s.private_notes.iter().rev().take(24).cloned().collect(),
            history:self.history.iter().rev().take(12).cloned().collect(),
            now,actions,
        }))
    }
    fn tool(&self, _world: &World, actor: &str) -> Option<String> {
        let i = self.seat_of(actor)?;
        if self.actions(i).is_empty() {
            return None;
        }
        if self.phase == Phase::LastWords {
            return Some("farewell 离场陈词：text 为0到400字的真实陈词、预言或署名纸条，target 可选指认座号；这仅为你的说法，不能转赠粮食或创造道具。空 text 表示放弃，五分钟超时自动离场。".into());
        }
        Some("十日生存实验：仅在 survival10.actions 允许时使用 mode_action；提交只是选择，领米与偷粮按真实完成结算，产粮按劳动报名结算；赛事天灾公告指定的绝收日除外，当日产粮为0，仍可报名但不能改领米，请先查看题面灾情。\
        action：work 白天报名劳动，每天最多四人，先到先得；报名成功分配田块，到该田块执行下地干活，仍按实际有效劳动累计八小时安排劳动，后台按报名每人结算一袋入公仓，不要求准时到田；劳动中不能交谈，主动结束劳动后可在白天回田继续累计；当天不能再白天领米，晚餐使用已有私粮；名额不转让、进度不过夜；\
        take_rice 白天登记领米，到公共粮仓对粮仓执行十分钟取粮，完成时立即将一袋米放进自己的私粮，不必等晚餐或回家；中断后可重试，当天只能成功领取一袋；whisper 给指定 target 发保密信(text，次日送达)；\
        sleep 睡觉，必须回到自己家并睡到床上；steal_public 偷公仓，必须到粮仓取米；\
        steal_home 夜间进入 target 的家取私粮；guard_home 守家，必须回自己住宅并保持清醒，不能连续两晚登记，中断也占用冷却；同一公仓或同一家私粮袋的偷粮动作真实时间重叠即撞车，相关者全部失败，本夜不能重试；先后不重叠不撞车；\
        speak 轮到本人时必须到篝火广场并保持清醒，用text填写说出口的原话；thought和reason不算发言，未公开发言不跳人；vote 全部发言轮次结束后到篝火广场并保持清醒记名投票(target,不可弃权，25分钟内，超时系统代投)；jury_vote 陪审团投票(target)；farewell 离场陈词(text，最多400字，可选target指认；纸条、预言仅为本人说法，不创造物品或转赠资源，空text放弃)。\
        target 填座号，reason 可写动机。劳动仍须按原有规则执行，后台产粮按报名结算；领米与偷粮须在对应目标完成真实取粮动作。\
        每人每晚要吃1袋个人存粮，连饿两晚出局；公开粮仓初始21袋、每人开局私粮1袋（仅新局配给）、每日生产最多4袋；偶数天18:00晚餐后前往篝火广场，18:30晚会开始（晚餐延后则集合顺延，留足30分钟），剩两人先完成离场陈词再由陪审团决定冠军，剩一人立即获胜并结束。淘汰者私粮归公仓，不另给投票奖励。找不到偷粮确证时可独立依据已见领粮、晚餐、本人声明等线索按资源救济理由支持淘汰，并说明依据来源；私粮不公开，不得编造库存或把囤粮当作偷粮证明。也要权衡劳动力、盟友与被淘汰者的评审票，不要机械投最富者。".into())
    }
    fn check_prop_action(
        &self,
        world: &World,
        actor: &str,
        spot: &crate::affordance::Spot,
    ) -> Result<()> {
        if Self::is_granary_collect(world, spot) {
            return self.check_collection(world, actor, spot).map(|_| ());
        }
        if Self::is_pantry_collect(world, spot) {
            return self.check_home_collection(world, actor, spot).map(|_| ());
        }
        if self.phase == Phase::Night && Self::is_sleep(spot) {
            let seat = self
                .seat_of(actor)
                .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
            if self.seats[seat].out.is_some()
                || !self
                    .night_orders
                    .get(&seat)
                    .is_some_and(|attempt| attempt.order == NightOrder::Sleep)
                || world.topo.prop(&spot.prop).map(|prop| prop.locale.as_str())
                    != self.home_of(world, seat).as_deref()
            {
                return Err(KernelError::action(
                    "夜间必须选择睡觉并使用自家床，不能选择偷粮或守家后再睡觉",
                ));
            }
        }
        if !Self::is_farm(world, &spot.prop) || !Self::is_labor_spot(spot) {
            return Ok(());
        }
        let seat = self
            .seat_of(actor)
            .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
        self.work_farm(world, seat, Some(&spot.prop)).map(|_| ())
    }
    fn start_prop_action(
        &mut self,
        world: &World,
        actor: &str,
        spot: &crate::affordance::Spot,
    ) -> Result<Option<f64>> {
        self.check_prop_action(world, actor, spot)?;
        if Self::is_granary_collect(world, spot) {
            self.check_collection(world, actor, spot)?;
            let seat = self.seat_of(actor).expect("取粮已校验参赛者");
            if self.phase == Phase::Night {
                self.open_theft_window(world, seat, spot, world.minutes())?;
            }
            return Ok(Some(COLLECT_MINUTES));
        }
        if Self::is_pantry_collect(world, spot) {
            let (seat, target) = self.check_home_collection(world, actor, spot)?;
            if let Some(attempt) = self.night_orders.get_mut(&seat) {
                attempt.stage = NightStage::Entering(target);
            }
            self.open_theft_window(world, seat, spot, world.minutes())?;
            return Ok(Some(COLLECT_MINUTES));
        }
        if !Self::is_farm(world, &spot.prop) || !Self::is_labor_spot(spot) {
            return Ok(None);
        }
        let seat = self
            .seat_of(actor)
            .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
        self.register_work(world, seat, Some(&spot.prop))?;
        Ok(Some(
            (WORK_REQUIRED - Self::worked_minutes(&self.day_orders[&seat])).min(spot.minutes),
        ))
    }
    fn complete_prop_action(
        &mut self,
        world: &mut World,
        actor: &str,
        spot: &crate::affordance::Spot,
    ) -> Result<Option<Vec<String>>> {
        let Some(seat) = self.seat_of(actor) else {
            return Ok(None);
        };
        if self.phase == Phase::Night
            && (Self::is_granary_collect(world, spot) || Self::is_pantry_collect(world, spot))
        {
            return self.finish_theft(world, seat, spot);
        }
        if Self::is_pantry_collect(world, spot) {
            return Err(KernelError::action(
                "私粮袋只能在夜间登记偷别人家后执行偷粮",
            ));
        }
        if !Self::is_granary_collect(world, spot) {
            return Ok(None);
        }
        self.collect_rice(world, seat, actor, spot)
    }
    fn interrupt_after_start(&mut self, world: &mut World) {
        self.interrupt_theft_colliders(world);
    }
    fn interrupt_after_complete(&mut self, world: &mut World) {
        self.interrupt_theft_colliders(world);
    }
    fn act(&mut self, world: &mut World, actor: &str, a: &GameModeAction) -> Result<String> {
        if self.phase == Phase::Over {
            let winner = self.winner.map_or_else(
                || "本局无人获胜".to_string(),
                |w| format!("{}获得冠军", self.label(w)),
            );
            return Err(KernelError::action(format!(
                "本局已结束：{winner}，不再接受玩法动作"
            )));
        }
        let i = self
            .seat_of(actor)
            .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
        if self.phase == Phase::Night
            && a.action == "guard_home"
            && self.seats[i].last_guard_day == self.day.checked_sub(1)
        {
            return Err(KernelError::action(format!(
                "你上一晚已经登记守家，第{}晚才能再次守家",
                self.day + 1
            )));
        }
        let allowed = self.actions(i);
        if a.action == "work" {
            self.work_farm(world, i, None)?;
        }
        if !allowed.contains(&a.action.as_str()) {
            return Err(KernelError::action(format!(
                "当前阶段不能提交 {}，可用：{:?}",
                a.action, allowed
            )));
        }
        if matches!(a.action.as_str(), "speak" | "vote") && !self.at_meeting(world, i) {
            return Err(KernelError::action(
                "必须本人到达篝火广场并保持清醒，才能发言或自主投票",
            ));
        }
        let answer = match a.action.as_str() {
            "farewell" => {
                let text = a.text.as_deref().unwrap_or("").trim();
                if text.chars().count() > MAX_TEXT {
                    return Err(KernelError::action("离场陈词最多400字"));
                }
                let target = a
                    .target
                    .as_ref()
                    .filter(|target| !target.trim().is_empty())
                    .map(|_| self.target(a).map(|seat| seat as u32 + 1))
                    .transpose()?;
                self.finish_farewell(world, Some(text.into()), target);
                "离场陈词已记录，已转为陪审员；没有转赠粮食或生成道具。".into()
            }
            "work" => {
                let farm = self.register_work(world, i, None)?;
                self.choice(
                    world,
                    i,
                    format!(
                        "把今天的行动登记为劳动，分配到{farm}；报名 {}/4。登记工作不等于已经挣到收成。",
                        self.registered_today()
                    ),
                    format!("work:{}-{}-{i}", self.seed, self.day),
                    true,
                );
                let field = world.topo.prop(&farm).map(|prop| prop.locale.clone());
                if let Some(field) = field {
                    self.guide(
                        world,
                        i,
                        crate::intent::Intent::Goto {
                            locale: field,
                            station: None,
                        },
                    );
                }
                format!("已登记今日工作，报名 {}/4，分配田块 {farm}。请到公共农田对该田块执行下地干活；中断后可补足八小时，收成在白天结束时按报名结算。{}", self.registered_today(), if self.crop_failure(self.day) { "注意：今日天灾绝收，即使劳动也不产粮，不能改领米。" } else { "" })
            }
            "take_rice" => {
                self.day_orders.insert(
                    i,
                    DayAttempt {
                        order: DayOrder::Take,
                        locked: true,
                        ..DayAttempt::default()
                    },
                );
                self.choice(
                    world,
                    i,
                    "把今天的行动登记为领米，之后必须到公共粮仓完成取粮；登记不等于已经领到米。"
                        .into(),
                    format!("take:{}-{}-{i}", self.seed, self.day),
                    true,
                );
                let goal = crate::intent::Intent::Goto {
                    locale: self.granary_point(world),
                    station: None,
                };
                self.guide(world, i, goal);
                "已登记领米。请到公共粮仓，对粮仓道具执行十分钟「取粮」；完成时公仓扣一袋、自己的私粮加一袋，可立即离开，不必等晚餐或回家。"
                    .into()
            }
            "whisper" => {
                if self.whispers_used.contains(&i) {
                    return Err(KernelError::action("今天已经发过一条悄悄话，明天再说"));
                }
                let to = self.target(a)?;
                if i == to || self.seats[to].out.is_some() {
                    return Err(KernelError::action("只能向其他未出局者发送悄悄话"));
                }
                let text = a.text.as_deref().unwrap_or("").trim();
                if text.is_empty() || text.chars().count() > MAX_TEXT {
                    return Err(KernelError::action("悄悄话需填写1至400字"));
                }
                let mut letter = Whisper {
                    from: i,
                    to,
                    sent_day: self.day,
                    content: text.into(),
                    archive_id: None,
                };
                let id = self.archive_whisper(&letter, Some(world.minutes()));
                let content = letter.content.clone();
                letter.archive_id = Some(id);
                self.pending_whispers.push(letter);
                self.whispers_used.push(i);
                // 收件人要到次日清晨才看见；玩家作为旁观者可以现在读到封存的原话。
                let sender = self.seats[i].id.clone();
                let receiver = self.seats[to].id.clone();
                world.manuscript_fact(
                    ManuscriptKind::Choice,
                    ManuscriptVisibility::Private,
                    Some(sender.as_str()),
                    None,
                    format!(
                        "给{}写悄悄话（第{}天清晨送达）：“{content}”",
                        self.label(to),
                        self.day + 1
                    ),
                    vec![sender.clone(), receiver],
                    format!("whisper:{id}"),
                );
                format!(
                    "悄悄话已封存，第{}天清晨送达{}",
                    self.day + 1,
                    self.label(to)
                )
            }
            "sleep" => {
                self.night_orders.insert(
                    i,
                    NightAttempt {
                        order: NightOrder::Sleep,
                        stage: NightStage::Chosen,
                        ..NightAttempt::default()
                    },
                );
                self.choice(
                    world,
                    i,
                    "夜里选择回自己家睡觉；天亮前离开床或做别的事会让这次睡眠失效。".into(),
                    format!("sleep:{}-{}-{i}", self.seed, self.day),
                    false,
                );
                if let Some(home) = self.home_of(world, i) {
                    self.guide(
                        world,
                        i,
                        crate::intent::Intent::Goto {
                            locale: home,
                            station: None,
                        },
                    );
                }
                "已登记睡觉。请回到自己住宅，到自己的床上开始睡觉并让这场睡眠自然结束；中途起床、离开或做别的事都会中断，本夜也不能再换别的行动。".into()
            }
            "steal_public" => {
                self.night_orders.insert(
                    i,
                    NightAttempt {
                        order: NightOrder::StealPublic,
                        stage: NightStage::Chosen,
                        ..NightAttempt::default()
                    },
                );
                self.choice(
                    world,
                    i,
                    "夜里选择偷公共粮仓；要走到粮仓真实完成取粮才算得手。".into(),
                    format!("steal_public:{}-{}-{i}", self.seed, self.day),
                    false,
                );
                self.guide(
                    world,
                    i,
                    crate::intent::Intent::Goto {
                        locale: self.granary_point(world),
                        station: None,
                    },
                );
                "已登记偷公仓，这不是白天领米。请走到公共粮仓并执行十分钟「取粮」（本阶段按偷窃结算），动作完成后才到账；没完成不会补发。".into()
            }
            "guard_home" => {
                self.seats[i].last_guard_day = Some(self.day);
                self.night_orders.insert(
                    i,
                    NightAttempt {
                        order: NightOrder::Guard,
                        stage: NightStage::Chosen,
                        ..NightAttempt::default()
                    },
                );
                self.choice(
                    world,
                    i,
                    "夜里选择守自己家；要整夜留在住宅并保持清醒才有效。".into(),
                    format!("guard:{}-{}-{i}", self.seed, self.day),
                    false,
                );
                if let Some(home) = self.home_of(world, i) {
                    self.guide(
                        world,
                        i,
                        crate::intent::Intent::Goto {
                            locale: home,
                            station: None,
                        },
                    );
                }
                "已登记守家。请回自己住宅并保持清醒一直到清晨；离开或睡着期间守家立即失效，偷窃按发生那一刻判断。".into()
            }
            "steal_home" => {
                let target = self.target(a)?;
                if i == target || self.seats[target].out.is_some() {
                    return Err(KernelError::action("不能偷自己或出局者的家"));
                }
                self.night_orders.insert(
                    i,
                    NightAttempt {
                        order: NightOrder::StealHome(target),
                        stage: NightStage::Searching(target),
                        ..NightAttempt::default()
                    },
                );
                self.choice(
                    world,
                    i,
                    format!(
                        "夜里选择进入{}家偷粮；只到场不取粮不算得手。",
                        self.label(target)
                    ),
                    format!("steal_home:{}-{}-{i}", self.seed, self.day),
                    false,
                );
                if let Some(home) = self.home_of(world, target) {
                    self.guide(
                        world,
                        i,
                        crate::intent::Intent::Goto {
                            locale: home,
                            station: None,
                        },
                    );
                }
                format!(
                    "已登记夜间入室。请找到{}的住宅，对其私粮袋执行十分钟「取粮」（本阶段按偷窃结算）并完成动作；只到场、空等或停在家门口不会到账。",
                    self.label(target)
                )
            }
            "speak" => {
                let text = a.text.as_deref().unwrap_or("").trim();
                if text.is_empty() || text.chars().count() > MAX_TEXT {
                    return Err(KernelError::action("发言需填写1至400字"));
                }
                self.lines.push(Survival10Line {
                    seat: i as u32 + 1,
                    text: text.into(),
                    at: world.minutes(),
                });
                let info = EventInfo::speech(
                    &self.seats[i].id,
                    &self.seats[i].name,
                    EventSpeech {
                        occasion: "篝火会议".into(),
                        seat: Some(i as u32 + 1),
                        text: text.into(),
                        public: true,
                    },
                );
                world.log(info, format!("{}在篝火会上说：{}", self.label(i), text));
                let heard = format!("{}在篝火会上说：{}", self.label(i), text);
                let now = world.minutes();
                for listener in self.living() {
                    if self.at_meeting(world, listener) {
                        world.actor_mut(&self.seats[listener].id).journal.fact(
                            now,
                            FactKind::Speech,
                            heard.clone(),
                            vec![self.seats[i].id.clone()],
                            None,
                            None,
                        );
                    }
                }
                self.next_speaker(world);
                "已公开发言".into()
            }
            "vote" | "jury_vote" => {
                let target = self.target(a)?;
                if self.seats[target].out.is_some() || i == target {
                    return Err(KernelError::action("必须投给另一位仍在场的参赛者"));
                }
                if self.phase == Phase::Vote
                    && self
                        .runoff_candidates
                        .as_ref()
                        .is_some_and(|candidates| !candidates.contains(&target))
                {
                    return Err(KernelError::action("重投只能投给平票候选人"));
                }
                self.ballots.insert(
                    i,
                    Ballot {
                        target,
                        automatic: false,
                    },
                );
                let round = if self.phase == Phase::Jury {
                    "终局陪审"
                } else if self.runoff_candidates.is_some() {
                    "篝火会重投"
                } else {
                    "篝火会投票"
                };
                // 提交后先给玩家看，但不算任何其他居民的见闻；揭票处另有公开汇总。
                world.manuscript_fact(
                    ManuscriptKind::Choice,
                    ManuscriptVisibility::Private,
                    Some(&self.seats[i].id),
                    None,
                    format!(
                        "{round}提交：把票投给{}（揭票前未向其他参赛者公开）。",
                        self.label(target)
                    ),
                    vec![self.seats[i].id.clone(), self.seats[target].id.clone()],
                    format!(
                        "ballot:{}-{}-{}-{}",
                        self.seed,
                        self.day,
                        i,
                        self.ballots.len()
                    ),
                );
                if self.phase == Phase::Vote && self.ballots.len() == self.living().len()
                    || self.phase == Phase::Jury && self.ballots.len() == self.jurors().len()
                {
                    self.until = world.minutes();
                }
                "已登记选票，揭票前不向其他参赛者公开目标".into()
            }
            _ => return Err(KernelError::action("未知玩法动作")),
        };
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seeded_order_is_deterministic() {
        let a = Survival10 {
            seed: 1,
            first_day: 0,
            day: 1,
            phase: Phase::Waiting,
            until: 0.0,
            public_rice: 21,
            produced_today: 0,
            consumed: 0,
            seats: vec![],
            day_orders: BTreeMap::new(),
            night_orders: BTreeMap::new(),
            night_reminded: Vec::new(),
            whispers_used: vec![],
            pending_whispers: vec![],
            whisper_archive: vec![],
            supply_changes: vec![],
            disasters: vec![],
            ballots: BTreeMap::new(),
            turn: None,
            speakers: vec![],
            runoff_candidates: None,
            speech_round: 0,
            lines: vec![],
            revealed: false,
            notice: String::new(),
            history: vec![],
            last_public_stolen: 0,
            last_hungry: vec![],
            winner: None,
            departures: Vec::new(),
            resume_phase: None,
            farewells: Vec::new(),
            theft_windows: Vec::new(),
            granary: GRANARY.to_string(),
        };
        assert_eq!(
            a.rotated(vec![1, 2, 3, 4], 12),
            a.rotated(vec![4, 3, 2, 1], 12)
        );
    }
}
