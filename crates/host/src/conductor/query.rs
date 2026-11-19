use crate::runtime::Runtime;
use protocol::{DirectorEffect, DirectorErrorKind};
use serde_json::{json, Value};

fn snapshot(runtime: &Runtime) -> Value {
    let residents: serde_json::Map<String, Value> = runtime
        .population
        .ordered()
        .into_iter()
        .map(|resident| {
            (
                resident.id.clone(),
                json!({
                    "id": resident.id,
                    "name": resident.name,
                    "home": resident.home,
                    "joined_at": resident.joined_at,
                    "persona": resident.persona,
                    "appearance": resident.appearance,
                    "needs": resident.needs,
                }),
            )
        })
        .collect();
    let agents: serde_json::Map<String, Value> = runtime
        .driver
        .runtime
        .state()
        .sessions
        .iter()
        .map(|(actor, session)| {
            (
                actor.clone(),
                json!({
                    "instructions": session.instructions,
                    "goals": session.goals,
                    "memory": session.memory,
                    "notebook": session.notebook,
                    "messages": session.messages,
                    "chat": session.chat,
                    "sources": session.input.sources,
                    "pending": session.pending,
                    "pause_kind": session.pause_kind(),
                    "pause_reason": session.pause_reason(),
                    "in_flight": session.in_flight.is_some(),
                }),
            )
        })
        .collect();
    let topo = &runtime.world.topo;
    json!({
        "world": runtime.world.snapshot(),
        "topology": {
            "locales": topo.locales,
            "places": topo.places,
            "passages": topo.passages,
            "stations": topo.stations,
            "props": topo.props(),
            "meals": topo.meals,
            "entry": topo.entry,
        },
        "residents": residents,
        "agents": agents,
        "catalog": runtime.prop_library.catalog.definitions,
        "settings": {
            "worldview": runtime.world_settings.worldview,
            "speed": runtime.speed,
            "pace": runtime.pace,
            "save_failed": runtime.save_failed,
        },
        "director": runtime.director.save(),
    })
}

pub(super) fn read(runtime: &mut Runtime, args: &Value) -> Result<String, DirectorEffect> {
    let path = match args.get("path") {
        None => "",
        Some(Value::String(path)) => path.as_str(),
        _ => return Err(rejected("path 须为字符串")),
    };
    let offset = integer(args, "offset", 0, usize::MAX)?;
    let limit = integer(args, "limit", 6000, 12000)?;
    if limit == 0 {
        return Err(rejected("limit 须大于零"));
    }
    if runtime.director.query_snapshot.is_none() {
        runtime.director.query_snapshot = Some(snapshot(runtime));
    }
    let snapshot = runtime.director.query_snapshot.as_ref().unwrap();
    let value = snapshot
        .pointer(path)
        .ok_or_else(|| rejected("没有这个数据路径；先用空 path 查看目录，路径使用 JSON Pointer，~ 和 / 分别写成 ~0 和 ~1"))?;
    let source = if path.is_empty() {
        json!({
            "world": "完整世界动态状态：actors 身体/行动/日记、inventory 背包、supplies 库存、interactions 交谈、contents 作品与版本；有玩法时包含 game_mode 的完整后台状态",
            "topology": "地图地点、通路、操作位、道具完整定义、用餐规则与入口",
            "residents": "按居民编号索引的人设、住宅、入镇时间与需求参数，不含模型绑定",
            "agents": "按居民编号索引的 Agent 游戏数据：人设与世界观指令、全部目标、长期记忆、笔记、模型对话、私聊、上下文来源原文、待处理工具与运行状态；不含模型额度、密钥或模型绑定配置",
            "catalog": "全部版本的道具定义，按定义引用索引；不是每种定义都能作为场景道具投放，投放以世界校验为准",
            "settings": "世界观、模拟速度、实际节奏与存盘状态；身体开关在 /world/physiology，个人需求参数在 /residents 中",
            "director": "导演会话、摘要与预约",
        })
    } else {
        value.clone()
    };
    let encoded =
        serde_json::to_string_pretty(&source).map_err(|error| rejected(&error.to_string()))?;
    let total = encoded.chars().count();
    if offset > total {
        return Err(rejected("offset 超出正文长度"));
    }
    let data: String = encoded.chars().skip(offset).take(limit).collect();
    let end = offset + data.chars().count();
    Ok(json!({
        "world_id": runtime.population.world_id,
        "snapshot_minutes": snapshot["world"]["minutes"],
        "scope": "本轮首次 query_data 时的只读快照；同轮翻页保持一致，写操作仍以执行时的世界状态校验。无任意文件访问，不含密钥、服务商配置或模型额度。",
        "path": path,
        "encoding": "json_text",
        "offset": offset,
        "total_chars": total,
        "next_offset": if end < total { Some(end) } else { None },
        "data": data,
    }).to_string())
}

fn integer(args: &Value, key: &str, default: usize, max: usize) -> Result<usize, DirectorEffect> {
    match args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .filter(|number| *number <= max)
            .ok_or_else(|| rejected(&format!("{key} 须为 0 到 {max} 的整数"))),
    }
}

fn rejected(text: &str) -> DirectorEffect {
    DirectorEffect {
        tool: "query_data".into(),
        ok: false,
        text: text.into(),
        error_kind: Some(DirectorErrorKind::InvalidArguments),
    }
}
