//! 导演每轮看到的世界：时钟、天气、地点、在场村民、已投放的道具、道具库里能放的、待触发的事、最近事件。
//! 另有只读的村民与剧情视图，供 `inspect`、`read_content`、`trace` 回答玩家的问题。
//! 只读、有界；不包含模型额度、密钥与模型配置。私聊与心声只进模型上下文，面板另行只展示导演的总结。
use crate::runtime::Runtime;
use serde_json::Value;
use std::fmt::Write;

/// 每条清单最多列这么多项。
const LIST_MAX: usize = 60;
/// 只列一部分时必须告诉模型还有多少件，免得它以为库只有这些。
const CATALOG_INLINE: usize = 240;
/// 最近事件条数。
const RECENT_EVENTS: usize = 20;
/// 单独读一个村民时最多给多少条记忆、想法与台词。
const PERSON_LINES: usize = 30;
const PERSON_CHAT_LINES: usize = 40;
const CONTENT_MAX_CHARS: usize = 3000;
const WATCH_ENTRIES: usize = 60;

pub(crate) fn world_view(runtime: &Runtime) -> String {
    let world = &runtime.world;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "时钟：{}，节奏 {}{}",
        world.clock(),
        runtime.speed,
        if runtime.speed > 0.0 {
            ""
        } else {
            "（暂停）"
        }
    );
    match world.weather() {
        Some(w) => {
            let _ = writeln!(
                out,
                "天气：{}（{}），气候 {}",
                w.view.kind.label(),
                match w.view.mode {
                    protocol::WeatherMode::Natural => "自然",
                    protocol::WeatherMode::Fixed => "固定",
                },
                world.climate().label()
            );
        }
        None => {
            let _ = writeln!(out, "天气：这张地图没有天气");
        }
    }

    let _ = writeln!(out, "\n地点（编号 名字 · 类型）：");
    for place in world.topo.places.iter().take(LIST_MAX) {
        let _ = writeln!(
            out,
            "- {} {} · {}{}",
            place.locale,
            place.name,
            place.place_type,
            if place.residential { " · 住宅" } else { "" }
        );
    }

    let _ = writeln!(out, "\n在场村民（编号 名字 @地点 · 正在做 · 身体）：");
    let mut people = 0;
    for id in world.actor_ids() {
        let Some(a) = world.actor(&id) else { continue };
        if !a.present() || a.is_avatar() {
            continue;
        }
        let body = if a.needs.body.dead {
            " · 已死亡"
        } else if a.needs.incapacitated() {
            " · 倒下"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "- {} {} @{} · {}{}",
            a.id,
            a.name,
            world.topo.locale_name(&a.locus.locale),
            a.current.verb,
            body
        );
        people += 1;
        if people >= LIST_MAX {
            break;
        }
    }
    if people == 0 {
        let _ = writeln!(out, "（还没有村民）");
    }

    // 能贴公告的公告栏：导演贴公告要在已有的栏上贴，所以单独列出来
    let props = world.prop_views();
    let boards: Vec<_> = props.iter().filter(|p| p.bulletin_board).collect();
    if !boards.is_empty() {
        let _ = writeln!(out, "\n公告栏（贴公告用它的编号）：");
        for p in boards {
            let _ = writeln!(
                out,
                "- {} {} @{}",
                p.id,
                p.name,
                world.topo.locale_name(&p.locale)
            );
        }
    }
    let _ = writeln!(out, "\n已投放的道具（编号 名字 @地点）：");
    for p in props.iter().take(LIST_MAX) {
        let _ = writeln!(
            out,
            "- {} {} @{}",
            p.id,
            p.name,
            world.topo.locale_name(&p.locale)
        );
    }
    if props.is_empty() {
        let _ = writeln!(out, "（没有）");
    }

    // 内置地图道具和玩家自制道具都由道具库校验；这里给定义编号，实例引用由主机补版本号。
    let mut placeable = 0;
    let total = runtime.prop_library.placeable().count();
    let _ = writeln!(
        out,
        "\n道具库里可投放的（名字 定义编号；库内共 {total} 件）："
    );
    for d in runtime.prop_library.placeable().take(CATALOG_INLINE) {
        let _ = writeln!(out, "- {} {}", d.name, d.id);
        placeable += 1;
    }
    if total > placeable {
        let _ = writeln!(out, "（上面是前 {placeable} 件；库里的全部都能投放）");
    }
    if placeable == 0 {
        let _ = writeln!(out, "（道具库是空的，只能贴公告、发通知、预约事件）");
    }

    let pending = world.director_pending();
    if !pending.is_empty() {
        let _ = writeln!(out, "\n待触发的事：");
        for (at, locale, text) in pending {
            let _ = writeln!(
                out,
                "- {} · {} · {}",
                kernel::clock_string(at),
                world.topo.locale_name(&locale),
                text
            );
        }
    }

    if !runtime.director_recent.is_empty() {
        let _ = writeln!(out, "\n最近发生：");
        for (clock, text) in runtime
            .director_recent
            .iter()
            .rev()
            .take(RECENT_EVENTS)
            .rev()
        {
            let _ = writeln!(out, "- {clock} {text}");
        }
    }
    out
}

/// 快速全景：给“现在小镇怎么样”这类问题，列出人中谁在做什么、世界有哪些异动。
pub(crate) fn survey(runtime: &Runtime) -> String {
    let world = &runtime.world;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}，{}。",
        world.clock(),
        match world.weather() {
            Some(w) => format!("{}天气", w.view.kind.label()),
            None => "无天气".into(),
        }
    );
    let living: Vec<_> = world
        .actor_ids()
        .into_iter()
        .filter_map(|id| world.actor(&id).map(|a| (id, a)))
        .filter(|(_, a)| a.present() && !a.is_avatar())
        .collect();
    let _ = writeln!(out, "镇上 {} 人：", living.len());
    for (_, a) in &living {
        let plan = if a.waiting_for_plan() {
            "（等新计划）"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "- {} {} @{} · {}{}",
            a.id,
            a.name,
            world.topo.locale_name(&a.locus.locale),
            a.current.verb,
            plan
        );
        for step in a.plan.iter().take(3) {
            let _ = writeln!(out, "    接着：{}", step.describe(&world.topo));
        }
    }
    if !runtime.director_recent.is_empty() {
        let _ = writeln!(out, "最近的世界动静：");
        for (clock, text) in runtime.director_recent.iter().rev().take(12).rev() {
            let _ = writeln!(out, "- {clock} {text}");
        }
    }
    out
}

/// 按名称或编号读一个村民：目标、计划、最近记忆、日记、想法与听到的台词。
/// 编码口径与面板一致：这些是导演能看到的上下文，不是世界事实。
pub(crate) fn inspect(runtime: &Runtime, args: &Value) -> Result<String, protocol::DirectorEffect> {
    let world = &runtime.world;
    let wanted = args["actor"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_arg("inspect 需要 actor：村民名字或编号"))?;
    let actor = world.actor(wanted).or_else(|| {
        world
            .actor_ids()
            .into_iter()
            .filter_map(|id| world.actor(&id))
            .find(|a| a.name == wanted)
    });
    let Some(a) = actor else {
        let names: Vec<_> = world
            .actor_ids()
            .into_iter()
            .filter_map(|id| world.actor(&id).map(|a| format!("{}（{}）", a.name, a.id)))
            .take(20)
            .collect();
        return Err(protocol::DirectorEffect {
            tool: "inspect".into(),
            ok: false,
            text: format!("没有叫「{wanted}」的村民。现有：{}", names.join("、")),
            error_kind: Some(protocol::DirectorErrorKind::Rejected),
        });
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}（{}）@{} · {}{}",
        a.name,
        a.id,
        world.topo.locale_name(&a.locus.locale),
        a.current.verb,
        if a.present() { "" } else { " · 不在场" }
    );
    if a.waiting_for_plan() {
        let _ = writeln!(out, "正在等新计划。");
    }
    let _ = writeln!(out, "正在做：{}", a.current.verb);
    if !a.plan.is_empty() {
        let _ = writeln!(out, "接着计划：");
        for q in a.plan.iter().take(8) {
            let _ = writeln!(out, "- {}", q.describe(&world.topo));
        }
    }
    if let Some(t) = a.journal.thought.as_ref() {
        let _ = writeln!(
            out,
            "最近的想法（{}）：{}",
            kernel::clock_string(t.at),
            truncate(&t.text, 500)
        );
    }
    let facts: Vec<_> = a
        .journal
        .days
        .iter()
        .rev()
        .take(2)
        .rev()
        .flat_map(|d| {
            d.summary
                .iter()
                .map(move |summary| (d.day, summary.as_str()))
        })
        .collect();
    if !facts.is_empty() {
        let _ = writeln!(out, "过去几天的日记：");
        for (day, text) in facts {
            let _ = writeln!(out, "- D{}：{}", day + 1, truncate(text, 300));
        }
    }
    // 收件箱：导演通知、别人说的话等显著事件都在这里，比流水更接近“记得什么”。
    let notices: Vec<_> = a.journal.events.iter().rev().take(PERSON_LINES).collect();
    if !notices.is_empty() {
        let _ = writeln!(out, "记得的事：");
        for f in notices.into_iter().rev() {
            let _ = writeln!(
                out,
                "- {} {}",
                kernel::clock_string(f.at),
                truncate(&f.text, 300)
            );
        }
    }
    let today = a.journal.today.iter().rev().take(PERSON_LINES);
    let today: Vec<_> = today.collect();
    if !today.is_empty() {
        let _ = writeln!(out, "今天做过：");
        for r in today.into_iter().rev() {
            let _ = writeln!(
                out,
                "- {} {}",
                kernel::clock_string(r.at),
                truncate(&r.text, 240)
            );
        }
    }
    let thoughts: Vec<_> = a.journal.thoughts_today.iter().rev().take(12).collect();
    if !thoughts.is_empty() {
        let _ = writeln!(out, "今天的想法：");
        for t in thoughts.into_iter().rev() {
            let _ = writeln!(
                out,
                "- {} {}",
                kernel::clock_string(t.at),
                truncate(&t.text, 400)
            );
        }
    }
    let recent = world.interactions().seen_by(&a.id, 3);
    if !recent.is_empty() {
        let _ = writeln!(
            out,
            "最近的交谈（含私聊；只供导演判断，不要原样转述给全村）："
        );
        for i in recent {
            let partners: Vec<_> = i
                .chat
                .as_ref()
                .map(|c| c.ever_members.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_else(|| vec![i.initiator.clone(), i.partner.clone()]);
            let names: Vec<_> = partners.iter().map(|p| actor_name(world, p)).collect();
            let _ = writeln!(
                out,
                "- {} {}：{}",
                kernel::clock_string(i.created_at),
                names.join(" / "),
                i.reason.clone().unwrap_or_else(|| "交谈".into())
            );
            if let Some(c) = &i.chat {
                let lines: Vec<_> = c
                    .entries
                    .iter()
                    .filter(|e| e.kind == protocol::ChatEntryKind::Speech)
                    .rev()
                    .take(PERSON_CHAT_LINES)
                    .collect();
                for e in lines.into_iter().rev() {
                    let _ = writeln!(out, "    {}：{}", e.name, truncate(&e.text, 300));
                }
            }
        }
    }
    Ok(out)
}

/// 给“最近发生了什么”用：从导演记录里提取时间线。
pub(crate) fn trace(runtime: &Runtime, args: &Value) -> Result<String, protocol::DirectorEffect> {
    Ok(watch(runtime, args))
}

/// 读公告、书籍或告示的正文；只读，不出示给村民。
pub(crate) fn read_content(
    runtime: &Runtime,
    args: &Value,
) -> Result<String, protocol::DirectorEffect> {
    let world = &runtime.world;
    let wanted = args["id"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_arg("read_content 需要 id：公告栏道具或作品编号"))?;
    let prop = world.topo.prop(wanted);
    let content_id = prop
        .and_then(|p| p.readable.as_ref().map(|t| t.work_id.clone()))
        .unwrap_or_else(|| wanted.to_owned());
    let Some(doc) = world.contents().documents.get(&content_id) else {
        let ids: Vec<_> = world
            .contents()
            .documents
            .keys()
            .take(20)
            .cloned()
            .collect();
        return Err(protocol::DirectorEffect {
            tool: "read_content".into(),
            ok: false,
            text: format!("没有这份内容「{wanted}」。可读编号：{}", ids.join("、")),
            error_kind: Some(protocol::DirectorErrorKind::Rejected),
        });
    };
    let Some(t) = doc.versions.last() else {
        return Err(bad_arg(&format!("「{content_id}」没有正文")));
    };
    Ok(format!(
        "{}（编号 {}，第 {} 版，作者 {}）：\n{}",
        t.title,
        t.work_id,
        t.version,
        t.author,
        truncate(&t.text, CONTENT_MAX_CHARS)
    ))
}

/// 最近的世界动静；导演记录在每拍从世界日志里摘取，足够回答剧情回顾。
pub(crate) fn watch(runtime: &Runtime, args: &Value) -> String {
    let minutes = args["minutes"]
        .as_f64()
        .filter(|m| *m > 0.0)
        .unwrap_or(12.0 * 60.0);
    let lines = runtime
        .director_recent
        .iter()
        .rev()
        .take(WATCH_ENTRIES)
        .collect::<Vec<_>>();
    let mut out = format!(
        "最近 {} 分钟的动静（记录 {} 条）：\n",
        minutes.round(),
        lines.len()
    );
    for (clock, text) in lines.into_iter().rev() {
        let _ = writeln!(out, "- {clock} {}", truncate(text, 320));
    }
    out
}

/// 导演自己做过什么：给玩家复盘，不重新影响世界。
pub(crate) fn history(runtime: &Runtime) -> String {
    let mut out = String::new();
    for entry in runtime.director.entries() {
        let _ = writeln!(out, "玩家：{}", entry.player);
        if !entry.reply.trim().is_empty() {
            let _ = writeln!(out, "导演：{}", entry.reply);
        }
        for effect in &entry.effects {
            let _ = writeln!(
                out,
                "- {}：{}",
                if effect.ok { "已做" } else { "未做" },
                effect.text
            );
        }
        if let Some(error) = &entry.error {
            let _ = writeln!(out, "- 失败：{error}");
        }
    }
    out
}

fn actor_name(world: &kernel::World, id: &str) -> String {
    world
        .actor(id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| id.into())
}
fn truncate(text: &str, max: usize) -> String {
    let mut s: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        s.push('…');
    }
    s
}
fn bad_arg(message: &str) -> protocol::DirectorEffect {
    protocol::DirectorEffect {
        tool: String::new(),
        ok: false,
        text: message.into(),
        error_kind: Some(protocol::DirectorErrorKind::InvalidArguments),
    }
}
