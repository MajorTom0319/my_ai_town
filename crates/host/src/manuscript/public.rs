//! 世界日志进剧本：只收公开发生的事，模型台账、走动和已重置这类不长故事的行不收；
//! 正文已经写明事态的公告与导演操作按类型收原文，不按中文猜。
use protocol::{EventInfo, EventKind, ManuscriptKind, ManuscriptTone};

/// 世界日志一行 →（种类、语气、原文、来源）。返回 None 表示不进剧本。
pub fn public_entry(
    info: &EventInfo,
    minutes: f64,
    raw: &str,
) -> Option<(ManuscriptKind, ManuscriptTone, String, String)> {
    if info.speech.is_some() {
        return None;
    }
    let text = strip_prefix(raw);
    if text.is_empty() || noisy(&text) {
        return None;
    }
    let (kind, tone) = match info.kind {
        EventKind::Director => (ManuscriptKind::Director, ManuscriptTone::Announcement),
        EventKind::Model | EventKind::Movement => return None,
        EventKind::World if notice(&text) => {
            (ManuscriptKind::Announcement, ManuscriptTone::Announcement)
        }
        EventKind::World => (ManuscriptKind::World, ManuscriptTone::Statement),
    };
    Some((
        kind,
        tone,
        text.clone(),
        format!("event:{}:{text}", (minutes * 60.0).round() as i64),
    ))
}

/// 日志正文去掉服务端附的 `[D1 07:17]` 前缀。
fn strip_prefix(text: &str) -> String {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('[') {
        if let Some(end) = rest.find("] ") {
            let clock = &rest[..end];
            if clock.starts_with('D') && clock.contains(':') {
                return rest[end + 2..].trim().to_string();
            }
        }
    }
    text.to_string()
}

/// 不像剧情的一行：重置、继续行动、录像开关这类维护消息。
fn noisy(text: &str) -> bool {
    text.starts_with("已重置：") || text == "停下的村民接着行动了" || text.starts_with("录像")
}

fn notice(text: &str) -> bool {
    text.starts_with("公告") || text.starts_with("通知") || text.starts_with("置顶")
}
