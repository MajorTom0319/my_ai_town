//! 剧本分段的读回与概览引用的边界；走公开的落盘与整理入口，不起服务进程。
use crate::manuscript::Manuscript;
use kernel::ManuscriptFact;
use protocol::{ManuscriptKind, ManuscriptTone, ManuscriptVisibility};

fn fact(source: &str, text: &str) -> ManuscriptFact {
    ManuscriptFact {
        kind: ManuscriptKind::Speech,
        visibility: ManuscriptVisibility::Public,
        tone: ManuscriptTone::Statement,
        actor: None,
        name: None,
        locale: None,
        text: text.into(),
        about: Vec::new(),
        source: source.into(),
        at: 0.0,
    }
}

#[test]
fn restart_appends_to_the_last_segment_instead_of_the_first() {
    let root = std::env::temp_dir().join(format!("town-manuscript-segment-{}", std::process::id()));
    let dir = root.join("manuscript").join("world-1");
    std::fs::create_dir_all(&dir).unwrap();
    // 加入一行不合格式的行：模拟写到一半被打断；它不能把后面的有效段号一起丢掉。
    std::fs::write(dir.join("events.jsonl"), "{\"seq\":1}\nnot-json\n").unwrap();
    let entry = |seq: u64, text: &str, source: &str| protocol::ManuscriptEntry {
        seq,
        world_id: "world-1".into(),
        sim_minutes: seq as f64,
        clock: format!("D1 00:{seq:02}"),
        kind: ManuscriptKind::World,
        visibility: ManuscriptVisibility::Public,
        tone: ManuscriptTone::Statement,
        actor: None,
        name: None,
        locale: None,
        text: text.into(),
        about: Vec::new(),
        source: source.into(),
    };
    std::fs::write(
        dir.join("events-0001.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::to_string(&entry(2, "第二段", "event:2")).unwrap(),
            "not-json"
        ),
    )
    .unwrap();
    let mut book = Manuscript::open(Some(&root), "world-1").unwrap();
    let appended = book.ingest(fact("event:3:第三段", "第三段")).unwrap();
    assert_eq!(appended.seq, 3);
    let parts: Vec<_> = book
        .entries
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    assert_eq!(parts, vec!["第二段", "第三段"]);
    let later = std::fs::read_to_string(dir.join("events-0001.jsonl")).unwrap();
    assert!(later.contains("第三段"), "重启后要接在最大段后面：{later}");
    assert_eq!(
        std::fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        "{\"seq\":1}\nnot-json\n"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn summary_citations_cannot_point_past_the_analyzed_batch() {
    let mut book = Manuscript::open(None, "world-1").unwrap();
    for index in 1..=3 {
        book.ingest(fact(
            &format!("event:{index}:事实{index}"),
            &format!("事实{index}"),
        ));
    }
    let summary = book
        .summarize(
            "现在".into(),
            vec![vec!["1".into(), "3".into()]],
            Vec::new(),
            vec![vec!["9".into()]],
            1,
        )
        .unwrap();
    assert_eq!(summary.through_seq, 1);
    assert_eq!(summary.conflicts, vec![vec![String::from("1")]]);
    assert!(summary.questions.is_empty());
}

#[test]
fn an_automatically_requested_batch_retries_until_it_is_accepted() {
    let mut book = Manuscript::open(None, "world-1").unwrap();
    for index in 1..=5 {
        book.ingest(fact(
            &format!("event:{index}:事实{index}"),
            &format!("事实{index}"),
        ));
    }
    assert!(!book.auto_due(0, 20));
    book.retry_auto();
    assert!(book.auto_due(0, 20), "没有接单的批次必须在节流后重试");
    book.accepted_auto();
    assert!(!book.auto_due(0, 20), "在途或已接单的同一批不重复入队");
    book.retry_auto();
    book.finish_auto();
    assert!(!book.auto_due(0, 20), "整理完成后的失败批次不再重试");
}

#[test]
fn judge_only_facts_are_private_even_when_old_producers_marked_them_public() {
    let mut book = Manuscript::open(None, "world-1").unwrap();
    // 旧流水只剩显示文本，原事实来源已不在条目里；出局归仓仍要唤醒整理。
    let mut event = fact(
        "event:1:裁判全知：甲出局前持有私粮2袋，已全部归入公仓。",
        "裁判全知：甲出局前持有私粮2袋，已全部归入公仓。",
    );
    event.kind = ManuscriptKind::Announcement;
    let judge = book.ingest(event).unwrap();
    assert_eq!(judge.visibility, ManuscriptVisibility::Private);
    assert!(
        book.auto_due(0, 20),
        "旧流水的出局账目仍是关键剧情，自动整理不能因隐私修正漏看"
    );
    book.finish_auto();
    let mut automatic = fact(
        "vote:1-0-automatic",
        "1号没有提交选票，系统按既定规则代投给2号。",
    );
    automatic.visibility = ManuscriptVisibility::Private;
    let other = book.ingest(automatic).unwrap();
    assert_eq!(other.visibility, ManuscriptVisibility::Private);
    assert!(
        !book.auto_due(1, 20),
        "普通私密代投记录不能绕过自动整理门槛"
    );
}

#[test]
fn only_marked_key_events_bypass_the_auto_summary_threshold() {
    let mut book = Manuscript::open(None, "world-1").unwrap();
    book.ingest_event(
        &protocol::EventInfo::default(),
        1.0,
        "[D1 00:01] 【十日生存】公告：有人报名劳动。",
    );
    assert!(!book.auto_due(0, 20), "日程公告不该每来一条就调模型");
    book.ingest(fact(
        "event:100:【十日生存】公告：天灾公告（已张贴公告栏）",
        "【十日生存】公告：天灾公告（已张贴公告栏）",
    ));
    assert!(
        !book.auto_due(0, 20),
        "普通公告即使带天灾字样也不能绕过门槛"
    );
    book.ingest_event(
        &protocol::EventInfo::default(),
        2.0,
        "[D1 00:02] 【十日生存】【关键剧情】公告：有人出局，私粮归入公仓。",
    );
    assert!(book.auto_due(0, 20));
}
