//! 世界执行器。只做两件事：按时间推进活动、把事实分发为通知与事件。
//! 不产生任何决策。各子系统持有自己的状态和规则，World 只做编排：
//! 命令入口取出参与者、调子系统方法、记录事实与事件。

use crate::error::Result;
use crate::{
    actor::Actors, affordance::Affordances, clock_string, content::ContentState,
    interactions::Interactions, inventory::Inventory, navigation::space::Placer, props::Props,
    topology::Topology,
};
use protocol::{
    EventInfo, ManuscriptKind, ManuscriptTone, ManuscriptVisibility, NeedRules, PhysiologySwitches,
};

pub use crate::activity::{ActKind, Activity, TravelStage};
pub use crate::actor::Actor;

// ---------- 事件 ----------

#[derive(Debug, Clone)]
pub enum Event {
    /// 观察日志（仪器用）：正文给人读，info 是种类、人物与提示卡细节
    Log {
        minutes: f64,
        text: String,
        info: EventInfo,
    },
}

// ---------- 世界 ----------

pub struct World {
    pub(crate) weather: Option<crate::weather::WeatherState>,
    /// 开着的玩法（game_mode/），随快照保存；没开时为 None。
    pub(crate) game_mode: Option<crate::game_mode::GameMode>,
    /// 加载时注入的二维自由行走几何，不写入世界快照。
    pub walk_map: Option<std::sync::Arc<crate::avatar::WalkMap>>,
    pub topo: Topology,
    /// 可供性索引（由道具表派生，affordance.rs）
    pub affordances: Affordances,
    /// 道具实例的运行期状态：放置、墓碑、补给（props/state.rs）
    pub(crate) props: Props,
    pub(crate) contents: ContentState,
    pub(crate) inventory: Inventory,
    pub(crate) interactions: Interactions,
    pub(crate) actors: Actors,
    pub(crate) minutes: f64,
    pub(crate) events: Vec<Event>,
    /// 需求参数的合法范围；由 host 从 config/needs_rules.json 读入，存档恢复与改参数时校验。
    pub(crate) need_rules: NeedRules,
    /// 世界级生理开关（physiology/commands.rs），随快照保存。
    pub(crate) physiology: PhysiologySwitches,
    /// 化身神力开关（miracle.rs），随快照保存。
    pub(crate) miracles: bool,
    /// 陌生环境：村民只知道到过的地方（exploration.rs），随快照保存。
    pub(crate) strangers: bool,
    /// 藏着的东西（道具编号）：找到过的人才看得见（exploration.rs），随快照保存。
    pub(crate) concealed: std::collections::BTreeSet<String>,
    /// 空间放置器：由 host 注入，内核放置道具时向它要位置与通路。
    pub(crate) placer: Option<Box<dyn Placer>>,
    /// 道具动作引发的后续：定时事件、随机流（happenings.rs），随快照保存。
    pub(crate) happenings: crate::happenings::Happenings,
    /// 同行的足迹与计时（interactions/walk.rs），不存档。
    pub(crate) walk_trails: crate::interactions::WalkTrails,
    /// 每人上次所在的已知地点（sightings.rs），不存档；为空表示还没开始记。
    pub(crate) places_seen: Option<std::collections::BTreeMap<String, Option<String>>>,
    /// 自上次取走后新发生的剧本事实（manuscript.rs），不存档；host 每拍取走写进剧本流水。
    pub(crate) manuscript_outbox: Vec<ManuscriptFact>,
    /// 供剧本去重的世界日志编号，不存档。它不让同一分钟、同一场合的两句真话互相吞掉。
    manuscript_logseq: u64,
}

/// 一条已确认的剧本事实：内核在台词、心声和动作结果落进各人经历的位置报出，host 原样收进剧本。
/// 它只是给玩家看的旁白，不改变任何人的记忆，也不进给模型的观察。
#[derive(Debug, Clone, PartialEq)]
pub struct ManuscriptFact {
    pub kind: protocol::ManuscriptKind,
    pub visibility: protocol::ManuscriptVisibility,
    pub tone: protocol::ManuscriptTone,
    pub actor: Option<String>,
    pub name: Option<String>,
    pub locale: Option<String>,
    pub text: String,
    pub about: Vec<String>,
    pub source: String,
    /// 发生时的模拟分钟；host 写进流水，排序以它为准
    pub at: f64,
}

impl World {
    /// t=0 是第 1 日 00:00；默认从 07:00 开出。
    pub fn new(topo: Topology, start_minutes: f64, need_rules: NeedRules) -> Self {
        World {
            weather: None,
            game_mode: None,
            walk_map: None,
            affordances: Affordances::build(&topo),
            contents: ContentState::from_topology(&topo),
            props: Props::from_topology(&topo),
            topo,
            inventory: Default::default(),
            interactions: Default::default(),
            minutes: start_minutes,
            actors: Actors::new(),
            events: Vec::new(),
            need_rules,
            physiology: Default::default(),
            miracles: false,
            strangers: false,
            concealed: Default::default(),
            placer: None,
            happenings: Default::default(),
            walk_trails: Default::default(),
            places_seen: None,
            manuscript_outbox: Vec::new(),
            manuscript_logseq: 0,
        }
    }

    /// 有空间地图的世界必须装一个放置器，否则不能放置道具。
    pub fn set_placer(&mut self, placer: Box<dyn Placer>) {
        self.placer = Some(placer);
    }

    pub fn minutes(&self) -> f64 {
        self.minutes
    }
    pub fn clock(&self) -> String {
        clock_string(self.minutes)
    }
    pub fn actor(&self, id: &str) -> Option<&Actor> {
        self.actors.get(id)
    }
    pub fn actor_ids(&self) -> Vec<String> {
        self.actors.keys().cloned().collect()
    }
    /// 有人倒下了但还活着：这时世界不自动快进，给旁人和玩家留出救人的时间。
    pub fn anyone_collapsed(&self) -> bool {
        self.actors
            .values()
            .any(|a| a.present() && a.needs.incapacitated() && !a.needs.body.dead)
    }

    /// 按编号取可变人物。编号来自 actors 表或已校验的命令，不存在即内核不变式被破坏。
    /// 测试用：换一个人的所在地点（真实地图里靠走路到达）。
    #[doc(hidden)]
    pub fn actor_for_test(&mut self, id: &str) -> &mut Actor {
        self.actor_mut(id)
    }

    pub(crate) fn actor_mut(&mut self, id: &str) -> &mut Actor {
        self.actors
            .get_mut(id)
            .unwrap_or_else(|| panic!("村民 {id} 不存在"))
    }

    /// 显著通知：进收件箱，也作为世界事件留给 Agent 按游标读取。
    pub(crate) fn notify(&mut self, actor_id: &str, text: String) {
        let now = self.minutes;
        self.actor_mut(actor_id).notify(now, text);
    }

    /// 直接安置在家（出生处所即家）。从入口加入走 population.rs 的 `admit`。
    pub fn spawn(&mut self, id: &str, name: &str, locale: &str) -> Result<()> {
        self.admit(crate::population::Admission {
            id,
            name,
            home: Some(locale),
            entry: None,
        })
    }

    /// 报一条剧本事实。去重由 host 按 source 做；空正文不报。
    pub(crate) fn manuscript(&mut self, fact: ManuscriptFact) {
        if !fact.text.trim().is_empty() {
            self.manuscript_outbox.push(ManuscriptFact {
                at: self.minutes,
                ..fact
            });
        }
    }

    /// 报一件只有部分人或没有居民知道的事：剧本给玩家看，但不借私密日志改任何人的记忆。
    /// `about` 是与这件事直接有关的人；公开事件仍走世界日志，好让在场的人照常看见。
    #[allow(clippy::too_many_arguments)]
    pub fn manuscript_fact(
        &mut self,
        kind: protocol::ManuscriptKind,
        visibility: protocol::ManuscriptVisibility,
        actor: Option<&str>,
        locale: Option<&str>,
        text: impl Into<String>,
        about: Vec<String>,
        source: impl Into<String>,
    ) {
        let name = actor.and_then(|id| self.actors.get(id).map(|a| a.name.clone()));
        self.manuscript(ManuscriptFact {
            kind,
            visibility,
            tone: protocol::ManuscriptTone::Result,
            actor: actor.map(str::to_owned),
            name,
            locale: locale.map(str::to_owned),
            text: text.into(),
            about,
            source: source.into(),
            at: self.minutes,
        });
    }

    /// 取走这一拍新报的剧本事实。动作回执写在本人身上，读档时跳过，不补记。
    pub fn take_manuscript(&mut self) -> Vec<ManuscriptFact> {
        for actor in self.actors.values_mut() {
            if let Some(events) = actor.manuscript_outbox.take() {
                self.manuscript_outbox.extend(events);
            }
        }
        self.manuscript_outbox
            .append(&mut self.interactions.manuscript);
        let mut facts = std::mem::take(&mut self.manuscript_outbox);
        // 台词、心声和动作可能在同一拍里由不同出口报出；剧本按发生时刻排，贡献者顺序不决定故事顺序。
        facts.sort_by(|a, b| {
            a.at.total_cmp(&b.at)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.text.cmp(&b.text))
        });
        facts
    }

    pub(crate) fn log(&mut self, info: EventInfo, text: String) {
        self.manuscript_logseq += 1;
        if let Some(speech) = &info.speech {
            // 玩法在公开场合或频道里说的一段话：正文是完整的一行，剧本里收原话。
            let fact = ManuscriptFact {
                kind: ManuscriptKind::Speech,
                visibility: if speech.public {
                    ManuscriptVisibility::Public
                } else {
                    ManuscriptVisibility::Private
                },
                tone: ManuscriptTone::Statement,
                actor: info.actor.as_ref().map(|a| a.id.clone()),
                name: info.actor.as_ref().map(|a| a.name.clone()),
                locale: info
                    .actor
                    .as_ref()
                    .and_then(|a| self.actors.get(&a.id))
                    .map(|a| a.locus.locale.clone()),
                text: speech.text.clone(),
                // 玩法只写明了自己的听众范围；秘密频道不谎称对众人公开，具体听众由该玩法的经历记录佐证。
                about: info.actor.iter().map(|a| a.id.clone()).collect(),
                source: format!("speech:{}-{}", self.manuscript_logseq, speech.occasion),
                at: self.minutes,
            };
            self.manuscript(fact);
        }
        self.events.push(Event::Log {
            minutes: self.minutes,
            text,
            info,
        });
    }
}
