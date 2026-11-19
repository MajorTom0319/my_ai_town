//! 实时剧本的事实出口：台词、心声和动作结果在确认处报出；失败照原话记成失败，不写成做到；
//! 私聊只报给剧本，不因旁观而写进第三人的经历。
use crate::common;
use kernel::World;
use protocol::{
    ActionStatus, InteractionCommand, InteractionKind, ManuscriptKind, ManuscriptVisibility,
};
use serde_json::json;

fn world() -> World {
    let topo = common::two_places(json!([]));
    common::world_with(
        topo,
        600.0,
        &[("r1", "阿树", "loc_a"), ("r2", "小禾", "loc_a")],
    )
}

fn take(w: &mut World) -> Vec<kernel::ManuscriptFact> {
    w.take_manuscript()
}

/// 两人已经谈起来；`voice` 是说话人，`other` 是回应人。
fn talking(w: &mut World, voice: &str, other: &str) -> String {
    let talk = w
        .interact(
            voice,
            "invite",
            w.actor(voice).unwrap().actions.revision,
            &InteractionCommand::Offer {
                partner_id: other.into(),
                interaction: InteractionKind::Talk,
                message: "聊聊。".into(),
            },
        )
        .unwrap();
    w.interact(
        other,
        "accept",
        w.actor(other).unwrap().actions.revision,
        &InteractionCommand::Reply {
            interaction_id: talk.id.clone(),
            accept: true,
            message: "好。".into(),
        },
    )
    .unwrap();
    talk.id
}

#[test]
fn two_people_speaking_in_the_same_minute_both_reach_the_manuscript() {
    let mut w = world();
    let talk = talking(&mut w, "r1", "r2");
    w.take_manuscript();
    w.interact(
        "r1",
        "first-say",
        w.actor("r1").unwrap().actions.revision,
        &InteractionCommand::Say {
            interaction_id: talk.clone(),
            message: "我先说的话。".into(),
            to: None,
        },
    )
    .unwrap();
    w.interact(
        "r2",
        "second-say",
        w.actor("r2").unwrap().actions.revision,
        &InteractionCommand::Say {
            interaction_id: talk,
            message: "我同分钟回应。".into(),
            to: None,
        },
    )
    .unwrap();
    let facts = take(&mut w);
    for line in ["我先说的话。", "我同分钟回应。"] {
        assert!(
            facts
                .iter()
                .any(|f| f.kind == ManuscriptKind::Speech && f.text == line),
            "{line} 应当进剧本：{facts:?}"
        );
    }
}

#[test]
fn manuscript_reports_speech_thoughts_and_confirmed_results() {
    let mut w = world();
    w.take_manuscript();
    w.note_thought("r1", "先去看看水井。", "Agent 决策")
        .unwrap();
    let facts = take(&mut w);
    let thought = facts
        .iter()
        .find(|f| f.kind == ManuscriptKind::Thought)
        .unwrap_or_else(|| panic!("心声应当进剧本：{facts:?}"));
    assert_eq!(thought.visibility, ManuscriptVisibility::Private);
    assert_eq!(
        thought.source,
        format!("thought:r1:{}", thoughts_at(&w, "r1").sim_minutes)
    );

    common::run(
        &mut w,
        "r1",
        "g1",
        json!({"intent": {"verb": "前往", "locale": "loc_b"}}),
    )
    .unwrap();
    let receipt = common::finish_player_group(&mut w, "r1", "g1");
    let facts = take(&mut w);
    let action = facts
        .iter()
        .find(|f| f.kind == ManuscriptKind::Action)
        .unwrap_or_else(|| panic!("动作结果应当进剧本：{facts:?}"));
    assert_eq!(action.visibility, ManuscriptVisibility::Private);
    assert_eq!(action.tone, protocol::ManuscriptTone::Result);
    assert_eq!(action.actor.as_deref(), Some("r1"));
    assert!(
        action.text.contains(&receipt.verb) || action.text.contains("到达"),
        "{action:?}"
    );
}

fn thoughts_at(w: &World, id: &str) -> protocol::ThoughtView {
    w.actor(id).unwrap().journal.view().unwrap()
}

#[test]
fn failed_actions_are_recorded_as_failed_and_other_people_keep_their_memories() {
    let mut w = world();
    w.take_manuscript();
    let before = w.actor("r2").unwrap().journal.events.len();
    let rejected = common::run(
        &mut w,
        "r1",
        "g2",
        json!({"intent": {"verb": "前往", "locale": "loc_missing"}}),
    );
    assert!(rejected.is_err(), "{rejected:?}");
    let facts = take(&mut w);
    // 提交前校验就拒绝的意图不会成为世界事实，剧本不收。
    assert!(facts.is_empty(), "{facts:?}");
    assert_eq!(w.actor("r2").unwrap().journal.events.len(), before);

    // 真正走到一半失败的：目标动作开始、身体倒下导致中断/失败，结果按世界回执记下。
    let mut w = world();
    w.note_thought("r1", "该休息了。", "Agent 决策")
        .expect("心声应当记下");
    w.take_manuscript();
    let outcome = common::run(
        &mut w,
        "r1",
        "g3",
        json!({"intent": {"verb": "停留", "duration_minutes": 90}}),
    );
    assert!(outcome.is_ok(), "{outcome:?}");
    w.set_body_values("r1", Some(0.0), Some(0.0), Some(0.0), Some(5.0))
        .unwrap();
    let receipt = common::finish_player_group(&mut w, "r1", "g3");
    let facts = take(&mut w);
    if let Some(action) = facts.iter().find(|f| f.kind == ManuscriptKind::Action) {
        assert_eq!(action.tone, protocol::ManuscriptTone::Result);
        if receipt.status != ActionStatus::Succeeded {
            assert!(
                action.text.contains(&receipt.verb),
                "失败结果要照原文记下：{action:?} / {receipt:?}"
            );
        }
    }
    let private = facts
        .iter()
        .filter(|f| f.actor.as_deref() == Some("r2"))
        .count();
    assert_eq!(private, 0, "r1 的私密事实不进 r2 的剧本：{facts:?}");
}

#[test]
fn ordinary_chat_is_private_and_public_events_stay_in_the_world_log() {
    let mut w = world();
    let talk = w
        .interact(
            "r1",
            "invite",
            w.actor("r1").unwrap().actions.revision,
            &InteractionCommand::Offer {
                partner_id: "r2".into(),
                interaction: InteractionKind::Talk,
                message: "聊聊。".into(),
            },
        )
        .unwrap();
    w.interact(
        "r2",
        "accept",
        w.actor("r2").unwrap().actions.revision,
        &InteractionCommand::Reply {
            interaction_id: talk.id.clone(),
            accept: true,
            message: "好。".into(),
        },
    )
    .unwrap();
    w.take_manuscript();
    w.interact(
        "r1",
        "say",
        w.actor("r1").unwrap().actions.revision,
        &InteractionCommand::Say {
            interaction_id: talk.id.clone(),
            message: "只说给我们听。".into(),
            to: None,
        },
    )
    .unwrap();
    let facts = take(&mut w);
    let speech = facts
        .iter()
        .find(|f| f.kind == ManuscriptKind::Speech)
        .unwrap_or_else(|| panic!("台词应当进剧本：{facts:?}"));
    assert_eq!(speech.visibility, ManuscriptVisibility::Private);
    assert_eq!(speech.text, "只说给我们听。");
    assert!(speech.about.contains(&"r1".into()) && speech.about.contains(&"r2".into()));

    w.take_manuscript();
    w.director_announce("今晚别出门。", None).unwrap();
    let facts = w.take_events();
    let notice = facts
        .iter()
        .find_map(|event| match event {
            kernel::Event::Log { info, text, .. } if info.kind == protocol::EventKind::Director => {
                Some((info, text))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("导演公告应当写进世界日志：{facts:?}"));
    assert_eq!(notice.0.kind, protocol::EventKind::Director);
    assert!(notice.1.contains("今晚别出门"));
    assert!(
        take(&mut w).is_empty(),
        "公开世界日志由 host 收进剧本，不重复报"
    );
}
