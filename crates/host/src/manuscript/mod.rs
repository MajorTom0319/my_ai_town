//! 实时剧本：把世界确认过的台词、心声、动作结果和公开事件按世界追加进小镇目录，
//! 再增量广播给页面（crates/protocol 的 manuscript）。这里只收信、编号、去重、落盘，不写小说，
//! 不调模型，也不回写村民记忆。剧情概览以后单独接。
//!
//! 目录 `manuscript/<world_id>/` 里是 `events.jsonl`，一行一条；`meta.json` 记格式号。
//! 世界重置换新的 world_id，旧世界目录原样保留。读档、重启后接着最后的编号写；
//! 第一次接入之前的历史不补，页面据 cursor 知道从哪里开始。
use crate::{network::Clients, HostError};
use kernel::{ManuscriptFact, World};
use protocol::{
    EventInfo, ManuscriptEntry, ManuscriptKind, ManuscriptPage, ManuscriptSummary, ManuscriptTone,
    ManuscriptVisibility, ServerMsg,
};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

mod public;
mod summary;

pub use public::public_entry;
pub use summary::SummaryState;
pub use summary::{
    auto_due, body as summary_body, parse as parse_summary, prompt as summary_prompt, SummaryJob,
    AUTO_LINES,
};

/// 小镇目录下放剧本的子目录
pub const MANUSCRIPT_DIR: &str = "manuscript";
/// 一段写满多少条就分下一段；读的时候按段号接起来
const SEGMENT_LINES: usize = 4000;
/// 缺省一页多少条
pub const PAGE_LIMIT: usize = 400;
/// 一次最多取多少条
pub const MAX_PAGE: usize = 2000;
const FORMAT: u32 = 1;
const EVENTS: &str = "events.jsonl";
const META: &str = "meta.json";
const SUMMARY: &str = "summary.json";
/// 每次交给整理任务的实录上限；够看清当前局势，也不拖慢村民的模型队列。
const SUMMARY_LINES: usize = 160;

/// `events.jsonl` 是第 0 段，`events-0001.jsonl` 是第 1 段。
fn segment_number(path: &Path) -> Option<usize> {
    let name = path.file_name()?.to_str()?;
    if name == EVENTS {
        return Some(0);
    }
    name.strip_prefix("events-")?
        .strip_suffix(".jsonl")?
        .parse()
        .ok()
}

pub struct Manuscript {
    /// 世界标识；切换世界时换目录，旧目录不动
    world_id: String,
    /// 小镇目录下 manuscript/；运行时没有目录就不落盘，只在内存里编号
    root: Option<PathBuf>,
    entries: Vec<ManuscriptEntry>,
    /// 已经收过的来源编号；重启后从流水重建
    seen: HashSet<String>,
    cursor: u64,
    /// 当前可追加的最后一段；读档时按实际文件恢复，重启后接着它写
    segment: usize,
    segment_lines: usize,
    /// 是否已经见过至少一份分段文件；空目录不能被当成“第零段已满”
    has_segment: bool,
    summary: Option<ManuscriptSummary>,
    /// 上次整理成功写到的编号；重启后为 None，自动整理先等新实录或状态读入再决定。
    summary_through: Option<u64>,
    /// 自动整理已经成功接单、尚未回来的最后编号。
    auto_requested: Option<u64>,
    /// 已触发但没接单或失败的批次，等节流结束后优先重试，不能等新的普通记录。
    auto_retry: bool,
}

impl Manuscript {
    /// 打开某个世界的剧本。没有小镇目录（--no-save）时只保留本次运行的条目。
    pub fn open(root: Option<&Path>, world_id: &str) -> Result<Self, HostError> {
        let mut book = Self {
            world_id: world_id.into(),
            root: root.map(|root| root.join(MANUSCRIPT_DIR).join(world_id)),
            entries: Vec::new(),
            seen: HashSet::new(),
            cursor: 0,
            segment: 0,
            segment_lines: 0,
            has_segment: false,
            summary: None,
            summary_through: None,
            auto_requested: None,
            auto_retry: false,
        };
        if let Some(dir) = book.root.clone() {
            book.load(&dir)?;
        }
        Ok(book)
    }

    /// 切换世界：重置、读档换了世界标识时用它。旧世界已落盘的流水不动。
    pub fn bind(&mut self, root: Option<&Path>, world_id: &str) -> Result<(), HostError> {
        if self.world_id == world_id && self.root.is_some() == root.is_some() {
            return Ok(());
        }
        *self = Self::open(root, world_id)?;
        Ok(())
    }

    fn load(&mut self, dir: &Path) -> Result<(), HostError> {
        let meta = dir.join(META);
        if meta.exists() {
            let text = std::fs::read_to_string(&meta).map_err(|e| {
                HostError::Save(format!("读取剧本说明 {} 失败：{e}", meta.display()))
            })?;
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                HostError::Save(format!("解析剧本说明 {} 失败：{e}", meta.display()))
            })?;
            if value["format"].as_u64() != Some(FORMAT.into()) {
                return Err(HostError::Save(format!(
                    "剧本 {} 的格式号不认得，未改动原文件",
                    dir.display()
                )));
            }
        }
        self.read_segments(dir)?;
        let summary = dir.join(SUMMARY);
        if summary.exists() {
            let text = std::fs::read_to_string(&summary).map_err(|e| {
                HostError::Save(format!("读取剧情概览 {} 失败：{e}", summary.display()))
            })?;
            self.summary = serde_json::from_str(&text).ok();
            self.summary_through = self.summary.as_ref().map(|s| s.through_seq);
        }
        self.entries.sort_by_key(|e| e.seq);
        // 同一编号出现两次说明文件被改过：以先读到的为准，后面不覆盖。
        let mut last = 0;
        self.entries.retain(|e| {
            if e.seq <= last {
                return false;
            }
            last = e.seq;
            true
        });
        self.cursor = self.entries.last().map_or(0, |e| e.seq);
        Ok(())
    }

    fn read_segments(&mut self, dir: &Path) -> Result<(), HostError> {
        let Ok(list) = std::fs::read_dir(dir) else {
            return Ok(());
        };
        let mut paths: Vec<PathBuf> = list
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        // `events-0001.jsonl` 的段号大于 `events.jsonl`，但字典序更小；按段号读，
        // 重启后才不会把后一段接在前一段之前，再被编号去重误删半部剧情。
        paths.sort_by_key(|path| segment_number(path));
        // 不是剧本分段 `.jsonl` 的文件不能顺手改掉“最后一段”：读它可以忽略，
        // 但眼睛已经扫过它，写入必须仍停在真正的最大段。
        let last = paths.iter().filter_map(|path| segment_number(path)).max();
        for path in &paths {
            let Some(index) = segment_number(path) else {
                continue;
            };
            let file = match File::open(path) {
                Ok(file) => file,
                Err(_) => continue,
            };
            // 物理行数决定下一行从哪一段续写；坏行不能把后面的有效行挤到更早的段里去。
            let mut lines = 0;
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                lines += 1;
                let Ok(entry) = serde_json::from_str::<ManuscriptEntry>(&line) else {
                    continue;
                };
                if entry.world_id != self.world_id {
                    continue;
                }
                self.seen.insert(entry.source.clone());
                self.entries.push(entry);
            }
            if Some(index) == last {
                // 最后一段可能还没写满；重启后继续写它最后一行后面，不另开一份空段。
                self.segment = index;
                self.segment_lines = lines;
                self.has_segment = true;
            }
        }
        Ok(())
    }

    /// 收一条内核报来的事实。world、时刻都取自事实本身；返回新生成的条目。
    pub fn ingest(&mut self, fact: ManuscriptFact) -> Option<ManuscriptEntry> {
        if self.seen.contains(&fact.source) {
            return None;
        }
        self.push(
            fact.kind,
            fact.visibility,
            fact.tone,
            fact.actor,
            fact.name,
            fact.locale,
            fact.text,
            fact.about,
            fact.source,
            fact.at,
        )
    }

    /// 收一条世界日志：台词标记已经在文案里，公开事件单独收；模型台账与移动不收。
    pub fn ingest_event(
        &mut self,
        info: &EventInfo,
        minutes: f64,
        raw: &str,
    ) -> Option<ManuscriptEntry> {
        public_entry(info, minutes, raw).and_then(|(kind, tone, text, source)| {
            let actor = info.actor.as_ref();
            self.push(
                kind,
                ManuscriptVisibility::Public,
                tone,
                actor.map(|a| a.id.clone()),
                actor.map(|a| a.name.clone()),
                None,
                text,
                vec![],
                source,
                minutes,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        kind: ManuscriptKind,
        visibility: ManuscriptVisibility,
        tone: ManuscriptTone,
        actor: Option<String>,
        name: Option<String>,
        locale: Option<String>,
        text: String,
        about: Vec<String>,
        source: String,
        at: f64,
    ) -> Option<ManuscriptEntry> {
        if !self.seen.insert(source.clone()) {
            return None;
        }
        // 旧内核曾把出局者私粮余额列成公开；改不动已写的流水，但阅读与后续导出仍不能把它当作公开。
        let visibility = if text.contains("裁判全知") {
            ManuscriptVisibility::Private
        } else {
            visibility
        };
        self.cursor += 1;
        let entry = ManuscriptEntry {
            seq: self.cursor,
            world_id: self.world_id.clone(),
            sim_minutes: at,
            // 剧本与界面用同一条「D1 00:00 起」的时钟；世界重置回零点时一律从第一天起，
            // 不用模拟分钟 0 前后的浮点余数为玩家读出一个不存在的第 0 天。
            clock: kernel::clock_string(at.max(0.0)),
            kind,
            visibility,
            tone,
            actor,
            name,
            locale,
            text,
            about,
            source,
        };
        if let Err(error) = self.append(&entry) {
            eprintln!("[host] 写剧本失败：{error}");
        }
        self.entries.push(entry.clone());
        Some(entry)
    }

    fn append(&mut self, entry: &ManuscriptEntry) -> Result<(), HostError> {
        let Some(dir) = &self.root else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)
            .map_err(|e| HostError::Save(format!("建剧本目录 {} 失败：{e}", dir.display())))?;
        let meta = dir.join(META);
        if !meta.exists() {
            let text = format!("{{\"format\":{FORMAT}}}\n");
            let tmp = dir.join(format!(".{META}.tmp"));
            std::fs::write(&tmp, text)
                .and_then(|_| std::fs::rename(&tmp, &meta))
                .map_err(|e| HostError::Save(format!("写剧本说明 {} 失败：{e}", meta.display())))?;
        }
        // 分段：第一段 events.jsonl，之后 events-0001.jsonl……
        let mut segment = self.segment;
        let mut lines = self.segment_lines;
        let path = loop {
            let path = if segment == 0 {
                dir.join(EVENTS)
            } else {
                dir.join(format!("events-{segment:04}.jsonl"))
            };
            let exists = self.has_segment || lines > 0 || path.exists();
            if lines < SEGMENT_LINES || !exists {
                break path;
            }
            segment += 1;
            lines = 0;
        };
        let line = serde_json::to_string(entry).map_err(|e| HostError::Save(e.to_string()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| HostError::Save(format!("打开剧本 {} 失败：{e}", path.display())))?;
        file.write_all(line.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.flush())
            .map_err(|e| HostError::Save(format!("写剧本 {} 失败：{e}", path.display())))?;
        self.segment = segment;
        self.segment_lines = lines + 1;
        self.has_segment = true;
        Ok(())
    }

    /// 取一页：after 往后读、before 往前读；都不给时取最近的 limit 条。
    pub fn page(&self, after: Option<u64>, before: Option<u64>, limit: usize) -> ManuscriptPage {
        let limit = limit.clamp(1, MAX_PAGE);
        let entries = match (after, before) {
            (Some(after), _) => self
                .entries
                .iter()
                .filter(|e| e.seq > after)
                .take(limit)
                .cloned()
                .collect(),
            (None, Some(before)) => {
                let end = self.entries.partition_point(|e| e.seq < before);
                self.entries[end.saturating_sub(limit)..end].to_vec()
            }
            (None, None) => {
                let start = self.entries.len().saturating_sub(limit);
                self.entries[start..].to_vec()
            }
        };
        ManuscriptPage {
            world_id: self.world_id.clone(),
            entries,
            cursor: self.cursor,
        }
    }

    /// 把这一拍从内核取来的事实收进剧本，返回新条目（按编号顺序）。
    pub fn drain(&mut self, world: &mut World) -> Vec<ManuscriptEntry> {
        let mut entries: Vec<_> = world
            .take_manuscript()
            .into_iter()
            .filter_map(|fact| self.ingest(fact))
            .collect();
        // 内核取走前已按发生时刻排好；这里再保证编号也沿模拟时间递增，分页翻旧记录时顺序稳定。
        entries.sort_by(|a, b| {
            a.sim_minutes
                .total_cmp(&b.sim_minutes)
                .then_with(|| a.source.cmp(&b.source))
        });
        entries
    }

    /// 广播新条目。没有新条目不发。
    pub fn broadcast(clients: &Clients, entries: Vec<ManuscriptEntry>) {
        if !entries.is_empty() {
            crate::network::broadcast(clients, &ServerMsg::ManuscriptAppend { entries });
        }
    }

    pub fn message(
        &self,
        after: Option<u64>,
        before: Option<u64>,
        limit: Option<u32>,
    ) -> ServerMsg {
        ServerMsg::ManuscriptPage {
            page: self.page(after, before, limit.map_or(PAGE_LIMIT, |n| n as usize)),
        }
    }

    pub fn summary(&self) -> Option<&ManuscriptSummary> {
        self.summary.as_ref()
    }

    /// 上次成功整理或整数恢复到的编号：自动整理据此只数之后的新条目。
    pub fn summarized_through(&self) -> Option<u64> {
        self.summary_through
    }

    /// 交给整理任务的实录：从上一版概览之后接最近一段；第一版也取最近的实录，
    /// 不把很久以前的开场重新当成当前局势。
    pub fn summary_lines(&self, previous: Option<&ManuscriptSummary>) -> Vec<ManuscriptEntry> {
        let from = previous.map_or(0, |s| s.through_seq);
        let fresh: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.seq > from)
            .take(SUMMARY_LINES)
            .cloned()
            .collect();
        if from > 0 || self.entries.len() <= SUMMARY_LINES {
            fresh
        } else {
            self.entries[self.entries.len() - SUMMARY_LINES..].to_vec()
        }
    }

    /// 自动整理的门槛：梗概之后攒够一段新的剧情，或刚发生投票、淘汰、灾害这类关键事实。
    /// `requested` 是已成功发起、尚未回来的最大条目；失败重试由下方与概览一起判断。
    pub fn auto_due(&self, from: u64, threshold: usize) -> bool {
        auto_due(
            &self.entries,
            from,
            self.auto_requested,
            self.auto_retry,
            threshold,
        )
    }

    /// 自动整理发起失败：这批实录没有交给模型，不能等到下一次普通消息才重试。
    pub fn retry_auto(&mut self) {
        self.auto_requested = None;
        self.auto_retry = true;
    }

    /// 这批整理已有结果（成功或有错误回复），或玩家已取消自动整理：不必再把它当成待重试的批次。
    pub fn finish_auto(&mut self) {
        self.auto_retry = false;
    }

    /// 自动整理成功接单后只等本批结果；新的实录再按普通门槛和关键事件判断。
    pub fn accepted_auto(&mut self) {
        self.auto_requested = Some(self.cursor);
        self.auto_retry = false;
    }

    /// 模型返回的判断只保留能指回本世界、且不超过本版统计截止的条目；其余编号直接丢掉。
    pub fn summarize(
        &mut self,
        situation: String,
        conflicts: Vec<Vec<String>>,
        promises: Vec<Vec<String>>,
        questions: Vec<Vec<String>>,
        through: u64,
    ) -> Option<ManuscriptSummary> {
        let latest = self.entries.last()?.clone();
        let through_seq = through
            .min(latest.seq)
            .max(self.summary.as_ref().map_or(0, |s| s.through_seq));
        let clock = self
            .entries
            .iter()
            .find(|e| e.seq == through_seq)
            .map_or(latest.clock, |e| e.clock.clone());
        let cited = |items: Vec<Vec<String>>| {
            items
                .into_iter()
                .filter_map(|mut item| {
                    item.retain(|source| {
                        source
                            .parse::<u64>()
                            .is_ok_and(|seq| (1..=through_seq).contains(&seq))
                    });
                    (!item.is_empty()).then_some(item)
                })
                .collect()
        };
        let summary = ManuscriptSummary {
            through_seq,
            through_clock: clock,
            situation: situation.chars().take(600).collect(),
            conflicts: cited(conflicts),
            promises: cited(promises),
            questions: cited(questions),
        };
        if let Some(dir) = &self.root {
            if let Err(error) = std::fs::create_dir_all(dir) {
                eprintln!("[host] 建剧本目录失败：{error}");
            } else {
                let path = dir.join(SUMMARY);
                let tmp = dir.join(format!(".{SUMMARY}.tmp"));
                let result = serde_json::to_string(&summary)
                    .map_err(|e| HostError::Save(e.to_string()))
                    .and_then(|text| {
                        std::fs::write(&tmp, text)
                            .and_then(|_| std::fs::rename(&tmp, &path))
                            .map_err(|e| HostError::Save(format!("写剧情概览失败：{e}")))
                    });
                if let Err(error) = result {
                    eprintln!("[host] {error}");
                }
            }
        }
        self.summary = Some(summary.clone());
        self.summary_through = Some(through_seq);
        Some(summary)
    }
}

#[cfg(test)]
#[path = "../../tests/manuscript_unit.rs"]
mod tests;
