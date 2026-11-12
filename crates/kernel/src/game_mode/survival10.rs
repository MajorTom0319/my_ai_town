//! 聪明人小镇：七人、十日、21 袋公共米粮、每天最多产 4 袋。
//! 回合裁判只通过 `mode_action` 增减比赛用米，不能让通用 Props 创造比赛资源。
//! 私密信件与夜间目击只进入有关人员的 Journal/Context，不进入公共 World log。
use super::Rules;
use crate::error::{KernelError, Result};
use crate::{World, DAY};
use protocol::{
    EventInfo, EventSpeech, FactKind, GameModeAction, GameModeContext, GameModeView,
    Survival10Ballot, Survival10Context, Survival10Line, Survival10Phase as Phase, Survival10Seat,
    Survival10View,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DAWN: f64 = 480.0;
const DINNER: f64 = 1080.0;
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
/// 十日生存里每天入账的劳动时长：随时可以开始，累计够八小时即可，不要求准时到田。
const WORK_REQUIRED: f64 = 480.0;
const MAX_WORKERS: usize = 4;
/// 进别人住宅、摸到私粮袋要真实花的时间，不能选完就偷走。
const ENTER_MINUTES: f64 = 5.0;
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
    /// 工作：实际有效劳动的时长（可以带不足一分钟的零头）。聊天、离开农田或做别的事时都不增加；
    /// 入账、显示和八小时判定只看其中已满的整分钟。
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

impl Default for NightAttempt {
    fn default() -> Self {
        Self {
            order: NightOrder::default(),
            stage: NightStage::default(),
            seen: Vec::new(),
            settled: false,
            entry_ready_at: None,
            slept: false,
            on_bed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// 入室：到达目标住宅后开始计时，满 `ENTER_MINUTES` 才打开私粮袋。
    #[serde(default)]
    entry_ready_at: Option<f64>,
    /// 上床睡觉的动作自然完成过；之后离开床或做别的事只算“睡过”，不再算睡到清晨。
    #[serde(default)]
    slept: bool,
    /// 刚才是否在床上；用来发现离开床或睡眠被打断。
    #[serde(default)]
    on_bed: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ballot {
    target: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Survival10 {
    seed: u32,
    first_day: u32,
    day: u32,
    phase: Phase,
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
    ballots: BTreeMap<usize, Ballot>,
    turn: Option<usize>,
    speakers: Vec<usize>,
    #[serde(default)]
    runoff_candidates: Option<Vec<usize>>,
    lines: Vec<Survival10Line>,
    revealed: bool,
    notice: String,
    history: Vec<String>,
    last_public_stolen: u32,
    last_hungry: Vec<usize>,
    winner: Option<usize>,
    /// 粮仓所在地点。真实小镇固定是 `market_1`；测试地图没有这个地点时由测试改成自己的。
    #[serde(default = "default_granary_locale")]
    granary: String,
}

fn default_granary_locale() -> String {
    GRANARY.to_string()
}

impl Survival10 {
    pub(super) fn open(world: &mut World, seed: u32) -> Result<Self> {
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
                rice: 0,
                hungry_streak: 0,
                out: None,
                out_day: None,
                private_notes: Vec::new(),
            })
            .collect();
        let now = world.minutes();
        let today = (now / DAY).floor() as u32;
        let first_day = if crate::time_of_day(now) < DAWN {
            today
        } else {
            today + 1
        };
        let mode = Self {
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
            ballots: BTreeMap::new(),
            turn: None,
            speakers: Vec::new(),
            runoff_candidates: None,
            lines: Vec::new(),
            revealed: false,
            notice: "七位居民即将进入十日生存实验。".into(),
            history: Vec::new(),
            last_public_stolen: 0,
            last_hungry: Vec::new(),
            winner: None,
            granary: GRANARY.to_string(),
        };
        mode.sync_all_supplies(world);
        Ok(mode)
    }

    fn at(&self, day: u32, minute: f64) -> f64 {
        f64::from(self.first_day + day.saturating_sub(1)) * DAY + minute
    }
    fn living(&self) -> Vec<usize> {
        (0..self.seats.len())
            .filter(|&i| self.seats[i].out.is_none())
            .collect()
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
    /// 人此刻是否真的在农田干活。劳动中不能聊天：普通交谈把活动换成聊天占位，计时随即停止；
    /// 空等、走到半路或做别的动作都不算劳动。
    fn field(&self, world: &World, i: usize, now: f64) -> bool {
        self.field_started(world, i, now).is_some()
    }
    /// 当前这一轮下地干活从哪一刻开始。田块、动作和位置都相符才给出起点，右上角与累计共用。
    fn field_started(&self, world: &World, i: usize, now: f64) -> Option<f64> {
        let Some(a) = world.actor(&self.seats[i].id) else {
            return None;
        };
        if a.current.interaction.is_some()
            || a.current.is_travel()
            || !matches!(a.current.kind, crate::world::ActKind::Timed { .. })
            || a.current.remaining(now).is_none_or(|m| m <= EPS)
        {
            return None;
        }
        let Some(spot) = crate::physiology::body_spot(world, a) else {
            return None;
        };
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
        attempt.worked.floor().min(WORK_REQUIRED)
    }
    /// 右上角与田里看到的是同一段劳动：已入账整数分钟加上当前 `下地干活` 动作已经开始、
    /// 尚未经过下一分钟结算的那一小段。动作在片中被中断时，零头只进累计、不到一分钟不上榜。
    fn labor_progress(&self, world: &World, i: usize) -> f64 {
        let now = world.minutes();
        let attempt = self
            .day_orders
            .get(&i)
            .filter(|d| d.order == DayOrder::Work);
        let worked = attempt.map_or(0.0, Self::worked_minutes);
        // 当前这一片是先累计劳动再取视图，因此 worked 已包含正在进行的这不到一分钟；
        // 显示时补回这一个零头，下一次切片再把它并进 worked，两边不会各记一遍。
        let pending = self.field_started(world, i, now).map_or(0.0, |started| {
            let current = (now - started).clamp(0.0, crate::activity::SLICE_MINUTES);
            (attempt.map_or(0.0, |d| d.worked) - worked + current)
                .clamp(0.0, crate::activity::SLICE_MINUTES)
        });
        (worked + pending).min(WORK_REQUIRED)
    }
    fn is_sleep(spot: &crate::affordance::Spot) -> bool {
        spot.action
            .as_ref()
            .is_some_and(|a| a.completion == crate::props::Completion::Sleep)
    }
    /// 人已经躺在床上、正在完成一段睡眠。只看标称种类不够：床也是普通活动的一部分。
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
            || a.current.interaction.is_some()
            || a.current.is_travel()
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
    /// 按世界时间切片累计劳动：只认真正在农田干活的有效分钟，聊天、离开、做别的事都不记。
    /// 动作在切片中途开始、结束或被聊天打断时，只算真正与这一片重叠的干活时间。
    fn track_work(&mut self, world: &World, from: f64, to: f64) {
        if self.phase != Phase::Day || to <= from {
            return;
        }
        for &i in &self.living() {
            let Some(actor) = world.actor(&self.seats[i].id) else {
                continue;
            };
            // 聊天把动作换成聊天占位时 field_started 立即失效，聊天期间一分钟也不记；
            // 动作结束或被谈话打断时，只取当前这段从片头到结束之间的交集，不再拼接两段时间。
            let effort = self.field_started(world, i, to).map_or(0.0, |started| {
                let ended = actor.current.remaining(from).map_or(to, |left| from + left);
                let ended = ended.min(to);
                (ended - started.clamp(from, ended)).max(0.0)
            });
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
    fn settle_home_entry(&mut self, world: &mut World) {
        if self.phase != Phase::Night {
            return;
        }
        // 住宅取粮：完成那一刻看目标是否真在守家，再扣目标私粮；目击只写实际在目标住宅、醒着的人。
        let mut home_takers = Vec::new();
        for &i in &self.living() {
            let Some(n) = self.night_orders.get(&i) else {
                continue;
            };
            let NightOrder::StealHome(t) = n.order else {
                continue;
            };
            if n.settled || t >= self.seats.len() {
                continue;
            }
            let Some(home) = self.home_of(world, t) else {
                continue;
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
            if entered {
                home_takers.push((i, t, home));
            }
        }
        for (i, t, home) in home_takers {
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
                            n.on_bed = false;
                            n.stage = if n.slept {
                                NightStage::Sleeping
                            } else {
                                NightStage::Chosen
                            };
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
                    if n.entry_ready_at.is_none() {
                        let id = self.seats[i].id.clone();
                        if world
                            .actor(&id)
                            .is_some_and(|a| a.current.interaction.is_some())
                        {
                            world.interrupt_actions(&id, "开始今夜入室行动");
                        }
                        if let Some(n) = self.night_orders.get_mut(&i) {
                            n.stage = NightStage::Entering(t);
                            n.entry_ready_at = Some(world.minutes() + ENTER_MINUTES);
                        }
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
            || actor.plan.iter().any(|queued| queued.binding().is_some())
            || actor.plan.len() >= 8
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
        if world.active_chat(&id).is_some() {
            world.interrupt_actions(&id, "十日生存实验的正式行动开始");
        }
        world.notify(&id, msg.to_string());
    }
    fn announce(&mut self, world: &mut World, msg: String, wake: bool) {
        self.notice = msg.clone();
        self.history.push(msg.clone());
        if self.history.len() > 160 {
            self.history.remove(0);
        }
        world.log(EventInfo::default(), format!("【十日生存】{msg}"));
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
        self.prepare_day(world);
        self.sync_all_supplies(world);
        self.phase = Phase::Day;
        let now = world.minutes();
        self.until = self.at(self.day, DINNER).max(now + DAY_NOTICE);
        self.day_orders.clear();
        self.whispers_used.clear();
        self.night_orders.clear();
        self.night_reminded.clear();
        self.ballots.clear();
        self.revealed = false;
        self.produced_today = 0;
        self.lines.clear();
        let mut pending = std::mem::take(&mut self.pending_whispers);
        for letter in pending.drain(..) {
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
            "第{}/10天开始。公仓{}袋。{}白天每人工作或领米二选一，公共农田四个田块，每天最多四人报名，先到先得；每人实际劳动满八小时产一袋入公仓，并可额外发送一条次日送达的悄悄话。",
            self.day,self.public_rice,news
        ), true);
    }
    /// 公开可见的路线提示：只提醒还在等决定的人去地图上能找到的粮仓/农田；
    /// 已经提交的人不被改选，也不会被代替移动。提示本身不产生粮食。
    fn prepare_day(&self, world: &mut World) {
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
                    "第{}天：工作或领米二选一。农田在 {field_id}（{field_name}），粮仓在 {}（{granary_name}）。想工作：先报名取得四个名额之一，再到分配的田块下地干活，有效劳动满八小时；中断后可继续，名额不转让，进度不过夜。想领米：前往粮仓，下地领米。提交选择本身不给米。",
                    self.day, granary_point
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
                self.day_orders.get(&i).is_some_and(|d| {
                    d.order == DayOrder::Work
                        && !d.harvested
                        && Self::worked_minutes(d) + EPS >= WORK_REQUIRED
                })
            })
            .collect();
        for (rank, &i) in qualified.iter().enumerate() {
            if rank < MAX_WORKERS {
                self.day_orders.get_mut(&i).expect("已登记工作").harvested = true;
                self.public_rice += 1;
                self.produced_today += 1;
                self.sync_granary(world);
                self.remember(
                    world,
                    i,
                    format!(
                        "你今天在农田有效劳动{}小时，产出1袋米已进入公仓，不增加你的私粮。",
                        WORK_REQUIRED / 60.0
                    ),
                );
                self.announce(
                    world,
                    format!(
                        "{}今天劳动满八小时，收获1袋米，公仓新增1袋。",
                        self.label(i)
                    ),
                    false,
                );
                self.remember(world, i, "你今天劳动满八小时，1袋米已入公仓。".into());
            } else {
                self.announce(
                    world,
                    format!(
                        "{}完成了劳动，但受每日四袋产量上限限制，本日没有新增收成。",
                        self.label(i)
                    ),
                    false,
                );
                self.remember(
                    world,
                    i,
                    "旧存档今日已有超过四人劳动达标，受每日四袋上限限制未新增收成；次日起按四人报名限制执行。".into(),
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
                Some(d)
                    if d.order == DayOrder::Work
                        && Self::worked_minutes(d) + EPS < WORK_REQUIRED =>
                {
                    self.remember(
                        world,
                        i,
                        format!(
                            "你今天只实际劳动了{:.1}小时，没到八小时，没有收获。",
                            Self::worked_minutes(d) / 60.0
                        ),
                    )
                }
                _ => {}
            }
        }
        let mut hungry = Vec::new();
        for i in living {
            if self.seats[i].rice > 0 {
                self.seats[i].rice -= 1;
                self.sync_pantry(world, i);
                self.seats[i].hungry_streak = 0;
                self.consumed += 1;
                self.remember(
                    world,
                    i,
                    format!("你晚餐消耗1袋米，剩余{}袋。", self.seats[i].rice),
                );
            } else {
                self.seats[i].hungry_streak += 1;
                hungry.push(i);
                self.remember(world, i, "你晚餐没有米可吃。".into());
            }
        }
        self.last_hungry = hungry.clone();
        for i in hungry {
            if self.seats[i].hungry_streak >= 2 {
                self.eliminate(world, i, "连续两晚未进食");
            }
        }
        if self.living().len() <= 1 {
            self.finish_single(world);
        } else if self.day % 2 == 0 {
            // 第十天也必须先完成第五次篝火会投票；投票结束后才进入陪审团。
            self.start_speech(world);
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
        world.depart(&self.seats[i].id, "被淘汰，离开生存小镇，转为终局陪审团");
        self.announce(
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
                "你在第{}天出局，原因：{}。第10天将参与陪审团。",
                self.day, reason
            ),
        );
    }
    fn start_speech(&mut self, world: &mut World) {
        self.phase = Phase::Speech;
        self.speakers = self.living();
        self.runoff_candidates = None;
        self.turn = None;
        self.lines.clear();
        self.next_speaker(world);
    }
    fn start_runoff_speech(&mut self, world: &mut World, candidates: Vec<usize>) {
        self.phase = Phase::Speech;
        self.speakers = candidates.clone();
        self.runoff_candidates = Some(candidates);
        self.turn = None;
        self.lines.clear();
        self.next_speaker(world);
    }
    fn next_speaker(&mut self, world: &mut World) {
        if self.speakers.is_empty() {
            self.turn = None;
            self.phase = Phase::Vote;
            self.ballots.clear();
            self.revealed = false;
            self.until = world.minutes() + VOTE_LIMIT;
            let message = if self.runoff_candidates.is_some() {
                "平票候选人追加发言结束，进入重投；仍不能弃票，统一揭票。".to_string()
            } else {
                format!("第{}天篝火会开始记名投票，不能弃票，统一揭票。", self.day)
            };
            self.announce(world, message, true);
            return;
        }
        let i = self.speakers.remove(0);
        self.turn = Some(i);
        self.until = world.minutes() + SPEECH_LIMIT;
        self.announce(world, format!("篝火会轮到{}发言。", self.label(i)), false);
        self.call(world, i, "请用 mode_action action=speak 提交本轮会议发言。");
    }
    fn resolve_vote(&mut self, world: &mut World) {
        // 超时缺票：按固定种子选择一位其他在场人，并标为系统代投。
        let living = self.living();
        for &i in &living {
            if !self.ballots.contains_key(&i) {
                let others = self.rotated(
                    living.iter().copied().filter(|&j| j != i).collect(),
                    self.day * 37 + i as u32,
                );
                if let Some(&target) = others.first() {
                    self.ballots.insert(i, Ballot { target });
                    self.announce(
                        world,
                        format!("{}未按时投票，系统按既定规则代投。", self.label(i)),
                        false,
                    );
                }
            }
        }
        self.revealed = true;
        let mut counts = BTreeMap::<usize, usize>::new();
        for (v, target) in self
            .ballots
            .iter()
            .map(|(&v, b)| (v, b.target))
            .collect::<Vec<_>>()
        {
            *counts.entry(target).or_default() += 1;
            self.announce(
                world,
                format!("{}记名投票给{}。", self.label(v), self.label(target)),
                false,
            );
        }
        if let Some(max_count) = counts.values().copied().max() {
            let tied: Vec<usize> = counts
                .into_iter()
                .filter(|(_, n)| *n == max_count)
                .map(|(t, _)| t)
                .collect();
            if tied.len() > 1 && self.runoff_candidates.is_none() {
                self.start_runoff_speech(world, tied);
                return;
            }
            // 重投再次平票时，用固定种子作最终可复现的裁决。
            let target = self
                .rotated(tied, self.day * 13)
                .into_iter()
                .next()
                .expect("存在候选人");
            self.runoff_candidates = None;
            self.eliminate(world, target, "篝火会记名投票");
        }
        if self.living().len() <= 1 {
            self.finish_single(world);
        } else if self.day == 10 && self.living().len() == 2 {
            // 第十天的投票已经完成，且只剩两位决赛者，才交给出局者组成陪审团。
            self.start_jury(world);
        } else {
            self.start_night(world);
        }
    }
    fn start_jury(&mut self, world: &mut World) {
        self.phase = Phase::Jury;
        self.turn = None;
        self.runoff_candidates = None;
        self.ballots.clear();
        self.revealed = false;
        self.until = self.at(self.day, DINNER).max(world.minutes() + JURY_LIMIT);
        self.announce(
            world,
            "出局者组成陪审团，为最后两人投票选出冠军；不能弃票。".into(),
            true,
        );
    }
    fn resolve_jury(&mut self, world: &mut World) {
        let jury = self.jurors();
        for &i in &jury {
            if !self.ballots.contains_key(&i) {
                self.remember(world, i, "你没有按时提交陪审团投票，本票作废。".into());
            }
        }
        let mut counts = BTreeMap::<usize, usize>::new();
        for b in self.ballots.values() {
            *counts.entry(b.target).or_default() += 1;
        }
        let winner = counts
            .values()
            .copied()
            .max()
            .map(|max| {
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
            })
            .or_else(|| self.living().into_iter().next());
        self.winner = winner;
        self.phase = Phase::Over;
        if let Some(w) = winner {
            self.announce(world, format!("陪审团选出{}为冠军。", self.label(w)), true);
        } else {
            self.announce(world, "陪审团无有效选票，本局无人获胜。".into(), true);
        }
    }
    fn finish_single(&mut self, world: &mut World) {
        let winner = self.living().into_iter().next();
        self.winner = winner;
        self.phase = Phase::Over;
        if let Some(w) = winner {
            self.announce(world, format!("{}成为最后的生存者。", self.label(w)), true);
        } else {
            self.announce(world, "没有参赛者存活，本局无人获胜。".into(), true);
        }
    }
    fn start_night(&mut self, world: &mut World) {
        self.phase = Phase::Night;
        self.night_orders.clear();
        self.night_reminded.clear();
        self.last_public_stolen = 0;
        self.until = self.at(self.day + 1, DAWN);
        self.announce(
            world,
            "夜间行动：睡觉、偷公仓、偷别人家、守家，每人仅一次。".into(),
            true,
        );
        self.check_night(world);
    }
    /// 夜间截止前还没提交的人只收到一次点名提醒；不替他们选，也不补发粮食。
    fn check_night(&mut self, world: &mut World) {
        let now = world.minutes();
        let left = self.until - now;
        if left > 60.0 || left < 0.0 {
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
        // 取粮、偷窃、守家和目击都在动作完成那一刻结算；天亮这里只记录没有完成的登记。
        for &i in &self.living() {
            let Some(n) = self.night_orders.get(&i) else {
                self.remember(world, i, "你今夜没有提交夜间行动，因此什么也没做。".into());
                continue;
            };
            match n.order {
                NightOrder::Sleep if !n.slept && !n.on_bed => {
                    self.remember(
                        world,
                        i,
                        "你登记了睡觉，但没有在自己住宅的床上完成一场睡眠；本夜没有有效睡觉。"
                            .into(),
                    );
                }
                NightOrder::Guard if !self.keeping_guard(world, i) => {
                    self.remember(
                        world,
                        i,
                        "你登记了守家，但昨夜没有一直保持在自己住宅醒着；守家保护失效。".into(),
                    );
                }
                NightOrder::StealPublic if !n.settled => {
                    self.remember(
                        world,
                        i,
                        "你选择了偷公仓，但没有在粮仓完成取粮动作，什么也没拿到。".into(),
                    );
                }
                NightOrder::StealHome(_) if !n.settled => {
                    self.remember(
                        world,
                        i,
                        "你选择了入室偷粮，但没有在目标私粮袋旁完成取粮动作，什么也没拿到。".into(),
                    );
                }
                _ => {}
            }
        }
        if self.day >= 10 || self.living().len() <= 1 {
            self.finish_single(world);
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
            Phase::Speech => self.next_speaker(world),
            Phase::Vote => self.resolve_vote(world),
            Phase::Night => self.resolve_night(world),
            Phase::Jury => self.resolve_jury(world),
            Phase::Over => return false,
        }
        true
    }
    fn actions(&self, i: usize) -> Vec<&'static str> {
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
                vec!["sleep", "steal_public", "steal_home", "guard_home"]
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
    fn boundary(&self, world: &World) -> f64 {
        if self.phase == Phase::Over {
            return f64::INFINITY;
        }
        // 白天劳动和夜间看守、睡觉、收粮都按世界时间推进；每分钟看一次真实状态，回合截止仍优先。
        if matches!(self.phase, Phase::Day | Phase::Night) {
            (self.until - world.minutes()).clamp(0.0, 1.0)
        } else {
            (self.until - world.minutes()).max(0.0)
        }
    }
    fn tempo(&self, _world: &World) -> f64 {
        if matches!(self.phase, Phase::Speech | Phase::Vote | Phase::Jury) {
            1.0
        } else {
            3.0
        }
    }
    fn advance(&mut self, world: &mut World) {
        self.sync_all_supplies(world);
        // boundary 每分钟切一次；用真实上一片的末尾到这一片的末尾计，不把模型等待的真实时间当劳动。
        let now = world.minutes();
        let from = (now - 1.0).max(self.until - DAY);
        if self.phase == Phase::Day {
            self.track_work(world, from, now);
        } else if self.phase == Phase::Night {
            self.track_night(world);
        }
        self.settle_home_entry(world);
        if self.phase == Phase::Night {
            self.check_night(world);
        }
        for _ in 0..32 {
            if !self.maybe_advance(world) {
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
            until: self.until.is_finite().then_some(self.until),
            turn: self.turn.map(|i| i as u32 + 1),
            lines: self.lines.clone(),
            ballots: self
                .ballots
                .iter()
                .map(|(&i, b)| Survival10Ballot {
                    voter: i as u32 + 1,
                    target: self.revealed.then_some(b.target as u32 + 1),
                })
                .collect(),
            revealed: self.revealed,
            history: self.history.iter().rev().take(16).cloned().collect(),
            winner: self.winner.map(|i| i as u32 + 1),
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
            hungry_streak:s.hungry_streak,
            seats:others,known:s.private_notes.iter().rev().take(24).cloned().collect(),
            history:self.history.iter().rev().take(12).cloned().collect(),
            now:self.notice.clone(),actions,
        }))
    }
    fn tool(&self, _world: &World, actor: &str) -> Option<String> {
        let i = self.seat_of(actor)?;
        if self.actions(i).is_empty() {
            return None;
        }
        Some("十日生存实验：仅在 survival10.actions 允许时使用 mode_action；提交只是选择，粮食只按真实完成结算。\
        action：work 白天报名劳动，每天最多四人，先到先得；报名成功分配田块，到该田块执行下地干活，当天有效劳动累计满八小时才收获；中断后只需补足剩余时间，名额不转让、进度不过夜；\
        take_rice 白天登记领米，到公共粮仓对粮仓执行十分钟取粮，完成时立即将一袋米放进自己的私粮，不必等晚餐或回家；中断后可重试，当天只能成功领取一袋；whisper 给指定 target 发保密信(text，次日送达)；\
        sleep 睡觉，必须回到自己家并睡到床上；steal_public 偷公仓，必须到粮仓取米；\
        steal_home 夜间进入 target 的家取私粮；guard_home 守家，必须回自己住宅并保持清醒；\
        speak 篝火发言(text)；vote 记名投票(target,不可弃权)；jury_vote 陪审团投票(target)。\
        target 填座号，reason 可写动机。只有实际去田里劳动、走到粮仓、进了对应住宅，才会产生结果。\
        每人每晚要吃1袋个人存粮，连饿两晚出局；公开粮仓初始21袋、每日生产最多4袋、偶数天会议、第十天陪审团决定冠军。".into())
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
        if Self::is_granary_collect(world, spot) {
            self.check_collection(world, actor, spot)?;
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
        if !Self::is_granary_collect(world, spot) {
            return Ok(None);
        }
        let seat = self.check_collection(world, actor, spot)?;
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
        let witnesses: Vec<_> = if self.phase == Phase::Night {
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
        self.public_rice -= 1;
        self.seats[seat].rice += 1;
        if self.phase == Phase::Day {
            let attempt = self.day_orders.get_mut(&seat).expect("已登记领米");
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
            self.last_public_stolen += 1;
            let attempt = self.night_orders.get_mut(&seat).expect("已登记偷公仓");
            attempt.stage = NightStage::Taking;
            attempt.settled = true;
        }
        self.sync_pantry(world, seat);
        self.sync_granary(world);
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
        Ok(Some(vec![format!(
            "已取回1袋米，个人私粮现有{}袋，公仓剩余{}袋。",
            self.seats[seat].rice, self.public_rice
        )]))
    }
    fn act(&mut self, world: &mut World, actor: &str, a: &GameModeAction) -> Result<String> {
        let i = self
            .seat_of(actor)
            .ok_or_else(|| KernelError::action("你不在十日实验参赛名单中"))?;
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
        let answer = match a.action.as_str() {
            "work" => {
                let farm = self.register_work(world, i, None)?;
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
                format!("已登记今日工作，报名 {}/4，分配田块 {farm}。请到公共农田对该田块执行下地干活；累计有效劳动满八小时才收获，聊天、离开或做别的事都会停表，回来只需补足剩余时间。名额不转让、进度不过夜，现在还没增加公仓。", self.registered_today())
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
                self.pending_whispers.push(Whisper {
                    from: i,
                    to,
                    sent_day: self.day,
                    content: text.into(),
                });
                self.whispers_used.push(i);
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
                self.guide(
                    world,
                    i,
                    crate::intent::Intent::Goto {
                        locale: self.granary_point(world),
                        station: None,
                    },
                );
                "已登记偷公仓。请走到公共粮仓并对粮仓执行「取粮」，动作完成后才到账；没完成动作不会补发。".into()
            }
            "guard_home" => {
                self.night_orders.insert(
                    i,
                    NightAttempt {
                        order: NightOrder::Guard,
                        stage: NightStage::Chosen,
                        ..NightAttempt::default()
                    },
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
                    "已登记夜间入室。请找到{}的住宅，对其私粮袋执行「取粮」并完成动作；只在家门口不算。",
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
                    },
                );
                world.log(info, format!("{}在篝火会上说：{}", self.label(i), text));
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
                self.ballots.insert(i, Ballot { target });
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
            ballots: BTreeMap::new(),
            turn: None,
            speakers: vec![],
            runoff_candidates: None,
            lines: vec![],
            revealed: false,
            notice: String::new(),
            history: vec![],
            last_public_stolen: 0,
            last_hungry: vec![],
            winner: None,
            granary: GRANARY.to_string(),
        };
        assert_eq!(
            a.rotated(vec![1, 2, 3, 4], 12),
            a.rotated(vec![4, 3, 2, 1], 12)
        );
    }
}
