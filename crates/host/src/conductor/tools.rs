//! 导演的工具：每个调用都翻成 Runtime 已有的命令，改动经内核校验、随存档提交。
//! 一轮里改变世界的调用是一笔改动：全部做完才存一次盘，任何一条失败整轮撤回，
//! 世界不会留下半截效果；失败的条目照样回给玩家看原因。
use crate::runtime::Runtime;
use kernel::props::{Instance, Placement};
use protocol::{DirectorEffect, DirectorErrorKind, PlaceNear, WeatherKind, WeatherMode};
use serde_json::Value;

/// 每次回复最多执行几个改变世界的调用。
pub(crate) const CALLS_PER_ROUND: usize = 4;
/// 道具实例编号前缀：和玩家自己摆的 `user:` 分开，日志里分得清是谁放的。
const DIRECTOR_PROP_PREFIX: &str = "director:";

impl Runtime {
    /// 执行一轮回复里的工具调用。结果写进 `director.effects`，由轮次结尾广播。
    /// 读工具只回答问题，不改变世界；先读的事实用于后面同一轮的写操作，写操作不进入下一轮。
    pub(crate) fn director_apply(&mut self, reply: &agent::conversation::ModelReply) {
        let mut effects = Vec::new();
        let started = self.begin_change();
        let mut changes = 0;
        let mut rolled_back = false;
        for call in &reply.calls {
            if rolled_back {
                effects.push(failed(
                    &call.name,
                    DirectorErrorKind::Rejected,
                    "前面的一项没有通过，这一轮已整体撤回，这条没有执行",
                ));
                continue;
            }
            match call.name.as_str() {
                "look" | "survey" | "watch" | "inspect" | "read_content" | "trace" | "history" => {
                    let effect = match self.director_read(&call.name, &call.arguments) {
                        Ok(effect) => effect,
                        Err(effect) => effect,
                    };
                    effects.push(effect);
                }
                "narrate" => effects.push(done("narrate", "记下了一段旁白".into())),
                "announce" | "schedule" | "place_prop" | "remove_prop" | "post_bulletin"
                | "set_weather" => {
                    if changes == CALLS_PER_ROUND {
                        effects.push(failed(
                            &call.name,
                            DirectorErrorKind::Rejected,
                            "一轮最多做 4 件事，这条没有执行",
                        ));
                        continue;
                    }
                    changes += 1;
                    match self.director_change(&call.name, &call.arguments) {
                        Ok(effect) => effects.push(effect),
                        Err(effect) => {
                            rolled_back = true;
                            effects.push(effect);
                        }
                    }
                }
                other => effects.push(failed(
                    other,
                    DirectorErrorKind::InvalidArguments,
                    "没有这个工具",
                )),
            }
        }
        if changes > 0 {
            let edits_catalog = reply.calls.iter().any(|c| {
                matches!(
                    c.name.as_str(),
                    "place_prop" | "remove_prop" | "post_bulletin"
                )
            });
            if rolled_back {
                // 已有条目失败时整轮不提交：世界停在改动前，给玩家看的仍是各条的失败原因。
                started.rollback(self);
            } else {
                self.director_props_changed = self.director_props_changed || edits_catalog;
                match started.commit::<host::HostError>(self) {
                    Ok(()) => effects.push(done("world", "改动已保存".into())),
                    Err(aborted) => {
                        self.director_props_changed = false;
                        effects.push(failed(
                            "world",
                            DirectorErrorKind::World,
                            &format!("改动保存失败，这一轮已撤回：{aborted}"),
                        ));
                    }
                }
            }
        }
        self.director.effects.extend(effects);
    }

    /// 读工具的事实只进入本轮的题面和给玩家的回答，不写存档。
    fn director_read(&self, name: &str, args: &Value) -> Result<DirectorEffect, DirectorEffect> {
        let text = match name {
            "look" => "重新查看了世界".into(),
            "survey" => crate::conductor::context::survey(self),
            "watch" => crate::conductor::context::watch(self, args),
            "inspect" => crate::conductor::context::inspect(self, args)?,
            "read_content" => crate::conductor::context::read_content(self, args)?,
            "trace" => crate::conductor::context::trace(self, args)?,
            "history" => crate::conductor::context::history(self),
            _ => return Err(bad("没有这个读工具")),
        };
        Ok(done(name, text))
    }

    fn director_change(
        &mut self,
        name: &str,
        args: &Value,
    ) -> Result<DirectorEffect, DirectorEffect> {
        let ok = |text: String| Ok(done(name, text));
        match name {
            "announce" => {
                let text = text_arg(args, "text")?;
                let locale = optional_arg(args, "locale");
                self.world
                    .director_announce(&text, locale.as_deref())
                    .map_err(|e| kernel_error(name, e))?;
                ok(format!("通知了：{}", short(&text)))
            }
            "schedule" => {
                let text = text_arg(args, "text")?;
                let minutes = args["after_minutes"]
                    .as_f64()
                    .ok_or_else(|| bad("after_minutes 须是数字"))?;
                let locale = optional_arg(args, "locale");
                let at = self
                    .world
                    .director_schedule(&text, minutes, locale.as_deref())
                    .map_err(|e| kernel_error(name, e))?;
                ok(format!(
                    "{} 会通知：{}",
                    kernel::clock_string(at),
                    short(&text)
                ))
            }
            "place_prop" => {
                let definition = text_arg(args, "definition")?;
                let locale = text_arg(args, "locale")?;
                // 模型常把道具名当编号；只在名字唯一命中时替它换成库里的定义编号。
                // 实例必须写库中定义的完整引用（core:prop_x@1），不能由名字造出新定义。
                let entry = match self.director_definition(&definition) {
                    Ok(entry) => entry,
                    Err(message) => {
                        return Err(failed(name, DirectorErrorKind::Rejected, &message))
                    }
                };
                let reference = entry.reference();
                if !self.world.topo.locales.contains_key(&locale) {
                    return Err(failed(
                        name,
                        DirectorErrorKind::World,
                        &format!("没有这个地点：{locale}"),
                    ));
                }
                let instance = Instance {
                    id: self.next_director_prop_id(),
                    definition: reference,
                    locale: locale.clone(),
                    name: optional_arg(args, "name"),
                    board: None,
                };
                let placement: Placement = self
                    .library_placement_with(
                        serde_json::to_value(&instance).map_err(|e| bad(&e.to_string()))?,
                        DIRECTOR_PROP_PREFIX,
                    )
                    .map_err(|e| failed(name, DirectorErrorKind::Rejected, &e))?;
                self.world
                    .place_prop_near(placement, None::<&PlaceNear>)
                    .map_err(|e| kernel_error(name, e))?;
                ok(format!(
                    "在{}投放了 {}",
                    self.world.topo.locale_name(&locale),
                    instance.id
                ))
            }
            "remove_prop" => {
                let id = text_arg(args, "id")?;
                self.world
                    .remove_prop(&id)
                    .map_err(|e| kernel_error(name, e))?;
                ok(format!("移除了 {id}"))
            }
            "post_bulletin" => {
                let locale = text_arg(args, "locale")?;
                let board = text_arg(args, "board")?;
                let title = text_arg(args, "title")?;
                let text = text_arg(args, "text")?;
                let id = format!("user:director-{}", self.director.state.next_round);
                self.world
                    .create_content(
                        id,
                        locale.clone(),
                        title.clone(),
                        "导演".into(),
                        text,
                        Some(board),
                    )
                    .map_err(|e| kernel_error(name, e))?;
                ok(format!(
                    "在{}贴了「{}」",
                    self.world.topo.locale_name(&locale),
                    title
                ))
            }
            "set_weather" => {
                if self.world.weather().is_none() {
                    return Err(failed(name, DirectorErrorKind::World, "这张地图没有天气"));
                }
                let mode = match args["mode"].as_str() {
                    Some("fixed") => WeatherMode::Fixed,
                    Some("natural") => WeatherMode::Natural,
                    _ => return Err(bad("mode 只能是 fixed 或 natural")),
                };
                let kind = match optional_arg(args, "kind") {
                    Some(kind) => Some(
                        serde_json::from_value::<WeatherKind>(Value::String(kind))
                            .map_err(|_| bad("不支持的天气"))?,
                    ),
                    None => None,
                };
                let request = kernel::weather::WeatherRequest {
                    request_id: format!("director-{}", self.director.state.next_round),
                    expected_revision: 0,
                    mode,
                    kind,
                };
                let view = self
                    .world
                    .set_weather(request)
                    .map_err(|e| kernel_error(name, e))?;
                ok(format!("天气改为{}", view.kind.label()))
            }
            other => Err(bad(&format!("没有这个工具：{other}"))),
        }
    }

    fn next_director_prop_id(&self) -> String {
        let mut n = 1;
        loop {
            let id = format!("{DIRECTOR_PROP_PREFIX}prop-{n}");
            if self.world.topo.prop(&id).is_none() {
                return id;
            }
            n += 1;
        }
    }
}

impl Runtime {
    /// 从道具库中按定义编号或唯一名字找到可投放的定义；同名定义给出候选，不猜。
    fn director_definition(&self, wanted: &str) -> Result<&kernel::props::Definition, String> {
        let wanted = wanted.trim();
        if let Some(d) = self.prop_library.catalog.latest(wanted) {
            return Ok(d);
        }
        let matches: Vec<_> = self
            .prop_library
            .placeable()
            .filter(|d| d.name == wanted || d.reference() == wanted)
            .collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => {
                let names: Vec<_> = self
                    .prop_library
                    .placeable()
                    .take(12)
                    .map(|d| format!("{}（{}）", d.name, d.id))
                    .collect();
                Err(format!(
                    "道具库里没有「{wanted}」；只能按库里已有的名称或定义编号投放。{}",
                    if names.is_empty() {
                        "道具库是空的".into()
                    } else {
                        format!("例如：{}", names.join("、"))
                    }
                ))
            }
            many => Err(format!(
                "「{wanted}」对应多个道具定义，请给定义编号：{}",
                many.iter()
                    .map(|d| format!("{}（{}）", d.name, d.id))
                    .collect::<Vec<_>>()
                    .join("、")
            )),
        }
    }
}

fn text_arg(args: &Value, key: &str) -> Result<String, DirectorEffect> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| bad(&format!("缺少 {key}")))
}
fn optional_arg(args: &Value, key: &str) -> Option<String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}
fn bad(message: &str) -> DirectorEffect {
    failed("", DirectorErrorKind::InvalidArguments, message)
}
fn kernel_error(name: &str, error: kernel::KernelError) -> DirectorEffect {
    failed(name, DirectorErrorKind::World, &error.to_string())
}
fn done(tool: &str, text: String) -> DirectorEffect {
    DirectorEffect {
        tool: tool.into(),
        ok: true,
        text,
        error_kind: None,
    }
}
fn failed(tool: &str, error_kind: DirectorErrorKind, text: &str) -> DirectorEffect {
    DirectorEffect {
        tool: tool.into(),
        ok: false,
        text: text.into(),
        error_kind: Some(error_kind),
    }
}
fn short(text: &str) -> String {
    let mut s: String = text.chars().take(40).collect();
    if text.chars().count() > 40 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use host::props::PropLibrary;
    use std::fs;

    /// 自带道具库 `core:` 定义也是库内内容，导演必须能投放；库外定义与乱写的名字必须被拒。
    #[test]
    fn director_places_only_definitions_that_are_in_the_prop_library() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let map: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("data/maps/2d-town/map.json")).unwrap(),
        )
        .unwrap();
        let library = PropLibrary::load(
            root.join("data/props/core/definitions.json")
                .to_str()
                .unwrap(),
            root.join("data/props/compatibility/retired-core-20260925.json")
                .to_str()
                .unwrap(),
            None,
            &map,
        )
        .unwrap();
        let core = library
            .catalog
            .latest("core:prop_6d11edf922")
            .expect("自带道具库应有手推车");
        assert!(library.placeable().any(|d| d.id == core.id));
        assert!(core.id.starts_with(host::props::CORE_PREFIX));
        assert!(library.catalog.latest("not:in-library").is_none());
    }
}
