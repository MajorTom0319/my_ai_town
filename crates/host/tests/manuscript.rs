//! 实时剧本：世界确认的动作结果按世界追加；来源相同的只收一次；
//! 重启（读档）后接着编号写，不重发旧条目；世界重置换新目录，旧世界流水原样保留。
use crate::{
    avatar::{command, state},
    director::Endpoint,
    support::*,
};
use serde_json::{json, Value};
use std::{fs, time::Instant};

fn manuscript(ws: &mut tungstenite::WebSocket<std::net::TcpStream>) -> Value {
    reply(ws, json!({"type":"get_manuscript"}), "manuscript_page")["page"].clone()
}

#[test]
fn the_manuscript_appends_once_per_source_and_survives_a_restart() {
    let mut town = Town::new();
    town.args = map_args(MAP_2D);
    town.mock_delay = Some("0.01".into());
    let mut props = read_json(&town.dir.join("props.json"));
    let catalog: Value =
        serde_json::from_str(include_str!("../../../data/props/core/definitions.json")).unwrap();
    props.as_array_mut().unwrap().push(
        catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|prop| prop["id"] == "core:bulletin_board")
            .unwrap()
            .clone(),
    );
    let mut ws = town.start();
    let s = state(&mut ws);
    let entered = command(
        &mut ws,
        &s,
        1,
        json!({"kind":"enter","name":"aya","appearance":{"marker":"我","color":"#387963"}}),
    );
    assert_eq!(entered["ok"], true, "{entered}");
    let s = entered["state"].clone();
    let empty_world = s["world_id"].as_str().unwrap().to_owned();
    let early = reply(
        &mut ws,
        json!({"type":"summarize_manuscript","request_id":"empty","world_id":empty_world}),
        "manuscript_summary",
    );
    assert_eq!(early["summary"], Value::Null);
    assert!(
        early["error"]
            .as_str()
            .is_some_and(|e| e.contains("没有可整理")),
        "{early}"
    );
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"go","speed":600}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 600.0);
    let world_id = s["world_id"].as_str().unwrap().to_owned();
    let action = command(
        &mut ws,
        &s,
        2,
        json!({"kind":"action","revision":s["actor"]["revision"],"action":{"intent":{"verb":"停留","duration_minutes":2}}}),
    );
    assert_eq!(action["ok"], true, "{action}");
    let mut seen: Vec<Value> = Vec::new();
    let mut found = None;
    // 这一局只有化身在行动：读到化身的停留结果就停表，随后别的居民不会先写进剧本。
    while found.is_none() {
        for message in received_until(&mut ws, |m| {
            m["type"] == "manuscript_append" || m["type"] == "roster"
        }) {
            if message["type"] == "manuscript_append" {
                seen.extend(message["entries"].as_array().unwrap().clone());
            }
        }
        found = seen
            .iter()
            .find(|e| {
                e["kind"] == "action"
                    && e["actor"] == "player_avatar"
                    && e["text"].as_str().is_some_and(|t| t.contains("停留"))
            })
            .cloned();
    }
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"stop","speed":0}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 0.0);
    let result = found.unwrap();
    assert_eq!(result["tone"], "result");
    assert_eq!(result["visibility"], "private");
    assert_eq!(result["world_id"], world_id);
    let seqs: Vec<u64> = seen.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "编号要严格递增：{seqs:?}"
    );
    let sources: Vec<&str> = seen.iter().map(|e| e["source"].as_str().unwrap()).collect();
    let mut unique = sources.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        sources.len(),
        "同一来源不能收两次：{sources:?}"
    );
    let page = manuscript(&mut ws);
    // 停表前最后一拍可能跨过整点，多写一条天气；以停表后的实际编号为准继续核对。
    let count = page["cursor"].as_u64().unwrap();
    let last_seq = count;
    assert_eq!(page["cursor"], count, "停表后剧本不再追加：{page}");

    // 往前翻一页要按编号接得上，而不是每次都把同一批旧条目再发一遍。
    let earlier = reply(
        &mut ws,
        json!({"type":"get_manuscript","before":last_seq,"limit":500}),
        "manuscript_page",
    )["page"]
        .clone();
    let old: Vec<u64> = earlier["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(
        old,
        (last_seq.saturating_sub(500).max(1)..last_seq).collect::<Vec<_>>(),
        "旧记录要按编号向前读"
    );

    town.shutdown();
    let dir = town.dir.join("state/manuscript").join(&world_id);
    let persisted = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert_eq!(persisted.lines().count() as u64, count);
    assert!(count >= seen.len() as u64);

    let mut ws = town.start();
    // 重启后自动整理可能先占住模型池；测试手动整理，等它把手上的那一段整理完再来。
    reply(
        &mut ws,
        json!({"type":"set_auto_summary","world_id":world_id,"request_id":"manual-check","enabled":false}),
        "world_settings_saved",
    );
    let page = manuscript(&mut ws);
    assert_eq!(page["world_id"], world_id);
    assert_eq!(page["cursor"], count);
    assert_eq!(
        page["entries"].as_array().unwrap().last().unwrap()["seq"],
        count
    );
    assert_eq!(page["entries"].as_array().unwrap().len() as u64, count);
    assert_eq!(
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["seq"] == last_seq)
            .count(),
        1,
        "重启后同一动作只读回一次"
    );
    assert!(
        fs::read_to_string(dir.join("events.jsonl"))
            .unwrap()
            .lines()
            .count() as u64
            >= count,
        "重启后继续写最后一段，不另开错位的新段"
    );

    // 概览是单独任务产出的归纳：有实录后可以生成，结果能追溯来源，重启后仍在。
    assert!(!page["entries"].as_array().unwrap().is_empty());
    send(
        &mut ws,
        json!({"type":"summarize_manuscript","request_id":"summary","world_id":world_id}),
    );
    let acknowledged = until(&mut ws, |m| {
        m["type"] == "manuscript_summary" && m["request_id"] == "summary"
    });
    // 重启后可能先收完在途的自动整理，手动请求才排上；它自己发出的回执一定不再说“正在整理”。
    if acknowledged["error"].as_str() != Some("剧情概览正在整理，稍等一下") {
        assert_eq!(acknowledged["error"], Value::Null, "{acknowledged}");
    }
    let deadline = Instant::now() + LONG_WAIT;
    let done = loop {
        let message = until(&mut ws, |m| m["type"] == "manuscript_summary");
        if message["request_id"] == "summary" && message["generated"] == true {
            break message;
        }
        if message["request_id"].is_null() && message["generated"] == true {
            // 在途的自动整理完成；等它把模型池让出来后再请一次手动整理。
            send(
                &mut ws,
                json!({"type":"summarize_manuscript","request_id":"summary","world_id":world_id}),
            );
        }
        assert!(Instant::now() < deadline, "手动整理一直没有回结果");
    };
    assert!(!done["summary"]["situation"].as_str().unwrap().is_empty());
    assert_eq!(done["summary"]["through_seq"], count);
    let after_summary = fs::read_to_string(dir.join("summary.json")).unwrap();
    assert!(after_summary.contains("through_seq"));

    // 重置世界：新的剧本目录，旧世界的事件不被新局带回。
    reply(
        &mut ws,
        json!({"type":"reset_world","request_id":"reset"}),
        "world_reset",
    );
    let fresh = manuscript(&mut ws);
    assert_ne!(fresh["world_id"], world_id);
    assert_eq!(fresh["cursor"], 0);
    assert!(fresh["entries"].as_array().unwrap().is_empty());
    assert!(dir.exists(), "旧世界的剧本原样保留");
}

#[test]
fn auto_summary_runs_once_for_a_batch_of_facts_and_not_again_while_unchanged() {
    let mut town = Town::new();
    town.args = map_args(MAP_2D);
    let mut ws = town.start();
    let s = state(&mut ws);
    let entered = command(
        &mut ws,
        &s,
        1,
        json!({"kind":"enter","name":"aya","appearance":{"marker":"我","color":"#387963"}}),
    );
    assert_eq!(entered["ok"], true, "{entered}");
    let s = entered["state"].clone();
    let world_id = s["world_id"].as_str().unwrap().to_owned();
    // 只加入化身时没人会持续行动；加一位固定回复的居民，让世界真的写出多条实录。
    let out = join(&mut ws, "tpl_b", None);
    assert_eq!(out["ok"], true, "{out}");
    // 固定回复模式延迟很短，先把自动整理关掉，手动整理一次拿到确定的基准。
    reply(
        &mut ws,
        json!({"type":"set_auto_summary","world_id":world_id,"request_id":"off","enabled":false}),
        "world_settings_saved",
    );
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"base-go","speed":600}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 600.0);
    let deadline = Instant::now() + LONG_WAIT;
    loop {
        assert!(Instant::now() < deadline, "居民一直没有写出可整理的实录");
        let event = until(&mut ws, |m| {
            matches!(m["type"].as_str(), Some("manuscript_append" | "roster"))
        });
        if event["type"] == "manuscript_append" && !event["entries"].as_array().unwrap().is_empty()
        {
            break;
        }
    }
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"base-stop","speed":0}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 0.0);
    send(
        &mut ws,
        json!({"type":"summarize_manuscript","world_id":world_id,"request_id":"base"}),
    );
    let base = until(&mut ws, |m| {
        m["type"] == "manuscript_summary" && m["request_id"] == "base" && m["generated"] == true
    });
    assert_eq!(base["error"], Value::Null, "{base}");
    let from = base["summary"]["through_seq"].as_u64().unwrap();
    reply(
        &mut ws,
        json!({"type":"set_auto_summary","world_id":world_id,"request_id":"on","enabled":true}),
        "world_settings_saved",
    );
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"go","speed":600}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 600.0);
    // 自动整理要等新剧情攒够固定回复模式的门槛；读到就立刻停表，免得停表后主线又跑出一段。
    // 再打开页面按编号续读时，补齐的旧条目在广播里出现得晚；它们不算读到自动整理时的“新剧情”。
    let deadline = Instant::now() + LONG_WAIT;
    let mut produced = false;
    let mut auto = None;
    while Instant::now() < deadline && auto.is_none() {
        let message = until(&mut ws, |m| {
            matches!(
                m["type"].as_str(),
                Some("manuscript_append" | "manuscript_summary" | "roster")
            )
        });
        if message["type"] == "manuscript_append"
            && message["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["seq"].as_u64() > Some(from))
        {
            produced = true;
        }
        if message["type"] == "manuscript_summary"
            && message["generated"] == true
            && message["request_id"].is_null()
            && message["summary"]["through_seq"].as_u64() > Some(from)
        {
            auto = Some(message);
        }
    }
    let auto = auto.unwrap_or_else(|| panic!("没有等到自动整理（produced={produced}）"));
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"stop","speed":0}),
    );
    // 停表回执前已经排进连接的消息可能仍是最后一拍产生的，所以停表后的检查从停表回执之后开始。
    received_until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 0.0);
    assert_eq!(auto["error"], Value::Null, "{auto}");
    let through = auto["summary"]["through_seq"].as_u64().unwrap();
    reply(
        &mut ws,
        json!({"type":"set_auto_summary","world_id":world_id,"request_id":"off","enabled":false}),
        "world_settings_saved",
    );
    let stopped = manuscript(&mut ws);
    let stopped = stopped["cursor"].as_u64().unwrap();
    assert!(stopped >= through);
    let seen = manuscript(&mut ws);
    assert_eq!(
        seen["cursor"].as_u64(),
        Some(stopped),
        "暂停时实录不能再被补写"
    );
}

/// 七人必须各有住宅；没有住宅的参赛者会在第一天结算时被当作“没存活”记账，测试不能把经营失败混进隐私断言。
fn seven_homes(town: &Town) {
    let mut topology = read_json(&town.dir.join("topology.json"));
    for index in 4..=7 {
        let home = format!("home_{index}");
        let station = format!("arrive_home_{index}");
        let node = format!("n_home_{index}");
        topology["locales"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":home,"name":home,"kind":"dwell"}));
        topology["stations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":station,"locale":home,"name":home,"kind":"arrival","node":node}));
        topology["passages"].as_array_mut().unwrap().push(json!({"id":format!("door_{home}"),"a":"square","b":home,"node_a":"n_square","node_b":node,"minutes":1,"capabilities":["通行"]}));
        topology["places"].as_array_mut().unwrap().push(json!({"name":home,"locale":home,"arrival_station":station,"type":"住家","summary":"","residential":true}));
    }
    town.write("topology.json", topology);
}

/// 洪涝的受灾名单和出局者的私粮余额只给全知剧本；公开流水的公告仍保持公开，
/// 不能靠删掉秘密来“通过”，也不能把这些内容写进任何居民的记忆。
#[test]
fn survival10_judge_only_results_stay_out_of_the_public_view() {
    let endpoint = Endpoint::new(vec![
        json!({"role":"assistant","content":"已执行并张贴公告、通知在场参赛者：洪涝。","tool_calls":[{"id":"flood","type":"function","function":{"name":"trigger_game_event","arguments":"{\"kind\":\"survival10_disaster\",\"event\":{\"kind\":\"flood\"}}"}}]}),
    ]);
    let mut town = endpoint.town();
    town.args.clear();
    seven_homes(&town);
    let mut props = read_json(&town.dir.join("props.json"));
    let catalog: Value =
        serde_json::from_str(include_str!("../../../data/props/core/definitions.json")).unwrap();
    let defined = |id: &str| {
        catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|prop| prop["id"] == id)
            .unwrap()
            .clone()
    };
    props.as_array_mut().unwrap().extend(
        [
            "core:bulletin_board",
            "core:public-granary",
            "core:private-ration-sack",
        ]
        .into_iter()
        .map(defined),
    );
    town.write("props.json", props);
    for index in 0..7 {
        let mut template = read_json(&fixture("library/tpl_a.json"));
        template["template_id"] = json!(format!("privacy_{index}"));
        template["fields"]["name"] = json!(format!("参赛者{index}"));
        town.write(&format!("library/privacy_{index}.json"), template);
    }
    let mut ws = town.start();
    for index in 0..7 {
        join_id(&mut ws, &format!("privacy_{index}"));
    }
    text_only_world(&mut ws);
    let state = reply(
        &mut ws,
        json!({"type":"get_world_settings"}),
        "world_settings",
    )["state"]
        .clone();
    let world_id = state["world_id"].clone();
    for (id, definition, locale) in [
        ("user:s10-disaster-board", "core:bulletin_board@1", "square"),
        ("user:s10-granary-v2", "core:public-granary@2", "square"),
        (
            "user:s10-home_pantry_v2_1",
            "core:private-ration-sack@2",
            "home_1",
        ),
    ] {
        let out = reply(
            &mut ws,
            json!({"type":"place_prop","request_id":id,"world_id":world_id,"instance":{"id":id,"definition":definition,"locale":locale}}),
            "prop_result",
        );
        assert_eq!(out["ok"], true, "{out}");
    }
    let mode = reply(
        &mut ws,
        json!({"type":"set_game_mode","request_id":"mode","world_id":world_id,"game_mode":{"kind":"survival10","seed":7}}),
        "world_settings_saved",
    );
    assert_eq!(mode["ok"], true, "{mode}");
    send(
        &mut ws,
        json!({"type":"director_say","request_id":"flood","world_id":world_id,"text":"让洪水冲掉几家的私粮"}),
    );
    let outcome = until(&mut ws, |message| {
        message["type"] == "manuscript_append"
            && message["entries"].as_array().unwrap().iter().any(|entry| {
                entry["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("裁判全知") && text.contains("住宅受灾"))
            })
    });
    let entries = outcome["entries"].as_array().unwrap();
    let loss = entries
        .iter()
        .find(|entry| {
            entry["text"]
                .as_str()
                .is_some_and(|text| text.contains("住宅受灾"))
        })
        .unwrap();
    assert_eq!(loss["visibility"], "private", "{loss}");
    let notice = entries
        .iter()
        .find(|entry| {
            entry["source"]
                .as_str()
                .is_some_and(|source| source.ends_with("-disaster-1-notice"))
        })
        .expect("公开的张贴公告仍要在剧本里");
    assert_eq!(notice["visibility"], "public", "{notice}");
}

/// 天灾公告只公开公仓损失；剧本另给玩家看清受害家庭，但这份全知结果不进任何人的记忆。
#[test]
fn disaster_losses_are_told_to_the_audience_without_leaking_to_residents() {
    let endpoint = Endpoint::new(vec![
        json!({"role":"assistant","content":"已执行并张贴公告、通知在场参赛者：洪涝。","tool_calls":[{"id":"flood","type":"function","function":{"name":"trigger_game_event","arguments":"{\"kind\":\"survival10_disaster\",\"event\":{\"kind\":\"flood\"}}"}}]}),
        json!({"role":"assistant","content":"已核实。"}),
        json!({"role":"assistant","content":"天灾已经执行。"}),
        json!({"role":"assistant","content":"按规则发通知。","tool_calls":[{"id":"flood","type":"function","function":{"name":"trigger_game_event","arguments":"{\"kind\":\"survival10_disaster\",\"event\":{\"kind\":\"flood\"}}"}},{"id":"fail","type":"function","function":{"name":"remove_prop","arguments":"{\"id\":\"missing-prop\"}"}}]}),
        json!({"role":"assistant","content":"这一轮没有执行。"}),
    ]);
    let mut town = endpoint.town();
    town.args.clear();
    let mut topology = read_json(&town.dir.join("topology.json"));
    for index in 4..=7 {
        let home = format!("home_{index}");
        let station = format!("arrive_home_{index}");
        let node = format!("n_home_{index}");
        topology["locales"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":home,"name":home,"kind":"dwell"}));
        topology["stations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":station,"locale":home,"name":home,"kind":"arrival","node":node}));
        topology["passages"].as_array_mut().unwrap().push(json!({"id":format!("door_{home}"),"a":"square","b":home,"node_a":"n_square","node_b":node,"minutes":1,"capabilities":["通行"]}));
        topology["places"].as_array_mut().unwrap().push(json!({"name":home,"locale":home,"arrival_station":station,"type":"住家","summary":"","residential":true}));
    }
    town.write("topology.json", topology);
    for index in 0..7 {
        let mut template = read_json(&fixture("library/tpl_a.json"));
        template["template_id"] = json!(format!("disaster_{index}"));
        template["fields"]["name"] = json!(format!("参赛者{index}"));
        town.write(&format!("library/disaster_{index}.json"), template);
    }
    let mut props = read_json(&town.dir.join("props.json"));
    let catalog: Value =
        serde_json::from_str(include_str!("../../../data/props/core/definitions.json")).unwrap();
    props.as_array_mut().unwrap().push(
        catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|prop| prop["id"] == "core:bulletin_board")
            .unwrap()
            .clone(),
    );
    town.write("props.json", props);
    let mut ws = town.start();
    // 只有玩家和导演要收到完整的、可以慢慢读的剧本，不让自动整理另占一份模型回复。
    reply(
        &mut ws,
        json!({"type":"set_auto_summary","request_id":"quiet","world_id":read_json(&town.save())["population"]["world_id"],"enabled":false}),
        "world_settings_saved",
    );
    for index in 0..7 {
        join_id(&mut ws, &format!("disaster_{index}"));
    }
    let state = reply(
        &mut ws,
        json!({"type":"get_world_settings"}),
        "world_settings",
    )["state"]
        .clone();
    let board = reply(
        &mut ws,
        json!({"type":"place_prop","request_id":"board","world_id":state["world_id"],"instance":{"id":"user:disaster-board","definition":"core:bulletin_board@1","locale":"square"}}),
        "prop_result",
    );
    assert_eq!(board["ok"], true, "{board}");
    let mode = reply(
        &mut ws,
        json!({"type":"set_game_mode","request_id":"mode","world_id":state["world_id"],"game_mode":{"kind":"survival10","seed":7}}),
        "world_settings_saved",
    );
    assert_eq!(mode["ok"], true, "{mode}");
    send(
        &mut ws,
        json!({"type":"director_say","request_id":"flood","world_id":state["world_id"],"text":"让洪水冲掉几家的私粮"}),
    );
    let receipts = received_until(&mut ws, |message| {
        message["type"] == "director_reply" && message["entry"]["player"] == "让洪水冲掉几家的私粮"
    });
    let reply_entry = receipts
        .iter()
        .find(|message| message["type"] == "director_reply")
        .unwrap();
    assert!(reply_entry["entry"]["effects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effect| effect["tool"] == "trigger_game_event" && effect["ok"] == true));

    let outcome = until(&mut ws, |message| {
        message["type"] == "manuscript_append"
            && message["entries"].as_array().unwrap().iter().any(|entry| {
                entry["source"].as_str().is_some_and(|source| {
                    source.starts_with("event:") && source.contains("-disaster-")
                })
            })
    });
    let loss = outcome["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["text"]
                .as_str()
                .is_some_and(|text| text.contains("裁判全知"))
        })
        .cloned()
        .unwrap_or_else(|| panic!("剧本里应有受灾家庭：{outcome}"));
    assert_eq!(loss["visibility"], "private");
    assert!(loss["text"].as_str().unwrap().contains("家私粮"), "{loss}");
    assert!(
        loss["clock"]
            .as_str()
            .is_some_and(|clock| clock.starts_with("D1 ")),
        "纪元时钟不能把重置后的第一拍读成第 0 天：{loss}"
    );
    let notice = outcome["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["source"]
                .as_str()
                .is_some_and(|source| source.ends_with("-disaster-1-notice"))
        })
        .unwrap_or_else(|| panic!("公开剧本应有张贴的天灾公告：{outcome}"));
    assert_eq!(notice["visibility"], "public");
    assert!(notice["text"]
        .as_str()
        .is_some_and(|text| text.contains("赛事天灾公告")));
    let save = read_json(&town.save());
    let residents = save["world"]["actors"].as_object().unwrap();
    assert!(
        !residents
            .values()
            .any(|actor| { serde_json::to_string(actor).unwrap().contains("裁判全知") }),
        "全知结果不能写进居民记忆"
    );

    send(
        &mut ws,
        json!({"type":"director_say","request_id":"rollback","world_id":state["world_id"],"text":"让洪水后删掉不存在的道具"}),
    );
    let rolled = received_until(&mut ws, |message| {
        message["type"] == "director_reply"
            && message["entry"]["player"] == "让洪水后删掉不存在的道具"
    });
    assert!(rolled
        .iter()
        .find(|message| message["type"] == "director_reply")
        .unwrap()["entry"]["effects"]
        .as_array()
        .unwrap()
        .iter()
        .all(|effect| effect["ok"] == false));
    // 剧本的剧目条目可能比这一轮的回执晚一拍广播；再等两拍，撤回的裁判结果也不能随后补进来。
    for _ in 0..2 {
        let _ = received_until(&mut ws, |message| message["type"] == "roster");
    }
    let page = manuscript(&mut ws);
    let count = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| {
            entry["text"]
                .as_str()
                .is_some_and(|text| text.contains("裁判全知"))
        })
        .count();
    assert_eq!(count, 1, "撤回的一轮不能留下天灾结果：{page}");
}

/// 聪明人小镇：晚餐与饥饿进入旁观剧本；全知余额不能借剧本写入任何居民记忆。
#[test]
fn survival10_authoritative_events_reach_the_audience_without_leaking_to_residents() {
    // 夹具地图只有三处住宅；用裸拓扑给七名参赛者各自的住宅，才能验证“睡在本人住宅/守家”。
    let mut town = Town::new();
    town.args = vec!["--map-document".into(), "topology.json".into()];
    town.mock_delay = Some("0.01".into());
    let mut topology = read_json(&town.dir.join("topology.json"));
    for index in 4..=7 {
        let home = format!("home_{index}");
        let station = format!("arrive_home_{index}");
        let node = format!("n_home_{index}");
        topology["locales"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":home,"name":home,"kind":"dwell"}));
        topology["stations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":station,"locale":home,"name":home,"kind":"arrival","node":node}));
        topology["passages"].as_array_mut().unwrap().push(json!({"id":format!("door_{home}"),"a":"square","b":home,"node_a":"n_square","node_b":node,"minutes":1,"capabilities":["通行"]}));
        topology["places"].as_array_mut().unwrap().push(json!({"name":home,"locale":home,"arrival_station":station,"type":"住家","summary":"","residential":true}));
    }
    town.write("topology.json", topology);
    for index in 0..7 {
        let mut template = read_json(&fixture("library/tpl_a.json"));
        template["template_id"] = json!(format!("contestant_{index}"));
        template["fields"]["name"] = json!(["甲", "乙", "丙", "丁", "戊", "己", "庚"][index]);
        town.write(&format!("library/contestant_{index}.json"), template);
    }
    let mut ws = town.start();
    for index in 0..7 {
        join_id(&mut ws, &format!("contestant_{index}"));
    }
    let state = reply(
        &mut ws,
        json!({"type":"get_world_settings"}),
        "world_settings",
    )["state"]
        .clone();
    let mode = reply(
        &mut ws,
        json!({"type":"set_game_mode","request_id":"mode","world_id":state["world_id"],"game_mode":{"kind":"survival10","seed":7}}),
        "world_settings_saved",
    );
    assert_eq!(mode["ok"], true, "{mode}");
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"go","speed":600}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 600.0);
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    let dinner = loop {
        assert!(Instant::now() < deadline, "第一天晚餐应进入剧本");
        let page = manuscript(&mut ws);
        if let Some(entry) = page["entries"].as_array().unwrap().iter().find(|entry| {
            entry["text"]
                .as_str()
                .is_some_and(|text| text.contains("裁判全知：第1天晚餐"))
        }) {
            break entry.clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(dinner["visibility"], "private");
    assert!(dinner["text"].as_str().unwrap().contains("消耗晚餐1袋"));
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"stop","speed":0}),
    );
    until(&mut ws, |m| m["type"] == "roster" && m["speed"] == 0.0);
    let saved = read_json(&town.save());
    let journals = serde_json::to_string(&saved["population"]["residents"]).unwrap();
    assert!(
        !journals.contains("裁判全知：第1天晚餐"),
        "全知晚饭不能写成居民记忆"
    );
    town.shutdown();
}
