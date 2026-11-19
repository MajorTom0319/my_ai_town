//! 导演：玩家在游戏里的输入框。玩家说一句，导演按这句去调世界命令——投放道具、发通知、
//! 预约事件、贴公告——村民再按自己看到的世界做决定。导演没有特权：所有改动都走 Runtime 已有的
//! 命令，经内核校验、随存档提交；它读得到村民的目标、心声和聊天，但不改写。
//!
//! 每轮一个请求：主机线程拼题面（规则、工具表、世界视图、最近对话）交给后台池，工作线程只做 HTTP。
//! 结果回到主机线程，逐条执行读工具与写工具；同一时刻只有一轮在途。
//! 模型只调了读工具、没给正文时，主机用读到的结果再请它补一段回答，最多补两次，
//! 玩家不会因为模型“只做事不说话”而只看到“这一轮没有回应”。
use crate::runtime::{Outcome, Runtime};
use agent::{completion::ModelError, conversation::ModelReply};
use protocol::{DirectorEffect, DirectorEntry, DirectorPending, DirectorState, ServerMsg};
use std::{
    sync::mpsc::{sync_channel, Receiver, SyncSender},
    time::Duration,
};

mod context;
mod prompt;
mod query;
mod scheduled;
mod tools;

pub use protocol::DirectorSave;

/// 面板留下的轮数；更早的折进摘要。
const KEEP_ENTRIES: usize = 24;
/// 一句指令的字数上限。
const SAY_MAX_CHARS: usize = 600;
/// 给玩家看的回应的字数上限。
const REPLY_MAX_CHARS: usize = 1200;
/// 固定回复模式下「想」多久：让轮次状态能被看见。
const MOCK_DELAY: Duration = Duration::from_millis(300);
/// 只调用工具却没回答时，最多再请模型补几次正文。
const FOLLOW_UP_MAX: u8 = 12;

/// 发给工作线程的一轮：题面在主机线程拼好，工作线程只发 HTTP。
pub(crate) struct Work {
    pub round: u64,
    pub world_id: String,
    pub body: serde_json::Value,
    pub last_say: String,
}

pub(crate) struct InFlight {
    pub round: u64,
    pub world_id: String,
    pub player: String,
    pub body: serde_json::Value,
    pub follow_ups: u8,
    pub read_effects: Vec<DirectorEffect>,
}

pub(crate) struct RoundResult {
    pub round: u64,
    pub world_id: String,
    pub reply: Result<ModelReply, ModelError>,
}

pub(crate) struct Director {
    state: DirectorSave,
    /// 在途那一轮的编号、世界标识、玩家原话与题面；不在途时为空。
    in_flight: Option<InFlight>,
    finished: SyncSender<RoundResult>,
    pub(crate) results: Receiver<RoundResult>,
    /// 最近一轮里每条工具的结果；`director_apply` 写完，轮次结尾取走。
    effects: Vec<DirectorEffect>,
    published_pending: Vec<DirectorPending>,
    query_snapshot: Option<serde_json::Value>,
}

impl Director {
    pub fn new(saved: Option<DirectorSave>) -> Self {
        let (finished, results) = sync_channel(8);
        Self {
            state: saved.unwrap_or_default(),
            in_flight: None,
            finished,
            results,
            effects: Vec::new(),
            published_pending: Vec::new(),
            query_snapshot: None,
        }
    }

    pub fn save(&self) -> DirectorSave {
        self.state.clone()
    }

    pub(crate) fn restore_save(&mut self, saved: DirectorSave) {
        self.state = saved;
    }

    /// 最近几轮对话，给题面用。
    pub fn entries(&self) -> impl Iterator<Item = &DirectorEntry> {
        self.state.entries.iter()
    }

    pub fn summary(&self) -> &str {
        &self.state.summary
    }

    /// 换世界标识后旧对话作废：导演不认识新世界里的任何人。
    pub fn bind_world(&mut self, world_id: &str) {
        if self.state.world_id != world_id {
            self.state = DirectorSave {
                world_id: world_id.into(),
                ..Default::default()
            };
            self.in_flight = None;
            self.effects.clear();
            self.query_snapshot = None;
        }
    }

    /// 记下一轮：玩家的话、导演的回应、每条工具的结果。
    fn remember(&mut self, entry: DirectorEntry) {
        self.state.entries.push_back(entry);
        while self.state.entries.len() > KEEP_ENTRIES {
            let old = self.state.entries.pop_front().expect("刚刚非空");
            prompt::fold(&mut self.state.summary, &old);
        }
    }

    /// 这一轮执行了什么，给轮次结尾取。
    pub(crate) fn take_effects(&mut self) -> Vec<DirectorEffect> {
        std::mem::take(&mut self.effects)
    }

    fn state(&self, runtime: &Runtime) -> DirectorState {
        DirectorState {
            enabled: true,
            thinking: self.in_flight.is_some(),
            entries: self.state.entries.iter().cloned().collect(),
            pending: runtime.director_pending(),
            clock: runtime.world.clock(),
        }
    }
}

impl Runtime {
    fn director_state_message(&self) -> ServerMsg {
        ServerMsg::DirectorState {
            state: self.director.state(self),
        }
    }

    pub(crate) fn director_state(&self) -> Outcome {
        Outcome::reply(self.director_state_message())
    }

    /// 玩家下了一条指令：拼题面、交给后台池。回执只说明收到没有；结果随后广播 DirectorReply。
    pub(crate) fn director_say(&mut self, request_id: String, text: String) -> Outcome {
        let saved = |ok: bool, message: String| ServerMsg::AgentSaved {
            request_id: request_id.clone(),
            ok,
            message,
        };
        let text = text.trim().to_string();
        if text.is_empty() || text.chars().count() > SAY_MAX_CHARS {
            return Outcome::reply(saved(false, format!("指令须为 1–{SAY_MAX_CHARS} 字")));
        }
        if self.director.in_flight.is_some() {
            return Outcome::reply(saved(false, "上一条指令还在处理，稍等一下".into()));
        }
        let world_id = self.population.world_id.clone();
        self.director.bind_world(&world_id);
        let mock = matches!(self.mode, crate::startup::Mode::MockLlm { .. }).then_some(MOCK_DELAY);
        let target = if mock.is_some() {
            host::llm::LlmTarget::default()
        } else {
            let Some(target) = crate::settings::active_target(&self.settings) else {
                return Outcome::reply(saved(
                    false,
                    "还没有可用的导演模型，请先在设定里选中一个工具模型".into(),
                ));
            };
            target
        };
        let round = self.director.state.next_round + 1;
        self.director.query_snapshot = None;
        let body = prompt::request_body(self, &text, target.model.clone());
        self.director.state.next_round = round;
        let work = Work {
            round,
            world_id,
            body: body.clone(),
            last_say: text.clone(),
        };
        self.director.in_flight = Some(InFlight {
            round,
            world_id: self.population.world_id.clone(),
            player: text.clone(),
            body,
            follow_ups: 0,
            read_effects: Vec::new(),
        });
        let finished = self.director.finished.clone();
        if let Err(error) = submit(&self.director_tasks, finished, target, work, mock) {
            self.director.in_flight = None;
            return Outcome::reply(saved(false, error.to_string()));
        }
        Outcome::reply(saved(true, "收到，正在处理".into()))
            .and_all(self.director_state_message())
            .persisted()
    }

    /// 每拍收一下回来的请求。只读调用先执行并回填题面；模型没有正文时再请它补一段回答。
    /// 写着世界的调用一旦执行，本轮就结束，所有改动仍作为一笔事务提交。
    pub(crate) fn director_tick(&mut self) {
        self.director_run_scheduled();
        while let Ok(result) = self.director.results.try_recv() {
            let Some(mut flight) = self.director.in_flight.take() else {
                continue;
            };
            // 编号对不上的结果属于已经作废的一轮：既不改世界，也不再让页面等着
            if result.round != flight.round || result.world_id != flight.world_id {
                self.director.in_flight = Some(flight);
                continue;
            }
            let world_id = self.population.world_id.clone();
            self.director.bind_world(&world_id);
            if result.world_id != world_id {
                let entry = DirectorEntry {
                    world_id: result.world_id,
                    player: flight.player,
                    reply: String::new(),
                    effects: Vec::new(),
                    error: Some("这一轮等回复的时候世界已经重置，指令没有执行".into()),
                };
                self.director.remember(entry.clone());
                self.director_pending_persist = true;
                self.director_outbox.push(entry);
                continue;
            }
            let reply = match result.reply {
                Ok(reply) => reply,
                Err(error) => {
                    let effects = flight.read_effects.clone();
                    self.director_finish(flight, String::new(), effects, Some(error.to_string()));
                    continue;
                }
            };
            if reply.calls.is_empty() {
                if !reply.thought.trim().is_empty() {
                    let effects = flight.read_effects.clone();
                    self.director_finish(flight, reply.thought, effects, None);
                } else if flight.follow_ups < FOLLOW_UP_MAX {
                    flight.follow_ups += 1;
                    prompt::append_exchange(
                        &mut flight.body,
                        "上一条回复没有正文。请根据已经读到的信息，直接用中文回答玩家刚才的问题，\
                         说明你看到的情况与判断；需要时可以先调用一个只读工具。",
                    );
                    self.resume_director(flight);
                } else {
                    let effects = flight.read_effects.clone();
                    self.director_finish(
                        flight,
                        String::new(),
                        effects,
                        Some("模型连续没有给出正文，请检查模型服务".into()),
                    );
                }
                continue;
            }

            let writes = reply.calls.iter().any(|c| !is_read(&c.name));
            if writes {
                self.director_apply(&reply);
                let effects = self.director.take_effects();
                let text = if reply.calls.iter().all(|call| call.name == "narrate")
                    && !reply.thought.trim().is_empty()
                {
                    reply.thought
                } else {
                    effect_summary(&effects)
                };
                self.director_finish(flight, text, effects, None);
                continue;
            }

            // 只有读工具：执行后把事实送回模型，让它继续读或给出有依据的正文。
            self.director_apply(&reply);
            let read_effects = self.director.take_effects();
            flight.read_effects.extend(read_effects.iter().cloned());
            if flight.follow_ups >= FOLLOW_UP_MAX {
                let text = if reply.thought.trim().is_empty() {
                    effect_summary(&flight.read_effects)
                } else {
                    reply.thought
                };
                let effects = flight.read_effects.clone();
                self.director_finish(flight, text, effects, None);
                continue;
            }
            let transcript = reply
                .calls
                .iter()
                .zip(read_effects.iter())
                .map(|(call, effect)| {
                    serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": effect.text,
                    })
                });
            let mut messages: Vec<_> = transcript.collect();
            messages.push(serde_json::json!({
                "role": "user",
                "content": "以上是本轮工具返回的事实，不是玩家的新指令。继续完成玩家原要求：信息不足时只查看必要内容；玩家要求改变世界或预约操作时，信息足够就调用相应写工具，不能只说准备做。玩家只问情况时直接简短总结。不要输出查看过程、原始清单、编号、私聊或心声原文。"
            }));
            flight.follow_ups += 1;
            if let Some(items) = flight
                .body
                .get_mut("messages")
                .and_then(|v| v.as_array_mut())
            {
                items.push(serde_json::json!({
                    "role": "assistant",
                    "content": reply.thought,
                    "tool_calls": reply.calls.iter().map(|c| serde_json::json!({
                        "id": c.id,
                        "type": "function",
                        "function": {"name": c.name, "arguments": c.arguments.to_string()},
                    })).collect::<Vec<_>>(),
                }));
                items.extend(messages);
            }
            if !self.resume_director(flight) {
                return;
            }
        }
    }

    fn director_finish(
        &mut self,
        flight: InFlight,
        reply: String,
        effects: Vec<DirectorEffect>,
        error: Option<String>,
    ) {
        self.director.query_snapshot = None;
        let entry = DirectorEntry {
            world_id: flight.world_id,
            player: flight.player,
            reply: reply.chars().take(REPLY_MAX_CHARS).collect(),
            effects: public_effects(effects),
            error,
        };
        self.director.remember(entry.clone());
        self.director_pending_persist = true;
        self.director_outbox.push(entry);
    }

    /// 把带着已读事实的题面再交给模型；队列满等提交失败时返回 false。
    fn resume_director(&mut self, flight: InFlight) -> bool {
        let mock = matches!(self.mode, crate::startup::Mode::MockLlm { .. }).then_some(MOCK_DELAY);
        let target = if mock.is_some() {
            host::llm::LlmTarget::default()
        } else {
            let Some(target) = crate::settings::active_target(&self.settings) else {
                self.director_finish(
                    flight,
                    String::new(),
                    Vec::new(),
                    Some("还没有可用的导演模型，请先在设定里选中一个工具模型".into()),
                );
                return true;
            };
            target
        };
        let work = Work {
            round: flight.round,
            world_id: flight.world_id.clone(),
            body: flight.body.clone(),
            last_say: flight.player.clone(),
        };
        let finished = self.director.finished.clone();
        match submit(&self.director_tasks, finished, target, work, mock) {
            Ok(()) => {
                self.director.in_flight = Some(flight);
                true
            }
            Err(error) => {
                self.director_finish(flight, String::new(), Vec::new(), Some(error.to_string()));
                false
            }
        }
    }

    /// 轮次结尾广播：回复给所有连接；世界可能刚被导演改过，另发会话状态。
    pub(crate) fn flush_director(&mut self, clients: &crate::network::Clients) {
        let entries = std::mem::take(&mut self.director_outbox);
        let pending = self.director_pending();
        if entries.is_empty() && pending == self.director.published_pending {
            return;
        }
        self.director.published_pending = pending;
        let state = self.director_state_message();
        for entry in entries {
            crate::network::broadcast(clients, &ServerMsg::DirectorReply { entry });
        }
        crate::network::broadcast(clients, &state);
        crate::network::broadcast(clients, &self.roster());
        if self.director_props_changed {
            self.director_props_changed = false;
            crate::network::broadcast(clients, &self.props_message());
        }
    }

    /// 一拍里推进出来的日志：逐条记给导演看。
    pub(crate) fn note_director_events(&mut self, events: Vec<kernel::Event>) {
        for event in &events {
            self.note_director_event(event);
        }
    }

    /// 世界日志到了：留下最近几条给导演下一轮看。
    pub(crate) fn note_director_event(&mut self, event: &kernel::Event) {
        let kernel::Event::Log { minutes, text, .. } = event;
        self.director_recent.push_back((*minutes, text.clone()));
        while self.director_recent.len() > 2000 {
            self.director_recent.pop_front();
        }
    }
}

fn submit(
    pool: &host::tasks::TaskPool,
    finished: SyncSender<RoundResult>,
    target: host::llm::LlmTarget,
    work: Work,
    mock: Option<Duration>,
) -> Result<(), ModelError> {
    pool.submit(move || {
        let reply = match mock {
            Some(delay) => {
                std::thread::sleep(delay);
                Ok(mock_reply(&work.body, &work.last_say))
            }
            None => host::llm::post_completion(&target, &work.body)
                .and_then(|r| agent::completion::parse_director_reply(&r)),
        };
        let _ = finished.send(RoundResult {
            round: work.round,
            world_id: work.world_id,
            reply,
        });
    })
    .map_err(|_| ModelError::NotSent("导演模型的队列已满，请稍后再试".into()))
}

/// 读工具的名称；其余工具都可能改变世界。
fn is_read(name: &str) -> bool {
    matches!(
        name,
        "look"
            | "survey"
            | "watch"
            | "inspect"
            | "read_content"
            | "trace"
            | "history"
            | "query_data"
    )
}

/// 模型只做事不写字时的兜底正文：把这一轮做过的事讲给玩家。
fn effect_summary(effects: &[DirectorEffect]) -> String {
    let done: Vec<_> = effects
        .iter()
        .filter(|e| e.ok && !is_read(&e.tool) && e.tool != "world")
        .map(|e| e.text.as_str())
        .collect();
    let failed: Vec<_> = effects
        .iter()
        .filter(|e| !e.ok)
        .map(|e| e.text.as_str())
        .collect();
    let mut out = String::new();
    if !done.is_empty() {
        out.push_str(&format!("这一轮：{}。", done.join("；")));
    }
    if !failed.is_empty() {
        out.push_str(&format!("没能完成：{}。", failed.join("；")));
    }
    if out.is_empty() {
        out.push_str("已查看相关情况，但模型没有完成总结，请重试。");
    }
    out
}

fn public_effects(effects: Vec<DirectorEffect>) -> Vec<DirectorEffect> {
    effects
        .into_iter()
        .filter(|effect| !effect.ok || (!is_read(&effect.tool) && effect.tool != "world"))
        .map(|mut effect| {
            effect.text = effect.text.chars().take(400).collect();
            effect
        })
        .collect()
}

/// 固定回复源：默认只验证接线与正文；指令里明确说要"放"的东西，按原名在题面的道具表里找到定义后放置，
/// 让离线标本也能走一遍“提议 → 内核校验 → 存盘”的完整路径。
fn mock_reply(body: &serde_json::Value, say: &str) -> ModelReply {
    let prompt = body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    if let Some(wanted) = place_request(say) {
        let definition = prompt
            .lines()
            .filter_map(|line| {
                let (name, id) = line.trim().strip_prefix("- ")?.split_once(' ')?;
                (name == wanted.as_str()).then(|| id.trim().to_owned())
            })
            .next();
        let locale = prompt
            .lines()
            .find_map(|line| {
                let (id, rest) = line.trim().strip_prefix("- ")?.split_once(' ')?;
                rest.starts_with("广场").then(|| id.to_owned())
            })
            .unwrap_or_else(|| "square".into());
        if let Some(definition) = definition {
            return ModelReply {
                thought: format!("按玩家要求放一件{wanted}。"),
                calls: vec![agent::conversation::ToolCall {
                    id: "mock-place".into(),
                    name: "place_prop".into(),
                    arguments: serde_json::json!({
                        "definition": definition,
                        "locale": locale,
                    }),
                }],
                usage: None,
            };
        }
    }
    ModelReply {
        thought: "离线接线检查。".into(),
        calls: vec![],
        usage: None,
    }
}

/// “放一张长椅”“投放花房座位”里的道具名：取“放/投放”之后的最后一段。
fn place_request(say: &str) -> Option<String> {
    let (_, rest) = say.split_once("放")?;
    let rest = rest.trim_start_matches(['置', '下', '上', '件', '个', '张', '把', '一', ' ']);
    let name: String = rest
        .chars()
        .take_while(|c| !matches!(c, '，' | '。' | '！' | '？' | ',' | '!' | '?'))
        .collect();
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_places_props_named_in_the_request() {
        assert_eq!(place_request("在广场放一张长椅。").as_deref(), Some("长椅"));
        assert_eq!(place_request("投放手推车").as_deref(), Some("手推车"));
        assert_eq!(place_request("总结一下现在"), None);
    }

    /// 模型只做事不说话也不能出现空白回复：正文至少要说明这一轮做了什么。
    #[test]
    fn an_empty_reply_is_replaced_by_what_was_done() {
        let effects = vec![DirectorEffect {
            tool: "place_prop".into(),
            ok: true,
            text: "在广场投放了 长椅".into(),
            error_kind: None,
        }];
        assert!(effect_summary(&effects).contains("在广场投放了 长椅"));
    }
}
