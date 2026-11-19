use crate::{runtime::Runtime, transaction::Abort};
use protocol::{
    DirectorEffect, DirectorEntry, DirectorErrorKind, DirectorPending, DirectorScheduled,
};
use serde_json::Value;

const PENDING_MAX: usize = 200;
const FURTHEST: f64 = 7.0 * kernel::DAY;

impl Runtime {
    pub(super) fn director_schedule_action(
        &mut self,
        args: &Value,
    ) -> Result<DirectorEffect, DirectorEffect> {
        let reject = |text: &str| DirectorEffect {
            tool: "schedule".into(),
            ok: false,
            text: text.into(),
            error_kind: Some(DirectorErrorKind::InvalidArguments),
        };
        let text = args["text"]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| reject("预约需要事项正文"))?;
        if text.chars().count() > 800 {
            return Err(reject("预约正文不能超过 800 字"));
        }
        if args
            .get("after_minutes")
            .is_some_and(|value| !value.is_null())
            && args.get("at_minutes").is_some_and(|value| !value.is_null())
        {
            return Err(reject(
                "after_minutes 与 at_minutes 只能选一个，不能同时指定",
            ));
        }
        let delay = args["after_minutes"]
            .as_f64()
            .or_else(|| {
                args["at_minutes"]
                    .as_f64()
                    .map(|at| at - self.world.minutes())
            })
            .ok_or_else(|| reject("预约需要 after_minutes 或 at_minutes"))?;
        if !delay.is_finite() || !(1.0..=FURTHEST).contains(&delay) {
            return Err(reject(
                "预约须在 1 到 10080 个模拟分钟后；现在就做的事请直接调用操作工具",
            ));
        }
        if self.director_pending().len() >= PENDING_MAX {
            return Err(reject("待触发的事太多，先等几件发生"));
        }
        let at = self.world.minutes() + delay;
        let tool = match args.get("action") {
            None | Some(Value::Null) => {
                if args["arguments"]
                    .as_object()
                    .is_some_and(|arguments| !arguments.is_empty())
                {
                    return Err(reject(
                        "指定了操作参数但缺少 action；不会把操作当成通知预约",
                    ));
                }
                let locale = args["locale"].as_str();
                self.world
                    .director_schedule(text, delay, locale)
                    .map_err(|error| reject(&error.to_string()))?;
                return Ok(DirectorEffect {
                    tool: "schedule".into(),
                    ok: true,
                    text: format!(
                        "已预约 {} 的通知：{text}（随模拟时间触发）",
                        kernel::clock_string(at)
                    ),
                    error_kind: None,
                });
            }
            Some(Value::String(tool)) if allowed(tool) => tool.clone(),
            _ => {
                return Err(reject(
                    "预约只能执行通知、天气、投放或移除道具、张贴公告、调整库存，不能预约查看或再次预约",
                ))
            }
        };
        let arguments = args
            .get("arguments")
            .filter(|arguments| arguments.is_object())
            .ok_or_else(|| reject("预约操作需要 arguments 对象，字段与对应操作工具相同"))?;
        validate(&tool, arguments).map_err(reject)?;
        self.director.state.scheduled.push(DirectorScheduled {
            at,
            text: text.into(),
            tool,
            arguments: arguments.clone(),
        });
        self.director
            .state
            .scheduled
            .sort_by(|left, right| left.at.total_cmp(&right.at));
        Ok(DirectorEffect {
            tool: "schedule".into(),
            ok: true,
            text: format!(
                "已预约 {} 执行：{text}（随模拟时间触发，到点重新校验）",
                kernel::clock_string(at)
            ),
            error_kind: None,
        })
    }

    pub(crate) fn director_pending(&self) -> Vec<DirectorPending> {
        let mut pending: Vec<_> = self
            .world
            .director_pending()
            .into_iter()
            .map(|(at, locale, text)| DirectorPending {
                at,
                clock: kernel::clock_string(at),
                locale,
                text,
            })
            .collect();
        pending.extend(self.director.state.scheduled.iter().map(|task| {
            DirectorPending {
                at: task.at,
                clock: kernel::clock_string(task.at),
                locale: task.arguments["locale"]
                    .as_str()
                    .unwrap_or(if task.tool == "announce" {
                        "入口地点"
                    } else {
                        "全镇"
                    })
                    .into(),
                text: task.text.clone(),
            }
        }));
        pending.sort_by(|left, right| left.at.total_cmp(&right.at));
        pending
    }

    pub(crate) fn director_step_minutes(&self, step: f64) -> f64 {
        self.director
            .state
            .scheduled
            .iter()
            .filter(|task| task.at.is_finite())
            .map(|task| (task.at - self.world.minutes()).max(0.0))
            .fold(step, f64::min)
    }

    pub(crate) fn director_run_scheduled(&mut self) {
        self.director.bind_world(&self.population.world_id);
        while let Some(index) = self
            .director
            .state
            .scheduled
            .iter()
            .position(|task| !task.at.is_finite() || task.at <= self.world.minutes())
        {
            let task = self.director.state.scheduled[index].clone();
            // 事务里执行可能推进模拟时间；日志属于执行的那一刻，先把它从世界队列拿出来单独交出去。
            let before = self.world.take_events();
            let player = format!("预约执行 · {}：{}", self.world.clock(), task.text);
            let result = self.transact(|runtime| {
                runtime.director.state.scheduled.remove(index);
                let effect = if allowed(&task.tool) {
                    runtime
                        .director_change(&task.tool, &task.arguments)
                        .map_err(|mut effect| {
                            if effect.tool.is_empty() {
                                effect.tool = task.tool.clone();
                            }
                            effect
                        })?
                } else {
                    return Err(DirectorEffect {
                        tool: task.tool.clone(),
                        ok: false,
                        text: "预约操作不在允许范围内".into(),
                        error_kind: Some(DirectorErrorKind::Rejected),
                    });
                };
                let entry = DirectorEntry {
                    world_id: runtime.population.world_id.clone(),
                    player: player.clone(),
                    reply: format!("预约已执行：{}", effect.text),
                    effects: vec![effect],
                    error: None,
                };
                runtime.director.remember(entry.clone());
                Ok(entry)
            });
            // 预约通知在事务以外另行发出；事务撤回时，这个动作产生的日志不入剧本。
            let mut events = vec![before];
            let entry = match result {
                Ok(entry) => {
                    self.director_props_changed |= matches!(
                        task.tool.as_str(),
                        "place_prop"
                            | "remove_prop"
                            | "post_bulletin"
                            | "adjust_supply"
                            | "trigger_game_event"
                    );
                    events.push(self.world.take_events());
                    entry
                }
                Err(aborted) => match aborted.cause {
                    Abort::SaveFailed(_) => break,
                    Abort::Rejected(effect) => {
                        let entry = DirectorEntry {
                            world_id: self.population.world_id.clone(),
                            player,
                            reply: format!("预约未执行：{}", effect.text),
                            effects: vec![effect],
                            error: None,
                        };
                        if self
                            .transact(|runtime| {
                                runtime.director.state.scheduled.remove(index);
                                runtime.director.remember(entry.clone());
                                Ok::<_, host::HostError>(())
                            })
                            .is_err()
                        {
                            break;
                        }
                        // 撤回后世界日志回到事务前；改道的通知本身仍是真实发生的。
                        events.push(self.world.take_events());
                        entry
                    }
                },
            };
            self.director_events.extend(events.into_iter().flatten());
            self.director_outbox.push(entry);
        }
    }
}

fn allowed(tool: &str) -> bool {
    matches!(
        tool,
        "announce"
            | "set_weather"
            | "place_prop"
            | "remove_prop"
            | "post_bulletin"
            | "adjust_supply"
            | "trigger_game_event"
    )
}

fn validate(tool: &str, arguments: &Value) -> Result<(), &'static str> {
    if arguments.to_string().len() > 16_384 {
        return Err("预约参数过长");
    }
    let required: &[&str] = match tool {
        "announce" => &["text"],
        "set_weather" => &["mode"],
        "place_prop" => &["definition", "locale"],
        "remove_prop" => &["id"],
        "post_bulletin" => &["locale", "board", "title", "text"],
        "adjust_supply" => &["id", "operation", "reason"],
        "trigger_game_event" => &["kind"],
        _ => return Err("不支持的预约操作"),
    };
    if required.iter().any(|key| {
        arguments[key]
            .as_str()
            .is_none_or(|text| text.trim().is_empty())
    }) {
        return Err("预约操作缺少必填字符串参数；请按对应操作工具的字段填写");
    }
    if tool == "set_weather" {
        match arguments["mode"].as_str() {
            Some("natural") => {}
            Some("fixed")
                if serde_json::from_value::<protocol::WeatherKind>(arguments["kind"].clone())
                    .is_ok() => {}
            _ => return Err("天气预约须指定 natural，或 fixed 加有效的 kind"),
        }
    }
    if tool == "adjust_supply" {
        super::tools::supply_adjustment(arguments)?;
    }
    if tool == "trigger_game_event" {
        serde_json::from_value::<protocol::GameModeEvent>(arguments.clone())
            .map_err(|_| "天灾参数无效，须指定玩法事件类型及事件参数")?;
    }
    Ok(())
}
