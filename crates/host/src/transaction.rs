//! 「修改 → 存盘 → 失败回滚」的唯一写法。改动前快照世界（连同尚未发出的世界日志）、人口、
//! Agent 会话、化身控制、世界设定与速度；改动被拒或存盘失败时逐项恢复，每一步都执行，
//! 恢复不了的部分一并报告。
use crate::{avatar::Control, runtime::Runtime};
use host::{population::Population, world_settings::WorldSettings, HostError};
use std::fmt;

/// 改动前的全部可变状态。
struct Checkpoint {
    world: kernel::WorldCheckpoint,
    population: Population,
    agent: agent::runtime::ControlCheckpoint,
    avatar_control: Control,
    world_settings: WorldSettings,
    speed: f64,
}

/// 事务没有提交的原因。
pub(crate) enum Abort<E> {
    /// 改动本身被拒绝。
    Rejected(E),
    /// 改动已做完，存盘失败。
    SaveFailed(HostError),
}

/// 事务失败：原因，以及没能恢复的部分（为空表示已完整回到改动前）。
pub(crate) struct Aborted<E> {
    pub cause: Abort<E>,
    pub leftovers: Vec<String>,
}

impl<E> Aborted<E> {
    /// 没能恢复的部分的说明；完整恢复时为空串。
    pub fn leftover_note(&self) -> String {
        if self.leftovers.is_empty() {
            String::new()
        } else {
            format!(
                "；回滚失败，状态可能不一致，建议重启：{}",
                self.leftovers.join("；")
            )
        }
    }
}

impl<E: fmt::Display> fmt::Display for Aborted<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.cause {
            Abort::Rejected(error) => write!(f, "{error}")?,
            Abort::SaveFailed(error) => write!(f, "保存失败，本次修改已撤回：{error}")?,
        }
        f.write_str(&self.leftover_note())
    }
}

/// 已经开始、还没存盘的改动。导演一轮里可能先看、再改：看的时候要能看到改到一半的世界，
/// 全部做完才存一次盘；失败时整轮撤回。
pub(crate) struct Change {
    before: Checkpoint,
}

impl Runtime {
    /// 执行改动并存盘；任一步失败就恢复到改动前。
    pub(crate) fn transact<T, E>(
        &mut self,
        change: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, Aborted<E>> {
        let started = self.begin_change();
        let value = match change(self) {
            Ok(value) => value,
            Err(error) => return Err(started.abort(self, Abort::Rejected(error))),
        };
        match started.commit(self) {
            Ok(()) => Ok(value),
            Err(aborted) => Err(Aborted {
                cause: aborted.cause,
                leftovers: aborted.leftovers,
            }),
        }
    }

    /// 记下改动前的状态；之后可以继续调用 `change` 想调几次调几次，最后 `commit` 或 `abort`。
    pub(crate) fn begin_change(&self) -> Change {
        Change {
            before: self.capture(),
        }
    }

    fn capture(&self) -> Checkpoint {
        Checkpoint {
            world: self.world.checkpoint(),
            population: self.population.clone(),
            agent: self.driver.runtime.control_checkpoint(),
            avatar_control: self.avatar_control.clone(),
            world_settings: self.world_settings.clone(),
            speed: self.speed,
        }
    }

    fn restore(&mut self, before: Checkpoint) -> Vec<String> {
        let mut leftovers = Vec::new();
        if let Err(error) = self.world.restore_checkpoint(before.world) {
            leftovers.push(format!("世界：{error}"));
        }
        self.population = before.population;
        self.driver.runtime.rollback_control(before.agent);
        self.avatar_control = before.avatar_control;
        self.world_settings = before.world_settings;
        self.speed = before.speed;
        leftovers
    }
}

impl Change {
    /// 存盘并确认改动；存盘失败时撤回。
    pub(crate) fn commit<E>(self, runtime: &mut Runtime) -> Result<(), Aborted<E>> {
        match runtime.checkpoint(true) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.abort(runtime, Abort::SaveFailed(error))),
        }
    }

    /// 整轮撤回，返回没能恢复的部分。
    pub(crate) fn abort<E>(self, runtime: &mut Runtime, cause: Abort<E>) -> Aborted<E> {
        let leftovers = runtime.restore(self.before);
        if !leftovers.is_empty() {
            eprintln!("[host] 回滚失败，状态可能不一致：{}", leftovers.join("；"));
        }
        Aborted { cause, leftovers }
    }

    /// 不要这次改动了：撤回但不带回错误。用于调用方已经自己攒好了逐条的失败说明。
    pub(crate) fn rollback(self, runtime: &mut Runtime) {
        let leftovers = runtime.restore(self.before);
        if !leftovers.is_empty() {
            eprintln!("[host] 回滚失败，状态可能不一致：{}", leftovers.join("；"));
        }
    }
}
