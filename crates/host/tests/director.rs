use crate::support::*;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tungstenite::WebSocket;

struct Endpoint {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Endpoint {
    fn new(replies: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
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
                let message = replies.pop_front().expect("不应再次调用模型来执行预约");
                let body = json!({"choices":[{"message":message}]}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        Self {
            url,
            stop,
            thread: Some(thread),
        }
    }

    fn town(&self) -> Town {
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
