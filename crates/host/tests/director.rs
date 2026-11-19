use crate::support::*;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tungstenite::WebSocket;

pub(crate) struct Endpoint {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Endpoint {
    pub(crate) fn new(replies: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let thread = std::thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            while !stopping.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut block = [0; 4096];
                loop {
                    let count = stream.read(&mut block).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&block[..count]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let length: usize = String::from_utf8_lossy(&bytes[..end])
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|length| length.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let start = bytes
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .unwrap()
                    + 4;
                received
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes[start..]).unwrap());
                let message = replies.pop_front().expect("不应再次调用模型来执行预约");
                let body = json!({"choices":[{"message":message}]}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        Self {
            url,
            stop,
            thread: Some(thread),
            requests,
        }
    }

    pub(crate) fn town(&self) -> Town {
        let mut town = Town::new();
        town.mock_delay = None;
        town.args = map_args(MAP_2D);
        town.write(
            "config/providers.json",
            json!({
                "active":{"provider":"fixture","model":"director"},
                "providers":[{"id":"fixture","name":"测试模型","base_url":self.url}]
            }),
        );
        town
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn message(calls: Vec<Value>) -> Value {
    json!({"role":"assistant","content":"查看过程和原始清单不该展示给玩家。","tool_calls":calls})
}

fn call(name: &str, arguments: Value) -> Value {
    json!({"id":name,"type":"function","function":{"name":name,"arguments":arguments.to_string()}})
}

fn schedule(text: &str, delay: f64, action: &str, arguments: Value) -> Value {
    call(
        "schedule",
        json!({"text":text,"after_minutes":delay,"action":action,"arguments":arguments}),
    )
}

fn say(ws: &mut WebSocket<TcpStream>, text: &str) -> Value {
    let state = reply(ws, json!({"type":"get_world_settings"}), "world_settings")["state"].clone();
    let received = reply(
        ws,
        json!({"type":"director_say","request_id":text,"world_id":state["world_id"],"text":text}),
        "agent_saved",
    );
    assert_eq!(received["ok"], true, "{received}");
    until(ws, |message| {
        message["type"] == "director_reply" && message["entry"]["player"] == text
    })["entry"]
        .clone()
}

fn pending(ws: &mut WebSocket<TcpStream>) -> Value {
    reply(ws, json!({"type":"get_director_state"}), "director_state")["state"]["pending"].clone()
}

#[test]
fn director_queries_authoritative_data_with_paging_and_without_configuration_access() {
    let endpoint = Endpoint::new(vec![
        message(vec![
            call("query_data", json!({"path":""})),
            call("query_data", json!({"path":"/residents","limit":80})),
            call("query_data", json!({"path":"/world/actors"})),
            call("inspect", json!({"actor":"阿甲"})),
        ]),
        message(vec![
            call("query_data", json!({"path":"/residents","offset":80})),
            call("query_data", json!({"path":"/agents"})),
            call("query_data", json!({"path":"/settings"})),
            call("query_data", json!({"path":"/topology/props","limit":80})),
        ]),
        json!({"role":"assistant","content":"已核实居民身体、目标、人设与世界规则。"}),
        message(vec![
            call("query_data", json!({"path":"/settings/credentials"})),
            call("query_data", json!({"path":"/agents/limits"})),
            call(
                "query_data",
                json!({"path":"/world/../config/credentials.json"}),
            ),
        ]),
        json!({"role":"assistant","content":"不能读取应用密钥与模型配置。"}),
    ]);
    let mut town = endpoint.town();
    town.write(
        "config/credentials.json",
        json!({"fixture":"sk-director-query-test-secret"}),
    );
    let mut ws = town.start();
    text_only_world(&mut ws);
    let actor = join_id(&mut ws, "tpl_a");
    let before = read_json(&town.save());
    let entry = say(&mut ws, "查看完整居民后台数据和世界规则");
    assert_eq!(entry["effects"], json!([]));
    let messages = endpoint.requests.lock().unwrap()[2]["messages"].clone();
    let effects: Vec<_> = messages
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| json!({"text":message["content"]}))
        .collect();
    assert_eq!(effects.len(), 8);
    let page = |index: usize| -> Value {
        serde_json::from_str(effects[index]["text"].as_str().unwrap()).unwrap()
    };
    let first = page(1);
    let second = page(4);
    assert_eq!(first["next_offset"], 80);
    assert_eq!(second["next_offset"], Value::Null);
    assert_eq!(first["snapshot_minutes"], second["snapshot_minutes"]);
    let residents: Value = serde_json::from_str(&format!(
        "{}{}",
        first["data"].as_str().unwrap(),
        second["data"].as_str().unwrap()
    ))
    .unwrap();
    assert_eq!(
        residents[&actor]["persona"],
        before["population"]["residents"][&actor]["persona"]
    );
    assert_eq!(residents[&actor]["name"], "阿甲");
    assert!(residents[&actor].get("model_binding").is_none());
    let actors: Value = serde_json::from_str(page(2)["data"].as_str().unwrap()).unwrap();
    assert!(actors[&actor]["needs"]["health"]["value"].is_number());
    assert!(effects[3]["text"].as_str().unwrap().contains("目标："));
    let agents: Value = serde_json::from_str(page(5)["data"].as_str().unwrap()).unwrap();
    assert!(agents[&actor]["goals"].is_object());
    assert!(agents[&actor]["memory"].is_object());
    let settings: Value = serde_json::from_str(page(6)["data"].as_str().unwrap()).unwrap();
    assert_eq!(settings["worldview"], before["world_settings"]["worldview"]);
    let facts = serde_json::to_string(&effects).unwrap();
    assert!(!facts.contains(&endpoint.url));
    assert!(!facts.contains("sk-director-query-test-secret"));
    assert_eq!(read_json(&town.save())["world"], before["world"]);
    let denied = say(&mut ws, "尝试读取应用密钥和模型绑定");
    assert!(denied["effects"]
        .as_array()
        .unwrap()
        .iter()
        .all(|effect| effect["ok"] == false));
    let requests = endpoint.requests.lock().unwrap();
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "query_data"));
    assert!(requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("snapshot_minutes"))));
}

#[test]
fn survival10_disasters_are_transactional_and_scheduled_once_after_restart() {
    let event = json!({"kind":"survival10_disaster","event":{"kind":"rats"}});
    let endpoint = Endpoint::new(vec![
        message(vec![
            call("trigger_game_event", event.clone()),
            call("remove_prop", json!({"id":"missing-prop"})),
        ]),
        message(vec![
            call("trigger_game_event", event.clone()),
            schedule(
                "洪涝预约",
                60.0,
                "trigger_game_event",
                json!({"kind":"survival10_disaster","event":{"kind":"flood"}}),
            ),
        ]),
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
    text_only_world(&mut ws);
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
    let before = read_json(&town.save())["world"]["game_mode"].clone();
    let rejected = say(&mut ws, "鼠患后执行错误操作，必须回滚");
    assert!(rejected["effects"]
        .as_array()
        .unwrap()
        .iter()
        .all(|effect| effect["ok"] == false));
    assert_eq!(read_json(&town.save())["world"]["game_mode"], before);
    let applied = say(&mut ws, "鼠患并预约洪涝");
    assert!(
        applied["effects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|effect| effect["ok"] == true),
        "{applied}"
    );
    assert_eq!(pending(&mut ws).as_array().unwrap().len(), 1);
    town.kill();
    drop(ws);
    let mut saved = read_json(&town.save());
    saved["director"]["scheduled"][0]["at"] = saved["world"]["minutes"].clone();
    std::fs::write(town.save(), serde_json::to_vec(&saved).unwrap()).unwrap();
    let mut ws = town.start();
    let state = reply(
        &mut ws,
        json!({"type":"get_director_state"}),
        "director_state",
    );
    let executed = state["state"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["player"]
                .as_str()
                .is_some_and(|text| text.contains("预约执行") && text.contains("洪涝预约"))
        })
        .unwrap();
    assert_eq!(executed["effects"][0]["ok"], true, "{executed}");
    let saved = read_json(&town.save());
    assert_eq!(
        saved["world"]["game_mode"]["survival10"]["disasters"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(pending(&mut ws).as_array().unwrap().is_empty());
    town.kill();
    drop(ws);
    let _ws = town.start();
    assert_eq!(
        read_json(&town.save())["world"]["game_mode"],
        saved["world"]["game_mode"]
    );
}

#[test]
fn supply_adjustments_are_transactional_and_scheduled_against_live_inventory_after_restart() {
    let id = "director:prop-1";
    let endpoint = Endpoint::new(vec![
        message(vec![
            call(
                "place_prop",
                json!({"definition":"test:water", "locale":"square"}),
            ),
            call(
                "adjust_supply",
                json!({"id":id,"operation":"set","quantity":9,"reason":"设置事件库存"}),
            ),
            schedule(
                "之后减半",
                6.0,
                "adjust_supply",
                json!({"id":id,"operation":"halve","reason":"预约损失一半"}),
            ),
        ]),
        message(vec![call(
            "adjust_supply",
            json!({"id":id,"operation":"subtract","quantity":2,"reason":"先损失两份"}),
        )]),
        message(vec![
            call(
                "adjust_supply",
                json!({"id":id,"operation":"set","quantity":1,"reason":"应整体撤回"}),
            ),
            call("remove_prop", json!({"id":"missing-prop"})),
        ]),
    ]);
    let mut town = endpoint.town();
    let mut ws = town.start();
    let entry = say(&mut ws, "放桶水，设为九份并预约减半");
    assert!(
        entry["effects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|effect| effect["ok"] == true),
        "{entry}"
    );
    let key = format!("{id}/drink");
    assert_eq!(read_json(&town.save())["world"]["supplies"][&key], 9);
    let entry = say(&mut ws, "先减少两份");
    assert!(
        entry["effects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|effect| effect["ok"] == true),
        "{entry}"
    );
    assert_eq!(read_json(&town.save())["world"]["supplies"][&key], 7);
    let entry = say(&mut ws, "修改后移除不存在的道具");
    assert!(
        entry["effects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|effect| effect["ok"] == false),
        "{entry}"
    );
    assert_eq!(read_json(&town.save())["world"]["supplies"][&key], 7);
    town.kill();
    drop(ws);
    let mut ws = town.start();
    assert_eq!(pending(&mut ws).as_array().unwrap().len(), 1);
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"run-supply-event","speed":10}),
    );
    let entry = until(&mut ws, |message| {
        message["type"] == "director_reply"
            && message["entry"]["player"]
                .as_str()
                .is_some_and(|player| player.contains("之后减半"))
    })["entry"]
        .clone();
    assert_eq!(entry["effects"][0]["ok"], true, "{entry}");
    assert!(entry["effects"][0]["text"]
        .as_str()
        .unwrap()
        .contains("7→3"));
    assert_eq!(read_json(&town.save())["world"]["supplies"][&key], 3);
    assert_eq!(pending(&mut ws), json!([]));
}

#[test]
fn scheduled_weather_survives_restart_and_save_failure_without_repeating_or_exposing_reads() {
    let endpoint = Endpoint::new(vec![
        message(vec![call("look", json!({}))]),
        message(vec![schedule(
            "下雨",
            6.0,
            "set_weather",
            json!({"mode":"fixed","kind":"rain"}),
        )]),
    ]);
    let mut town = endpoint.town();
    let mut ws = town.start();
    let entry = say(&mut ws, "查看情况后预约下雨");
    assert_eq!(entry["effects"].as_array().unwrap().len(), 1, "{entry}");
    assert_eq!(entry["effects"][0]["tool"], "schedule");
    assert!(!entry.to_string().contains("原始清单"), "{entry}");
    assert_eq!(pending(&mut ws).as_array().unwrap().len(), 1);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(pending(&mut ws).as_array().unwrap().len(), 1);
    town.kill();
    drop(ws);
    let mut ws = town.start();
    assert_eq!(pending(&mut ws).as_array().unwrap().len(), 1);
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"start","speed":10}),
    );
    until(&mut ws, |message| {
        message["type"] == "roster" && message["speed"] == 10.0
    });
    let blocked = town.save().with_file_name("world.json.tmp");
    std::fs::create_dir(&blocked).unwrap();
    until(&mut ws, |message| {
        message["type"] == "roster"
            && message["sim_minutes"]
                .as_f64()
                .is_some_and(|minutes| minutes >= 426.0)
    });
    let state = reply(
        &mut ws,
        json!({"type":"get_director_state"}),
        "director_state",
    )["state"]
        .clone();
    assert_eq!(state["pending"].as_array().unwrap().len(), 1, "{state}");
    let roster = until(&mut ws, |message| message["type"] == "roster");
    assert_ne!(roster["environment"]["kind"], "rain", "{roster}");
    std::fs::remove_dir(blocked).unwrap();
    let executed = until(&mut ws, |message| {
        message["type"] == "director_reply"
            && message["entry"]["player"]
                .as_str()
                .is_some_and(|text| text.starts_with("预约执行"))
    });
    assert_eq!(executed["entry"]["effects"][0]["ok"], true, "{executed}");
    assert_eq!(pending(&mut ws), json!([]));
    let saved = town.saved(|saved| {
        saved["director"]["entries"]
            .as_array()
            .is_some_and(|entries| entries.len() == 2)
    });
    assert_eq!(saved["director"]["scheduled"], Value::Null);
    town.kill();
    drop(ws);
    let mut ws = town.start();
    let state = reply(
        &mut ws,
        json!({"type":"get_director_state"}),
        "director_state",
    )["state"]
        .clone();
    assert_eq!(state["entries"].as_array().unwrap().len(), 2);
    assert_eq!(state["pending"], json!([]));
    assert_eq!(
        until(&mut ws, |message| message["type"] == "roster")["environment"]["kind"],
        "rain"
    );
}

#[test]
fn rejected_director_round_retracts_world_changes_and_new_reservations() {
    let endpoint = Endpoint::new(vec![
        message(vec![
            schedule(
                "之后下雪",
                1.0,
                "set_weather",
                json!({"mode":"fixed","kind":"snow"}),
            ),
            call("set_weather", json!({"mode":"fixed","kind":"rain"})),
            call("remove_prop", json!({"id":"missing-prop"})),
        ]),
        message(vec![
            schedule("移除道具", 1.0, "remove_prop", json!({"id":"missing-prop"})),
            schedule(
                "下雨",
                2.0,
                "set_weather",
                json!({"mode":"fixed","kind":"rain"}),
            ),
            schedule(
                "下雪",
                3.0,
                "set_weather",
                json!({"mode":"fixed","kind":"snow"}),
            ),
        ]),
    ]);
    let mut town = endpoint.town();
    let mut ws = town.start();
    let entry = say(&mut ws, "安排下雪、改天气并移除不存在的道具");
    assert!(
        entry["effects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|effect| effect["ok"] == false),
        "{entry}"
    );
    assert!(entry["reply"].as_str().unwrap().contains("撤回"));
    assert_eq!(pending(&mut ws), json!([]));
    assert_eq!(
        until(&mut ws, |message| message["type"] == "roster")["environment"]["kind"],
        "clear"
    );
    town.kill();
    drop(ws);
    let mut ws = town.start();
    assert_eq!(pending(&mut ws), json!([]));
    assert_eq!(
        until(&mut ws, |message| message["type"] == "roster")["environment"]["kind"],
        "clear"
    );
    say(&mut ws, "预约移除道具并下雨");
    send(
        &mut ws,
        json!({"type":"set_speed","request_id":"start","speed":10}),
    );
    let failed = until(&mut ws, |message| {
        message["type"] == "director_reply"
            && message["entry"]["player"]
                .as_str()
                .is_some_and(|text| text.starts_with("预约执行"))
    });
    assert_eq!(failed["entry"]["effects"][0]["ok"], false, "{failed}");
    let succeeded = until(&mut ws, |message| {
        message["type"] == "director_reply"
            && message["entry"]["effects"][0]["tool"] == "set_weather"
    });
    assert_eq!(succeeded["entry"]["effects"][0]["ok"], true, "{succeeded}");
    let snow = until(&mut ws, |message| {
        message["type"] == "director_reply"
            && message["entry"]["player"]
                .as_str()
                .is_some_and(|text| text.ends_with("下雪"))
    });
    assert_eq!(snow["entry"]["effects"][0]["ok"], true, "{snow}");
    assert_eq!(
        until(&mut ws, |message| message["type"] == "roster")["environment"]["kind"],
        "snow"
    );
    assert_eq!(pending(&mut ws), json!([]));
}
