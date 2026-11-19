//! 应用的命令分派与单拍顺序。保持命令 → 推进 → 模型结果 → roster → 角色卡细节的时序。
//! 客户端命令不能让进程退出：存盘失败只回错误回执并写日志，内存状态保留，下次检查点重试。
//! 命令和事务当场存盘；世界自己往前走的进度（含模型回复）到点才存，写盘在后台线程。
use crate::{
    join::JoinRequest,
    network::{broadcast, broadcast_event, send_to, Clients},
    resident_driver::ResidentDriver,
    settings::Settings,
    startup::WorldSetup,
    transaction::Abort,
};
use agent::AgentError;
use host::{
    library::Library, population::Population, tasks::TaskPool, world_settings::valid_speed,
    HostError,
};
use kernel::World;
use protocol::{Alert, ClientMsg, ContentCommand, EventInfo, ServerMsg, WeatherCode};

pub(crate) struct Runtime {
    /// 建新世界的地图数据与规则；重置时用它再建一次。
    pub setup: WorldSetup,
    pub avatar_control: crate::avatar::Control,
    pub world_settings: host::world_settings::WorldSettings,
    pub world: World,
    /// 玩家选定的速度：正常速度（见 base_pace）的倍数，0 为暂停，界面允许输入自定义倍数；随存档保存。
    pub speed: f64,
    /// 这一拍实际的推进速度（模拟分钟 / 现实秒）：大家都在做一件还要好一阵的事时比正常速度快
    /// （自动快进），不存档。
    pub pace: f64,
    pub driver: ResidentDriver,
    pub settings: Settings,
    pub store: Option<host::persistence::SaveStore>,
    pub population: Population,
    pub library: Library,
    pub prop_library: host::props::PropLibrary,
    /// 连接测试、模板生成这类一次性模型调用的有界池。
    pub tasks: TaskPool,
    /// 上一次存盘是否失败；只在状态变化时写日志，避免每秒刷屏。
    pub save_failed: bool,
    /// 录像：广播都过它一遍（见 host::recording）；和网络层共用一份
    pub tape: host::recording::Tape,
    /// 开新一卷录像要的小镇目录与地图；世界不存档时为空
    pub reels: Option<crate::recording_commands::ReelSource>,
    /// 实时剧本：世界确认的台词、心声和结果按世界追加到小镇目录，并增量广播。
    pub manuscript: crate::manuscript::Manuscript,
    /// 小镇目录；不存档时为空
    pub manuscript_root: Option<std::path::PathBuf>,
    /// 导演会话与在途轮次。
    pub director: crate::conductor::Director,
    /// 导演模型请求单独一个池：村民忙的时候输入框也能用。
    pub director_tasks: TaskPool,
    /// 这一拍要广播的导演轮次。
    pub director_outbox: Vec<protocol::DirectorEntry>,
    /// 上一拍写进剧本、还没广播的条目：导演这一轮失败撤回时，页面不该先看到裁判全知结果。
    pub manuscript_pending: Vec<protocol::ManuscriptEntry>,
    /// 剧情概览的整理任务：一次只发一个，不碰世界。请求发生在模型池里，结果下一拍收回。
    pub summary_state: crate::manuscript::SummaryState,
    /// 自动整理的节流：下一次允许开始的现实时刻与那次开始时写到的编号。
    /// 记录没变、或离上次开始太近时都不再调用模型；手动整理不受它影响。
    pub summary_wait: std::time::Instant,
    /// 自动整理的门槛：攒够这么多条新实录就整理一次；固定回复测试里调小，免得跑很多模拟分钟。
    pub summary_batch: usize,
    /// 自动整理后至少等这么久才能再发起，避免快进时连续调用模型。
    pub auto_summary_gap: std::time::Duration,
    /// 导演事务里产生的世界日志，这一拍收尾时一起进剧本。
    pub director_events: Vec<kernel::Event>,
    /// 最近的世界日志，导演每轮看到的那一段。
    pub director_recent: std::collections::VecDeque<(f64, String)>,
    /// 这一拍收过导演轮次，存档时带上会话。
    pub director_pending_persist: bool,
    /// 导演放的或移除的道具，这一拍结束要广播道具目录。
    pub director_props_changed: bool,
    /// 启动模式：固定回复、正式模型。
    pub mode: crate::startup::Mode,
    /// 各连接在看哪位村民的角色卡细节（见 resident_detail.rs）
    pub watchers: crate::resident_detail::Watchers,
}

/// 一条消息发给谁。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum To {
    /// 发出命令的连接：命令的回执、查询结果与错误。
    Sender,
    /// 所有连接：命令改动后的世界与设定状态。
    All,
}

/// 一条命令的处理结果：按顺序要发出的消息与各自发给谁，以及是否改动了需要存盘的状态。
/// 自己完成存盘（含回滚）的事务性命令不标存盘。
pub(crate) struct Outcome {
    pub messages: Vec<(To, ServerMsg)>,
    pub persist: bool,
}

impl Outcome {
    /// 不发消息。
    pub fn none() -> Self {
        Self {
            messages: Vec::new(),
            persist: false,
        }
    }
    /// 一条只回给发出者的消息。
    pub fn reply(message: ServerMsg) -> Self {
        Self::none().and_reply(message)
    }
    /// 后面再回给发出者一条。
    pub fn and_reply(mut self, message: ServerMsg) -> Self {
        self.messages.push((To::Sender, message));
        self
    }
    /// 后面再广播一条。
    pub fn and_all(mut self, message: ServerMsg) -> Self {
        self.messages.push((To::All, message));
        self
    }
    /// 改动了需要存盘的状态，发完消息后存盘。
    pub fn persisted(mut self) -> Self {
        self.persist = true;
        self
    }
}

/// 通用错误；发给发出者时由 `Runtime::command` 填上引起它的命令编号。
pub(crate) fn error_msg(message: String) -> ServerMsg {
    ServerMsg::Error {
        request_id: None,
        message,
    }
}

impl Runtime {
    /// 剧情概览：把新实录交给单独的整理任务。任务没有工具，也不能改世界；模型不可用时只回错误。
    /// 立即回执和稍后回来的结果都带着这次请求的编号，页面据此配对，不把自动整理当成自己的。
    fn summarize_manuscript(&mut self, world_id: &str, request_id: &str) -> Outcome {
        let current = self.manuscript.summary().cloned();
        let reply = |error: Option<String>| {
            Outcome::reply(ServerMsg::ManuscriptSummary {
                summary: current.clone(),
                error,
                request_id: Some(request_id.to_owned()),
                generated: false,
            })
        };
        if world_id != self.population.world_id {
            return reply(Some("世界已重置，请刷新页面后重试".into()));
        }
        if self.summary_state.busy() {
            return reply(Some("剧情概览正在整理，稍等一下".into()));
        }
        let entries = self.manuscript.summary_lines(self.manuscript.summary());
        let Ok((prompt, through)) =
            crate::manuscript::summary_prompt(&entries, self.manuscript.summary())
        else {
            // 在途的自动整理已经把这段记进概览时，手动请求没有新素材；
            // 这不是失败，不该用错误回执把已经生效的概览标成未更新。
            if self.manuscript.summary().is_some() {
                return reply(None);
            }
            return reply(Some("还没有可整理的实录".into()));
        };
        match self.start_summary(prompt, through, Some(request_id.to_owned())) {
            Ok(()) => Outcome::reply(ServerMsg::ManuscriptSummary {
                summary: self.manuscript.summary().cloned(),
                error: None,
                request_id: Some(request_id.to_owned()),
                generated: false,
            }),
            Err(error) => reply(Some(error.into())),
        }
    }

    /// 发起一次概览整理（手动和自动共用）；模型不可用、任务池满时返回原因，不阻塞世界。
    fn start_summary(
        &mut self,
        prompt: String,
        through: u64,
        request_id: Option<String>,
    ) -> Result<(), &'static str> {
        let mock = matches!(self.mode, crate::startup::Mode::MockLlm { .. });
        let target = if mock {
            host::llm::LlmTarget::default()
        } else {
            let Some(target) = crate::settings::active_target(&self.settings) else {
                return Err("还没有可用的工具模型，请先在设定里选中一个");
            };
            target
        };
        let body = crate::manuscript::summary_body(&target.model, &prompt);
        self.summary_state.start(
            &self.tasks,
            crate::manuscript::SummaryJob {
                world: self.population.world_id.clone(),
                through,
                target,
                body,
                mock,
                request_id,
            },
        )
    }

    fn poll_manuscript_summary(&mut self, clients: &Clients) {
        let Some(result) = self.summary_state.poll(&self.population.world_id) else {
            return;
        };
        let request_id = result.request_id;
        let automatic = request_id.is_none();
        if automatic {
            self.manuscript.finish_auto();
        }
        let message = match result.reply.and_then(|text| {
            let response = serde_json::json!({"choices":[{"message":{"content":text}}]});
            crate::manuscript::parse_summary(&response)
        }) {
            Ok((situation, conflicts, promises, questions)) => ServerMsg::ManuscriptSummary {
                summary: self.manuscript.summarize(
                    situation,
                    conflicts,
                    promises,
                    questions,
                    result.through,
                ),
                error: None,
                request_id: request_id.clone(),
                generated: true,
            },
            Err(error) => {
                if automatic {
                    self.manuscript.retry_auto();
                    self.summary_wait =
                        std::time::Instant::now() + std::time::Duration::from_secs(30);
                }
                // 失败只回原因；上一版概览留在 Manuscript 里，不能拿一个错误的空 summary 把它盖掉。
                let error = format!("概览暂未更新：{error}");
                // 自动整理失败只记日志，不把技术错误推给正在读剧本的旁观页面。
                if request_id.is_none() {
                    eprintln!("[host] 自动整理{error}");
                    return;
                }
                ServerMsg::ManuscriptSummary {
                    summary: self.manuscript.summary().cloned(),
                    error: Some(error),
                    request_id,
                    generated: false,
                }
            }
        };
        broadcast(clients, &message);
    }

    /// 自动整理：只在世界产生了新实录时，现实时间节流后发起；同一世界一次只发一个请求。
    /// 模型不可用或失败只记日志、等一会儿再试，不暂停世界，也不影响村民的决策。
    fn auto_summarize_manuscript(&mut self) {
        const RETRY: std::time::Duration = std::time::Duration::from_secs(30);
        // 关掉录像的世界不保存剧本文本，也不该为它多调一次模型。
        if !self.world_settings.recording || !self.world_settings.auto_summary {
            return;
        }
        let now = std::time::Instant::now();
        let through = self.manuscript.summarized_through().unwrap_or(0);
        // 对话、投票这类玩法事实可能不经世界日志；新条目要先过一段，免得每走一步就调模型。
        let due = self.manuscript.auto_due(through, self.summary_batch);
        // 另一个整理（手动或自动）还在时也不排队，更不把这一批记成已整理。
        if self.summary_state.busy() || now < self.summary_wait {
            return;
        }
        if !due {
            return;
        }
        let entries = self.manuscript.summary_lines(self.manuscript.summary());
        let Ok((prompt, through)) =
            crate::manuscript::summary_prompt(&entries, self.manuscript.summary())
        else {
            return;
        };
        match self.start_summary(prompt, through, None) {
            Ok(()) => {
                self.manuscript.accepted_auto();
                self.summary_wait = now + self.auto_summary_gap;
            }
            Err(error) => {
                self.manuscript.retry_auto();
                self.summary_wait = now + RETRY;
                eprintln!("[host] 自动整理剧情概览暂未开始：{error}");
            }
        }
    }

    /// 执行一条来自 `connection` 的命令：回执、查询结果与错误只发给它，状态变化广播给所有连接。
    pub(crate) fn command(&mut self, connection: u64, msg: ClientMsg, clients: &Clients) {
        let request_id = msg.request_id().map(str::to_owned);
        let outcome = match self.gate(&msg) {
            Some(rejection) => Outcome::reply(rejection),
            None => match msg {
                ClientMsg::DirectorSay {
                    request_id, text, ..
                } => self.director_say(request_id, text),
                other => self.dispatch(other, connection, clients),
            },
        };
        let reply = |message: &mut ServerMsg| {
            if let ServerMsg::Error { request_id: id, .. } = message {
                if id.is_none() {
                    id.clone_from(&request_id);
                }
            }
            send_to(clients, connection, message);
        };
        for (to, mut message) in outcome.messages {
            match to {
                To::Sender => reply(&mut message),
                To::All => broadcast(clients, &message),
            }
        }
        if outcome.persist {
            if let Err(error) = self.checkpoint(true) {
                reply(&mut error_msg(kept_in_memory(&error)));
            }
        }
    }

    /// 世界标识与就绪门槛统一在这里：命令自带 world_id 的必须对上当前世界；改世界的命令必须先设世界观。
    fn gate(&self, msg: &ClientMsg) -> Option<ServerMsg> {
        if world_id_of(msg).is_some_and(|id| id != self.population.world_id) {
            return Some(self.rejection(msg, "世界已重置，请刷新页面后重试"));
        }
        if self
            .world_settings
            .prefab_setup
            .as_ref()
            .is_some_and(|s| !s.complete())
            && matches!(
                msg,
                ClientMsg::SetWorldview { .. }
                    | ClientMsg::SetPhysiology { .. }
                    | ClientMsg::SetClimate { .. }
                    | ClientMsg::SetGameMode { .. }
                    | ClientMsg::SetStrangers { .. }
                    | ClientMsg::SetAgentLimits { .. }
                    | ClientMsg::SetPersona { .. }
                    | ClientMsg::SetResidentModel { .. }
            )
        {
            return Some(self.rejection(msg, "预制开局尚未完成，请先继续开局设置"));
        }
        if requires_ready(msg) && !self.world_settings.ready() {
            return Some(self.rejection(msg, "请先设置世界观"));
        }
        None
    }

    /// 按命令自己的回执类型退回；没有专用回执的用通用错误。
    pub(crate) fn rejection(&self, msg: &ClientMsg, message: &str) -> ServerMsg {
        let message = message.to_string();
        match msg {
            ClientMsg::SetWeather {
                request_id,
                world_id,
                ..
            } => {
                let code = if world_id != &self.population.world_id {
                    WeatherCode::WorldMismatch
                } else {
                    WeatherCode::WorldNotReady
                };
                self.weather_failed(request_id.clone(), code, message)
            }
            ClientMsg::BeginPrefabSetup { request_id, .. }
            | ClientMsg::AdvancePrefabSetup { request_id, .. } => ServerMsg::PrefabSetupSaved {
                request_id: request_id.clone(),
                ok: false,
                message,
                state: self.world_settings.prefab_setup.clone(),
            },
            ClientMsg::SetWorldview { request_id, .. }
            | ClientMsg::SetPhysiology { request_id, .. }
            | ClientMsg::SetMiracles { request_id, .. }
            | ClientMsg::SetClimate { request_id, .. }
            | ClientMsg::SetRecording { request_id, .. }
            | ClientMsg::DeleteRecording { request_id, .. }
            | ClientMsg::SetStrangers { request_id, .. }
            | ClientMsg::SetGameMode { request_id, .. }
            | ClientMsg::SetAutoSummary { request_id, .. }
            | ClientMsg::GameModeControl { request_id, .. } => ServerMsg::WorldSettingsSaved {
                request_id: request_id.clone(),
                ok: false,
                message,
            },
            ClientMsg::SummarizeManuscript { request_id, .. } => ServerMsg::ManuscriptSummary {
                summary: self.manuscript.summary().cloned(),
                error: Some(message),
                request_id: Some(request_id.clone()),
                generated: false,
            },
            ClientMsg::Content { request_id, .. }
            | ClientMsg::RemoveProp { request_id, .. }
            | ClientMsg::PlaceProp { request_id, .. }
            | ClientMsg::SetPropSupply { request_id, .. } => ServerMsg::PropResult {
                request_id: request_id.clone(),
                ok: false,
                message,
                preview: None,
            },
            ClientMsg::GetPlacementGrid { request_id, .. } => ServerMsg::PlacementGrid {
                request_id: request_id.clone(),
                grids: Vec::new(),
            },
            ClientMsg::PreviewPlacement { request_id, .. } => ServerMsg::PlacementPreview {
                request_id: request_id.clone(),
                ok: false,
                message,
                spot: None,
            },
            ClientMsg::JoinResident { op_id, .. } => ServerMsg::JoinResult {
                op_id: op_id.clone(),
                outcome: Some(self.failed_join(op_id, message)),
            },
            ClientMsg::SetResidentAppearance {
                request_id,
                resident_id,
                ..
            } => self.resident_appearance_reply(
                request_id.clone(),
                resident_id.clone(),
                false,
                message,
            ),
            ClientMsg::SetResidentBody { request_id, .. } => ServerMsg::AgentSaved {
                request_id: request_id.clone(),
                ok: false,
                message,
            },
            // 没有专用回执：门槛只可能拦下其中的 SetSpeed。
            ClientMsg::Avatar { .. }
            | ClientMsg::AvatarInput { .. }
            | ClientMsg::GetChatHistory { .. }
            | ClientMsg::GetManuscript { .. }
            | ClientMsg::GetWorldSettings
            | ClientMsg::GetRecordings
            | ClientMsg::GetProps
            | ClientMsg::ValidateProp { .. }
            | ClientMsg::SaveProp { .. }
            | ClientMsg::DeleteProp { .. }
            | ClientMsg::SetSpeed { .. }
            | ClientMsg::GetModelConfig
            | ClientMsg::SetModelConfig { .. }
            | ClientMsg::SetResidentModel { .. }
            | ClientMsg::RestartBuiltinModel { .. }
            | ClientMsg::TestModelConnection { .. }
            | ClientMsg::GetAgentConfig { .. }
            | ClientMsg::WatchResident { .. }
            | ClientMsg::SetPersona { .. }
            | ClientMsg::ResetWorld { .. }
            | ClientMsg::GetPopulation
            | ClientMsg::GetUsage
            | ClientMsg::GetResidentLibrary { .. }
            | ClientMsg::GetResidentTemplate { .. }
            | ClientMsg::SaveResidentTemplate { .. }
            | ClientMsg::PreviewTemplatePersona { .. }
            | ClientMsg::GenerateResidentTemplate { .. }
            | ClientMsg::ArchiveResidentTemplate { .. }
            | ClientMsg::PreviewResidentJoin { .. }
            | ClientMsg::GetResidentJoinResult { .. }
            | ClientMsg::GetDiary { .. }
            | ClientMsg::GetNotebook { .. }
            | ClientMsg::GetNotebookDetail { .. }
            | ClientMsg::GetAgentLimits
            | ClientMsg::SetAgentLimits { .. }
            | ClientMsg::GetAgentTrace
            | ClientMsg::SetAgentTrace { .. } => error_msg(message),
            // 导演指令不论卡在哪一步都用保存回执的形状回答：页面照常清输入框、报原因。
            ClientMsg::DirectorSay { request_id, .. } => ServerMsg::AgentSaved {
                request_id: request_id.clone(),
                ok: false,
                message,
            },
            // 读导演状态被拒（世界还没重置完）也回答一次，免得页面一直等。
            ClientMsg::GetDirectorState => ServerMsg::DirectorState {
                state: protocol::DirectorState {
                    enabled: false,
                    thinking: false,
                    entries: vec![],
                    pending: vec![],
                    clock: self.world.clock(),
                },
            },
        }
    }

    /// 唯一的分派表：每种客户端消息一行，新增消息编译器会指出漏项。
    fn dispatch(&mut self, msg: ClientMsg, connection: u64, clients: &Clients) -> Outcome {
        use ClientMsg::*;
        match msg {
            SummarizeManuscript {
                world_id,
                request_id,
            } => self.summarize_manuscript(&world_id, &request_id),
            SetWeather {
                request_id,
                expected_revision,
                mode,
                kind,
                ..
            } => self.save_weather(kernel::weather::WeatherRequest {
                request_id,
                expected_revision,
                mode,
                kind,
            }),
            Avatar { .. } | AvatarInput { .. } => {
                Outcome::reply(error_msg("化身命令走专门的入口".into()))
            }
            GetWorldSettings => Outcome::reply(self.world_settings_msg()),
            BeginPrefabSetup {
                request_id,
                setup_id,
                expected_revision,
                plan,
                ..
            } => self.begin_prefab_setup(request_id, setup_id, expected_revision, plan),
            AdvancePrefabSetup {
                request_id,
                setup_id,
                next_step,
                ..
            } => self.advance_prefab_setup(request_id, setup_id, next_step, clients),
            SetWorldview {
                request_id,
                expected_revision,
                text,
                ..
            } => self.save_worldview(request_id, expected_revision, &text),
            SetPhysiology {
                request_id,
                expected_revision,
                switches,
                ..
            } => self.save_physiology(request_id, expected_revision, switches),
            SetMiracles {
                request_id,
                enabled,
                ..
            } => self.save_miracles(request_id, enabled),
            SetClimate {
                request_id,
                climate,
                ..
            } => self.save_climate(request_id, climate),
            SetStrangers {
                request_id,
                enabled,
                ..
            } => self.save_strangers(request_id, enabled),
            SetRecording {
                request_id,
                enabled,
                ..
            } => self.save_recording(request_id, enabled),
            SetAutoSummary {
                request_id,
                enabled,
                ..
            } => self.save_auto_summary(request_id, enabled),
            GetRecordings => Outcome::reply(self.recordings_msg()),
            DeleteRecording {
                request_id, reel, ..
            } => self.delete_recording(request_id, &reel),
            SetGameMode {
                request_id,
                game_mode,
                ..
            } => self.save_game_mode(request_id, game_mode),
            GameModeControl {
                request_id,
                command,
                ..
            } => self.control_game_mode(request_id, &command),
            SetSpeed { speed, .. } => self.set_speed(speed, clients),
            ResetWorld { .. } => self.reset(),
            GetProps => Outcome::reply(self.props_message()),
            Content {
                request_id,
                command,
                ..
            } => self.content_command(request_id, command),
            ValidateProp {
                request_id,
                definition,
            } => self.validate_prop(request_id, definition),
            SaveProp {
                request_id,
                definition,
            } => self.save_prop(request_id, definition),
            DeleteProp { request_id, id } => self.delete_prop(request_id, &id),
            RemoveProp { request_id, id, .. } => self.remove_prop(request_id, &id),
            PlaceProp {
                request_id,
                instance,
                near,
                ..
            } => self.place_prop(request_id, instance, near),
            SetPropSupply {
                request_id,
                prop,
                action,
                remaining,
                ..
            } => self.set_prop_supply(request_id, &prop, &action, remaining),
            GetPlacementGrid { request_id, .. } => self.placement_grid(request_id),
            PreviewPlacement {
                request_id,
                instance,
                near,
                ..
            } => self.preview_placement(request_id, instance, near),
            GetPopulation => Outcome::reply(self.population_msg()),
            SetResidentAppearance {
                request_id,
                expected_revision,
                resident_id,
                appearance,
                ..
            } => {
                self.set_resident_appearance(request_id, expected_revision, resident_id, appearance)
            }
            SetResidentBody {
                request_id,
                resident_id,
                values,
                ..
            } => self.set_resident_body(request_id, &resident_id, values, clients),
            GetResidentLibrary { filter } => Outcome::reply(self.library_msg(&filter)),
            GetResidentTemplate { template_id } => Outcome::reply(self.template_msg(&template_id)),
            SaveResidentTemplate {
                request_id,
                template_id,
                expected_version,
                fields,
            } => self.save_template(request_id, template_id, expected_version, *fields),
            PreviewTemplatePersona { fields } => self.template_persona(&fields),
            GenerateResidentTemplate { request_id, brief } => {
                self.generate_template(request_id, brief, connection, clients)
            }
            ArchiveResidentTemplate {
                request_id,
                template_id,
                expected_version,
                archived,
            } => self.archive_template(request_id, &template_id, expected_version, archived),
            PreviewResidentJoin {
                template_id,
                name,
                home,
            } => self.join_preview(&template_id, name.as_deref(), home.as_deref()),
            JoinResident {
                op_id,
                world_id,
                template_id,
                template_version,
                name,
                home,
                model,
                persona,
            } => self.join(
                JoinRequest {
                    op_id,
                    world_id,
                    template_id,
                    template_version,
                    name,
                    home,
                    model,
                    persona: persona.map(|p| *p),
                },
                clients,
            ),
            GetResidentJoinResult { world_id, op_id } => self.join_result(&world_id, op_id),
            GetChatHistory {
                resident,
                request_id,
                chat_id,
                offset,
            } => self.chat_history(&resident, request_id, chat_id, offset),
            GetModelConfig => self.model_config(),
            SetModelConfig {
                request_id,
                active,
                providers,
                keys,
            } => self.set_model_config(request_id, active, providers, keys),
            SetResidentModel {
                request_id,
                resident,
                model,
            } => self.set_resident_model(request_id, &resident, model),
            RestartBuiltinModel { request_id } => self.restart_builtin_model(request_id),
            TestModelConnection {
                request_id,
                provider,
                model,
                api_key,
            } => self.test_connection(
                crate::settings::ConnectionTest {
                    request_id,
                    provider,
                    model,
                    api_key,
                },
                connection,
                clients,
            ),
            GetAgentConfig { resident } => self.agent_config(resident.as_deref()),
            WatchResident { resident } => self.watch_resident(connection, resident),
            SetPersona {
                request_id,
                resident,
                text,
            } => self.set_persona(request_id, &resident, text),
            GetDiary { resident } => self.diary(resident.as_deref()),
            GetNotebook {
                resident,
                request_id,
                query,
            } => self.notebook(resident, request_id, &query),
            GetNotebookDetail {
                resident,
                request_id,
                query,
            } => self.notebook_detail(resident, request_id, &query),
            DirectorSay { .. } => unreachable!("导演指令在 command 里先处理"),
            GetDirectorState => self.director_state(),
            GetManuscript {
                world_id,
                after,
                before,
                limit,
            } => {
                if world_id
                    .as_ref()
                    .is_some_and(|id| id != &self.population.world_id)
                {
                    Outcome::reply(error_msg("世界已重置，请刷新页面后重试".into()))
                } else {
                    Outcome::reply(self.manuscript.message(after, before, limit))
                }
            }
            GetAgentLimits => Outcome::reply(self.driver.limits_view(&self.world)),
            GetUsage => Outcome::reply(self.driver.usage_view(&self.world)),
            SetAgentLimits { request_id, limits } => self.set_agent_limits(request_id, limits),
            GetAgentTrace => Outcome::reply(self.agent_trace_msg()),
            SetAgentTrace {
                request_id,
                full_prompts,
            } => self.save_agent_trace(request_id, full_prompts),
        }
    }

    fn set_speed(&mut self, speed: f64, clients: &Clients) -> Outcome {
        if !valid_speed(speed) {
            return Outcome::reply(error_msg("速度须在 0 到 600 之间".into()));
        }
        if speed > 0.0 {
            // 点开始就是用户主动继续：清掉额度、故障和过时停止，条件仍在会再次停下。
            let resumed = self.driver.runtime.resume(&self.world);
            if resumed > 0 {
                broadcast_event(
                    clients,
                    &self.world,
                    if resumed == 1 {
                        "停下的村民接着行动了".to_string()
                    } else {
                        format!("{resumed} 位停下的村民接着行动了")
                    },
                    EventInfo::default(),
                );
            }
        }
        self.speed = speed;
        println!("[host] speed = {speed} 倍");
        Outcome::none().persisted()
    }

    /// 重置世界：人口与历史一起清空，回到第一天并停表；换世界标识，旧模型回复与旧加入命令因此失效。
    /// 没有特殊村民，重置后所有人都要从村民库重新加入；村民库本身不动。
    /// 重置是事务：新世界保存成功才算生效，保存失败时世界、人口、会话都回到重置前。
    fn reset(&mut self) -> Outcome {
        let population = Population::fresh();
        let fresh = match self
            .setup
            .fresh_world(&self.prop_library.catalog, &population.world_id)
        {
            Ok(world) => world,
            Err(error) => return Outcome::reply(error_msg(format!("重置失败：{error}"))),
        };
        let result = self.transact(|rt| {
            rt.driver.runtime.clear_world()?;
            rt.world = fresh;
            rt.avatar_control = Default::default();
            rt.population = population;
            // 录像开关是这个小镇的习惯，重置世界不改它
            let recording = rt.world_settings.recording;
            rt.world_settings = Default::default();
            rt.world_settings.recording = recording;
            rt.speed = 0.0;
            Ok::<_, AgentError>(())
        });
        if let Err(aborted) = result {
            let message = match &aborted.cause {
                Abort::Rejected(_) => format!("重置失败：{aborted}"),
                Abort::SaveFailed(error) => {
                    format!("重置未生效，保存失败：{error}{}", aborted.leftover_note())
                }
            };
            eprintln!("[host] {message}");
            return Outcome::reply(error_msg(message));
        }
        println!("[host] 世界已重置：{}，已暂停", self.world.clock());
        // 新世界另开一份剧本流水；旧世界的目录原样保留。
        self.summary_state.cancel();
        self.summary_wait = std::time::Instant::now();
        // 上一世界还没广播的条目必须跟着旧剧本收起，不能作为新世界的第一场戏发出去。
        self.manuscript_pending.clear();
        let root = self.manuscript_root.clone();
        if let Err(error) = self
            .manuscript
            .bind(root.as_deref(), &self.population.world_id)
        {
            eprintln!("[host] 重置后打开新剧本失败：{error}");
        }
        // 重置后是新的世界：上一卷录像收起，开新的一卷
        if self.world_settings.recording {
            if let Err(error) = self.start_reel() {
                eprintln!("[host] 重置后开新的一卷录像失败：{error}");
            }
        }
        Outcome::none()
            .and_all(ServerMsg::WorldReset {
                clock: self.world.clock(),
            })
            .and_all(event(&self.world, "已重置：人口、记忆、目标、进度和用量全部清空，回到第一天并暂停；地图布局与村民库保留"))
            .and_all(self.population_msg())
            .and_all(self.world_settings_msg())
    }

    /// 一拍：推进世界、收模型结果、发新请求、广播 roster，再给在看角色卡的连接补发变了的细节。
    /// 存盘或轨迹写入失败只写日志，不中断运行。
    pub(crate) fn tick(&mut self, clients: &Clients) {
        self.director_tick();
        let directed = std::mem::take(&mut self.director_events);
        self.manuscript_world_events(&directed);
        self.note_director_events(directed);
        self.avatar_tick(clients);
        // 上一拍确认的实录现在才广播；本拍若撤回导演改动，它刚报出的条目还留在缓冲里，不会外泄。
        let earlier = std::mem::take(&mut self.manuscript_pending);
        crate::manuscript::Manuscript::broadcast(clients, earlier);
        // 化身命令等在这拍开头报出的世界日志：本拍收进剧本，末了再广播。
        self.manuscript_pending
            .extend(self.manuscript.drain(&mut self.world));
        let inventory_before = self.world.inventory().revision();
        let missing = self.driver.missing_models(&self.population, &self.settings);
        self.driver.runtime.wait_for_models(&self.world, &missing);
        // 谁都还不能思考（都在等模型）时时间停着，化身照样能走：世界照走的话他们只会一路饿着
        let running = self.speed > 0.0
            && self.world_settings.ready()
            && !self.driver.runtime.nobody_can_think(&self.world);
        self.pace = if running { self.next_pace() } else { 0.0 };
        let events = if running {
            self.world.step(self.director_step_minutes(0.1 * self.pace))
        } else {
            Vec::new()
        };
        self.manuscript_world_events(&events);
        self.note_director_events(events.clone());
        self.driver.dispatch(&mut self.world, events, clients);
        self.director_run_scheduled();
        if let Err(error) = self.driver.receive(&mut self.world, clients) {
            eprintln!("[host] {error}");
        }
        let requests = self.driver.runtime.poll(&mut self.world, running);
        self.hold_if_needed(clients);
        if std::mem::take(&mut self.director_pending_persist) {
            self.checkpoint_now();
        }
        // 周期存盘：到点才抓一份状态交给后台线程写，这一拍不等它。失败只在刚开始失败时通知
        // 客户端，之后隔一会儿重试、不重复刷屏。
        let was_failing = self.save_failed;
        if let Err(error) = self.checkpoint(false) {
            if !was_failing {
                broadcast(clients, &error_msg(kept_in_memory(&error)));
            }
        }
        if let Err(error) = self.driver.send(
            &mut self.world,
            requests,
            &self.population,
            &self.settings,
            clients,
        ) {
            eprintln!("[host] {error}");
        }
        if inventory_before != self.world.inventory().revision() {
            broadcast(clients, &self.props_message());
        }
        self.poll_manuscript_summary(clients);
        self.manuscript_pending
            .extend(self.manuscript.drain(&mut self.world));
        self.auto_summarize_manuscript();
        broadcast(clients, &self.roster());
        self.publish_details(clients);
        self.avatar_publish(clients);
        self.flush_director(clients);
        if self.director_props_changed {
            self.director_props_changed = false;
            broadcast(clients, &self.props_message());
        }
        self.report_tape(clients);
    }

    /// 这一拍的推进速度。平时按正常速度走，走路、交谈都看得清；只有大家都在做一件还要好一阵的事
    /// （都睡了、都在干活）时才快进：每拍加快一点，最多每秒一小时，离最近一个人做完越近越慢，
    /// 到点时正好回到正常速度，接着出门的人不会一闪而过。
    /// 有人要做决定、玩家在控制化身或有人倒下时不快进，回复也就不会因为世界跑得太快而过时；
    /// 有人还没有能用的模型时也不快进。
    fn next_pace(&self) -> f64 {
        let normal = self.normal_pace();
        let calm = !self.avatar_control.held()
            && !self.world.anyone_collapsed()
            && !self.driver.runtime.thinking(&self.world)
            && !self.driver.runtime.waiting_for_model();
        let quiet = || {
            self.world
                .quiet_minutes(|id| self.driver.runtime.next_thought_in(&self.world, id))
        };
        match calm.then(quiet).flatten() {
            Some(left) if normal < FAST_PACE => (self.pace.max(normal) * FAST_RAMP)
                .min(FAST_PACE)
                .min(normal + left / EASE_SECONDS)
                .max(normal),
            _ => normal,
        }
    }

    /// 正常速度（模拟分钟 / 现实秒）：选定速度乘这张地图的基准，再乘开着的玩法此刻要的倍数（没开时是 1）。
    pub(crate) fn normal_pace(&self) -> f64 {
        self.speed * self.base_pace() * self.world.game_mode_tempo()
    }

    /// 选定速度为 1 时世界每秒走几分钟：让村民在画面上每秒走 WALK_UNITS_PER_SECOND 个地图单位。
    /// 步行速度都是每分钟 12 米，但地图的米制比例不同（默认地图每米 12 个单位，新建的地图 60 个），
    /// 按同一个时间比例走，新建地图上的人在画面上快五倍；按画面上的步速定基准，哪张图上都一样自然。
    pub(crate) fn base_pace(&self) -> f64 {
        let units_per_minute = self
            .world
            .walk_map
            .as_ref()
            .map_or(LEGACY_UNITS_PER_MINUTE, |m| {
                m.metres_per_minute * m.svg_units_per_metre
            });
        WALK_UNITS_PER_SECOND / units_per_minute
    }

    /// 额度用尽、故障或回复接连过时：全镇停表并保留位置、活动和用量。
    fn hold_if_needed(&mut self, clients: &Clients) {
        if self.speed == 0.0 {
            return;
        }
        let holds = self.town_holds();
        if holds.is_empty() {
            return;
        }
        self.speed = 0.0;
        for hold in holds {
            let text = format!(
                "全镇已暂停 · {}（{}）：{}",
                hold.resident,
                hold.kind.label(),
                hold.reason
            );
            println!("[host] {text}");
            let info =
                EventInfo::about(&hold.resident_id, &hold.resident).with_alert(Some(Alert::Hold {
                    pause: hold.kind,
                    reason: hold.reason,
                    cause: hold.cause,
                    provider: hold.provider,
                }));
            broadcast_event(clients, &self.world, text, info);
        }
    }

    /// 全镇暂停的原因，补上每位村民用的模型服务名，提示卡据此写「DeepSeek 的密钥用不了了」。
    pub(crate) fn town_holds(&self) -> Vec<protocol::WorldHold> {
        let mut holds = self.driver.runtime.town_holds(&self.world);
        for hold in &mut holds {
            if let Some(choice) = self
                .population
                .residents
                .get(&hold.resident_id)
                .and_then(|r| r.model_binding.as_ref())
            {
                hold.provider = self.settings.provider_name(&choice.provider);
            }
        }
        holds
    }

    /// 存盘。`now` 为真：现在就存，写完才返回；为假：到点才存，写盘在后台（见 `SaveStore`）。
    /// 失败时内存状态不动、返回存盘错误；只在失败与恢复的边界写日志。
    pub(crate) fn checkpoint(&mut self, now: bool) -> Result<(), HostError> {
        let Some(store) = &mut self.store else {
            return Ok(());
        };
        let result = store.checkpoint_bundle(
            &self.world,
            self.speed,
            &self.world_settings,
            Some(&self.population),
            Some(self.driver.runtime.state()),
            now,
            Some(self.director.save()),
        );
        match &result {
            Ok(true) if self.save_failed => {
                self.save_failed = false;
                eprintln!("[host] 存盘已恢复");
            }
            Err(error) if !self.save_failed => {
                self.save_failed = true;
                eprintln!("[host] {}", kept_in_memory(error));
            }
            _ => {}
        }
        result.map(|_| ())
    }

    /// 带上导演会话当场存盘；只用在导演这一拍收过轮次时，失败照旧只记在 `save_failed` 里。
    fn checkpoint_now(&mut self) {
        let was_failing = self.save_failed;
        if let Err(error) = self.checkpoint(true) {
            if !was_failing {
                eprintln!("[host] {}", kept_in_memory(&error));
            }
        }
    }
}

/// 正常速度下村民在画面上每秒走过的地图单位：约小人身高（立牌高约 123 个单位）的八成，
/// 像散步。默认地图按它算是每秒 0.69 分钟。
const WALK_UNITS_PER_SECOND: f64 = 100.0;
/// 没有行走地图时按默认地图的比例（每米 12 个单位、每分钟 12 米）定正常速度。
const LEGACY_UNITS_PER_MINUTE: f64 = 144.0;
/// 自动快进的上限：现实一秒，小镇过一小时。
const FAST_PACE: f64 = 60.0;
/// 快进时每拍加快的比例：三四秒从正常速度加到上限，几分钟的空档只是稍快一点。
const FAST_RAMP: f64 = 1.15;
/// 快进收尾：离最近一个人做完还剩 m 分钟时最多每秒走 m / EASE_SECONDS 分钟，越近越慢，
/// 最后几秒回到正常速度。
const EASE_SECONDS: f64 = 0.8;

/// 不回滚的存盘失败（周期存盘、非事务命令）的说明：改动还在内存里，下次检查点再写。
fn kept_in_memory(error: &HostError) -> String {
    format!("保存失败，内存状态保留，下次检查点重试：{error}")
}

impl Runtime {
    /// 这一拍内核推进出来的世界日志：公告、导演操作和其他公开发生的事进剧本。
    /// 带「一段话」标记的台词已由内核单报，这里跳过。
    fn manuscript_world_events(&mut self, events: &[kernel::Event]) {
        for event in events {
            let kernel::Event::Log {
                minutes,
                text,
                info,
            } = event;
            self.manuscript.ingest_event(info, *minutes, text);
        }
    }

    /// 命令、加入居民等路径产生的世界日志不在 tick 里：先收进剧本，再随其他状态一起发给页面。
    pub(crate) fn dispatch_world_events(&mut self, events: Vec<kernel::Event>, clients: &Clients) {
        self.manuscript_world_events(&events);
        let fresh = self.manuscript.drain(&mut self.world);
        crate::manuscript::Manuscript::broadcast(clients, fresh);
        self.driver.dispatch(&mut self.world, events, clients);
    }
}

pub(crate) fn event(world: &World, text: &str) -> ServerMsg {
    ServerMsg::Event {
        sim_minutes: world.minutes(),
        clock: world.clock(),
        text: text.into(),
        info: EventInfo::default(),
    }
}

/// 自带世界标识的命令；标识对不上就是旧页面发来的，一律退回。
fn world_id_of(msg: &ClientMsg) -> Option<&str> {
    match msg {
        ClientMsg::SetWeather { world_id, .. }
        | ClientMsg::Content { world_id, .. }
        | ClientMsg::BeginPrefabSetup { world_id, .. }
        | ClientMsg::AdvancePrefabSetup { world_id, .. }
        | ClientMsg::SetWorldview { world_id, .. }
        | ClientMsg::SetPhysiology { world_id, .. }
        | ClientMsg::SetMiracles { world_id, .. }
        | ClientMsg::SetClimate { world_id, .. }
        | ClientMsg::SetRecording { world_id, .. }
        | ClientMsg::SetAutoSummary { world_id, .. }
        | ClientMsg::DeleteRecording { world_id, .. }
        | ClientMsg::SetStrangers { world_id, .. }
        | ClientMsg::SetGameMode { world_id, .. }
        | ClientMsg::GameModeControl { world_id, .. }
        | ClientMsg::RemoveProp { world_id, .. }
        | ClientMsg::PlaceProp { world_id, .. }
        | ClientMsg::SetPropSupply { world_id, .. }
        | ClientMsg::GetPlacementGrid { world_id, .. }
        | ClientMsg::PreviewPlacement { world_id, .. }
        | ClientMsg::DirectorSay { world_id, .. }
        | ClientMsg::JoinResident { world_id, .. }
        | ClientMsg::SetResidentAppearance { world_id, .. }
        | ClientMsg::SetResidentBody { world_id, .. } => Some(world_id),
        // 化身命令自己核对世界，查加入结果按世界编号查不到就返回空。
        ClientMsg::Avatar { .. }
        | ClientMsg::AvatarInput { .. }
        | ClientMsg::GetResidentJoinResult { .. } => None,
        ClientMsg::SummarizeManuscript { world_id, .. } => Some(world_id),
        ClientMsg::GetChatHistory { .. }
        | ClientMsg::GetManuscript { .. }
        | ClientMsg::GetWorldSettings
        | ClientMsg::GetRecordings
        | ClientMsg::GetProps
        | ClientMsg::GetDirectorState
        | ClientMsg::ValidateProp { .. }
        | ClientMsg::SaveProp { .. }
        | ClientMsg::DeleteProp { .. }
        | ClientMsg::SetSpeed { .. }
        | ClientMsg::GetModelConfig
        | ClientMsg::SetModelConfig { .. }
        | ClientMsg::SetResidentModel { .. }
        | ClientMsg::RestartBuiltinModel { .. }
        | ClientMsg::TestModelConnection { .. }
        | ClientMsg::GetAgentConfig { .. }
        | ClientMsg::WatchResident { .. }
        | ClientMsg::SetPersona { .. }
        | ClientMsg::ResetWorld { .. }
        | ClientMsg::GetPopulation
        | ClientMsg::GetUsage
        | ClientMsg::GetResidentLibrary { .. }
        | ClientMsg::GetResidentTemplate { .. }
        | ClientMsg::SaveResidentTemplate { .. }
        | ClientMsg::PreviewTemplatePersona { .. }
        | ClientMsg::GenerateResidentTemplate { .. }
        | ClientMsg::ArchiveResidentTemplate { .. }
        | ClientMsg::PreviewResidentJoin { .. }
        | ClientMsg::GetDiary { .. }
        | ClientMsg::GetNotebook { .. }
        | ClientMsg::GetNotebookDetail { .. }
        | ClientMsg::GetAgentLimits
        | ClientMsg::SetAgentLimits { .. }
        | ClientMsg::GetAgentTrace
        | ClientMsg::SetAgentTrace { .. } => None,
    }
}

/// 改世界状态的命令都要先有世界观；查询不受限。
fn requires_ready(msg: &ClientMsg) -> bool {
    match msg {
        ClientMsg::SetWeather { .. }
        | ClientMsg::RemoveProp { .. }
        | ClientMsg::PlaceProp { .. }
        | ClientMsg::SetPropSupply { .. }
        | ClientMsg::JoinResident { .. }
        | ClientMsg::SetResidentAppearance { .. }
        | ClientMsg::SetResidentBody { .. } => true,
        ClientMsg::GameModeControl { .. } => true,
        ClientMsg::SetSpeed { speed, .. } => *speed > 0.0,
        ClientMsg::DirectorSay { .. } => true,
        ClientMsg::Content { command, .. } => !matches!(command, ContentCommand::Detail { .. }),
        ClientMsg::Avatar { .. }
        | ClientMsg::AvatarInput { .. }
        | ClientMsg::GetChatHistory { .. }
        | ClientMsg::GetWorldSettings
        | ClientMsg::BeginPrefabSetup { .. }
        | ClientMsg::AdvancePrefabSetup { .. }
        | ClientMsg::SetWorldview { .. }
        | ClientMsg::SetPhysiology { .. }
        | ClientMsg::SetMiracles { .. }
        | ClientMsg::SetClimate { .. }
        | ClientMsg::SetRecording { .. }
        | ClientMsg::SetAutoSummary { .. }
        | ClientMsg::GetRecordings
        | ClientMsg::DeleteRecording { .. }
        | ClientMsg::SetStrangers { .. }
        | ClientMsg::SetGameMode { .. }
        | ClientMsg::GetProps
        | ClientMsg::GetDirectorState
        | ClientMsg::GetPlacementGrid { .. }
        | ClientMsg::PreviewPlacement { .. }
        | ClientMsg::ValidateProp { .. }
        | ClientMsg::SaveProp { .. }
        | ClientMsg::DeleteProp { .. }
        | ClientMsg::GetModelConfig
        | ClientMsg::SetModelConfig { .. }
        | ClientMsg::SetResidentModel { .. }
        | ClientMsg::RestartBuiltinModel { .. }
        | ClientMsg::TestModelConnection { .. }
        | ClientMsg::GetAgentConfig { .. }
        | ClientMsg::WatchResident { .. }
        | ClientMsg::SetPersona { .. }
        | ClientMsg::ResetWorld { .. }
        | ClientMsg::GetPopulation
        | ClientMsg::GetUsage
        | ClientMsg::GetResidentLibrary { .. }
        | ClientMsg::GetResidentTemplate { .. }
        | ClientMsg::SaveResidentTemplate { .. }
        | ClientMsg::PreviewTemplatePersona { .. }
        | ClientMsg::GenerateResidentTemplate { .. }
        | ClientMsg::ArchiveResidentTemplate { .. }
        | ClientMsg::PreviewResidentJoin { .. }
        | ClientMsg::GetResidentJoinResult { .. }
        | ClientMsg::GetDiary { .. }
        | ClientMsg::GetNotebook { .. }
        | ClientMsg::GetNotebookDetail { .. }
        | ClientMsg::GetAgentLimits
        | ClientMsg::GetManuscript { .. }
        | ClientMsg::SummarizeManuscript { .. }
        | ClientMsg::SetAgentLimits { .. }
        | ClientMsg::GetAgentTrace
        | ClientMsg::SetAgentTrace { .. } => false,
    }
}
