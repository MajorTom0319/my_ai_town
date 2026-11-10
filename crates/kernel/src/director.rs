//! 导演是世界之外的玩家代理：它读得到世界与村民的记录，但改世界只能走这里和已有的道具、作品命令。
//! 这里只放两件事：通知与预约。
//!
//! 预约进 `happenings.pending`（与世界快照一起存），由 simulation.rs 的 `fire_happenings` 到点触发；
//! 触发复用道具动作的定时事件（只通知、不放道具）。导演只改变村民能看见什么，不碰 Actor 的会话、
//! 目标与记忆：村民怎么反应由他们自己决定。
use crate::{
    actor::Actor,
    error::{KernelError, Result},
    happenings::What,
    World,
};

/// 预约最近的几分钟后，最远几天：太近来不及反应，太远的堆在存档里没意义。
const SOONEST: f64 = 1.0;
const FURTHEST: f64 = 7.0 * crate::DAY;
/// 一条通知的字数上限。
const TEXT_MAX_CHARS: usize = 800;
/// 同时待触发的导演事件上限。
const PENDING_MAX: usize = 200;

impl World {
    /// 现在告诉在场（给定了地点时只告诉那里的）村民一件事。
    pub fn director_announce(&mut self, text: &str, locale: Option<&str>) -> Result<()> {
        let text = valid_text(text)?;
        let at = self.director_locale(locale)?;
        // 没给地点时只告诉入口那一处的人，和预约事件被听见的范围一致；日志仍写进世界。
        self.notify_locale(&text, Some(&at));
        self.log(protocol::EventInfo::director(), format!("导演通知：{text}"));
        Ok(())
    }

    /// 预约一件事：`after_minutes` 分钟后触发。返回触发的时刻。
    /// 每次预约都是新的一件，不按内容去重：玩家要重复说，就重复发生。
    pub fn director_schedule(
        &mut self,
        text: &str,
        after_minutes: f64,
        locale: Option<&str>,
    ) -> Result<f64> {
        let text = valid_text(text)?;
        ensure!(
            after_minutes.is_finite() && (SOONEST..=FURTHEST).contains(&after_minutes),
            KernelError::action("预约的时刻太近或太远")
        );
        let at_locale = self.director_locale(locale)?;
        ensure!(
            self.happenings.pending.len() < PENDING_MAX,
            KernelError::action("待触发的事太多，先等几件发生")
        );
        let at = self.minutes() + after_minutes;
        let key = format!("director-{}", self.next_director_key());
        self.happenings.push(
            at,
            What::Event {
                key,
                text,
                locale: at_locale,
                place: None,
            },
        );
        Ok(at)
    }

    /// 导演安排的待触发事件：触发时刻、地点、正文，按时刻先后。
    pub fn director_pending(&self) -> Vec<(f64, String, String)> {
        self.happenings
            .pending
            .iter()
            .filter_map(|p| match &p.what {
                What::Event {
                    key,
                    text,
                    locale,
                    place: None,
                } if key.starts_with("director-") => Some((p.at, locale.clone(), text.clone())),
                _ => None,
            })
            .collect()
    }

    /// 给定了地点时核对它存在，并原样返回；没给时取第一个地点（没有时取第一个可驻留地点）。
    fn director_locale(&self, locale: Option<&str>) -> Result<String> {
        if let Some(locale) = locale {
            ensure!(
                self.topo.locales.contains_key(locale),
                KernelError::action(format!("没有这个地点：{locale}"))
            );
            return Ok(locale.to_string());
        }
        self.topo
            .places
            .first()
            .map(|p| p.locale.clone())
            .or_else(|| {
                self.topo
                    .locales
                    .values()
                    .find(|l| l.kind == crate::topology::LocaleKind::Dwell)
                    .map(|l| l.id.clone())
            })
            .ok_or_else(|| KernelError::action("这张地图没有可通知的地点"))
    }

    /// 导演事件的编号：时刻加已有的条数，保证同一时刻的多条预约也不撞 key。
    fn next_director_key(&self) -> String {
        let count = self
            .happenings
            .pending
            .iter()
            .filter(|p| matches!(&p.what, What::Event { key, .. } if key.starts_with("director-")))
            .count();
        format!("{}-{count}", self.minutes().to_bits())
    }

    /// 告诉在场村民；给了地点就只告诉那一处的人。死者与化身不算听众。
    pub(crate) fn notify_locale(&mut self, text: &str, locale: Option<&str>) {
        let ids: Vec<_> = self
            .actors
            .values()
            .filter(|a| hears(a, locale))
            .map(|a| a.id.clone())
            .collect();
        for id in ids {
            self.notify(&id, text.to_string());
        }
    }
}

fn hears(a: &Actor, locale: Option<&str>) -> bool {
    a.present()
        && !a.is_avatar()
        && !a.needs.body.dead
        && locale.is_none_or(|place| a.locus.locale == place)
}

fn valid_text(text: &str) -> Result<String> {
    let text = text.trim();
    ensure!(
        !text.is_empty() && text.chars().count() <= TEXT_MAX_CHARS,
        KernelError::action("通知须为 1–800 字")
    );
    Ok(text.to_string())
}
