use crate::common;
use kernel::{
    avatar::{Area, Spot, Surface, WalkMap},
    navigation::space::Terrain,
    needs::NeedValues,
    Event, World,
};
use protocol::{
    FactKind, GameModeAction, GameModeChoice, GameModeContext, GameModeView, GroupStatus,
    ManuscriptKind, ManuscriptVisibility, PhysiologySwitches, Survival10Phase, Survival10View,
    SwordholderEnding, SwordholderPhase, SwordholderRole, SwordholderView, TideCircle, TideFlare,
    TideNewsKind, TidePhase, TideView, WerewolfOut, WerewolfPhase, WerewolfRole, WerewolfSide,
    WerewolfView,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn walk(w: &mut World, id: &str, key: &str, locale: &str) {
    common::run(
        w,
        id,
        key,
        json!({"intent": {"verb": "前往", "locale": locale}}),
    )
    .unwrap();
    common::finish(w, id, key);
}

fn notes(w: &World, id: &str) -> Vec<String> {
    w.actor(id)
        .unwrap()
        .journal
        .events_after(0)
        .iter()
        .map(|e| e.text.clone())
        .collect()
}

const HOUR: f64 = 60.0;
const DAY: f64 = 1440.0;
const GOAL: [f64; 2] = [24.0, 18.0];

fn disaster_board(world: &mut World, locale: &str) {
    let definition = kernel::props::Catalog::from_json(include_str!(
        "../../../data/props/core/definitions.json"
    ))
    .unwrap()
    .latest("core:bulletin_board")
    .unwrap()
    .clone();
    world
        .place_prop(kernel::props::Placement {
            instance: kernel::props::Instance {
                id: "disaster-board".into(),
                definition: definition.reference(),
                locale: locale.into(),
                name: None,
                board: None,
            },
            definition,
        })
        .unwrap();
}

#[test]
fn survival10_disasters_commit_real_losses_and_private_notices_once() {
    let (mut world, ids) = survival10_night_world(7);
    let event = |kind| {
        serde_json::from_value::<protocol::GameModeEvent>(
            json!({"kind":"survival10_disaster","event":{"kind":kind}}),
        )
        .unwrap()
    };
    let before = serde_json::to_value(world.snapshot()).unwrap();
    assert!(world.director_game_event(&event("rats")).is_err());
    assert_eq!(serde_json::to_value(world.snapshot()).unwrap(), before);
    disaster_board(&mut world, "loc_a");
    for id in &ids {
        world.test_give_rice(id, 5);
    }
    let before = survival10(&world);
    world.take_manuscript();
    world.director_game_event(&event("rats")).unwrap();
    let rats = survival10(&world);
    assert!((3..=10).contains(&(before.public_rice - rats.public_rice)));
    assert_eq!(granary_on_map(&world), rats.public_rice);
    for id in &ids {
        assert!(notes(&world, id)
            .iter()
            .any(|note| note.contains("赛事公告送达") && note.contains("鼠患")));
    }
    let mut replay = serde_json::to_value(world.snapshot()).unwrap();
    world.director_game_event(&event("flood")).unwrap();
    let flooded = survival10(&world);
    assert_eq!(flooded.public_rice, rats.public_rice - rats.public_rice / 3);
    let victims: Vec<_> = flooded.seats.iter().filter(|seat| seat.rice == 0).collect();
    assert!((1..=3).contains(&victims.len()));
    let facts = world.take_manuscript();
    let outcome = facts
        .iter()
        .find(|fact| fact.source.starts_with("event:") && fact.source.ends_with("-disaster-2"))
        .unwrap_or_else(|| panic!("天灾的裁判全知结果应进剧本：{facts:?}"));
    assert_eq!(outcome.visibility, ManuscriptVisibility::Private);
    for seat in &victims {
        assert!(outcome.text.contains(&seat.name), "{outcome:?}");
        assert!(outcome.about.contains(&seat.id), "{outcome:?}");
    }
    let notice = facts
        .iter()
        .find(|fact| fact.source.contains("-disaster-2-notice"))
        .unwrap_or_else(|| panic!("天灾的公告正文应进公开剧本：{facts:?}"));
    assert_eq!(notice.visibility, ManuscriptVisibility::Public);
    assert_eq!(notice.kind, ManuscriptKind::Announcement);
    assert!(notice.text.contains("赛事天灾公告"));
    let loss = 5 * victims.len() as u32;
    assert_eq!(
        flooded.seats.iter().map(|seat| seat.rice).sum::<u32>() + loss,
        35
    );
    for seat in &flooded.seats {
        let private = notes(&world, &seat.id)
            .iter()
            .any(|note| note.contains("洪涝浸坏了你的私粮"));
        assert_eq!(private, seat.rice == 0);
    }
    let saved = serde_json::to_value(world.snapshot()).unwrap();
    let archive = &saved["game_mode"]["survival10"]["disasters"][1];
    assert_eq!(
        archive["private_losses"].as_object().unwrap().len(),
        victims.len()
    );
    let public = serde_json::to_string(&flooded.disasters).unwrap();
    assert!(!public.contains("private_losses"));
    let bulletin = world
        .contents()
        .documents
        .keys()
        .find(|id| id.ends_with("-2") && id.contains("s10-disaster"))
        .unwrap();
    let content = serde_json::to_string(&world.content_detail(bulletin).unwrap()).unwrap();
    assert!(content.contains("洪涝"));
    assert!(!content.contains("private_losses"));
    for id in &ids {
        assert!(!content.contains(id));
    }
    world
        .restore_snapshot(serde_json::from_value(saved).unwrap())
        .unwrap();
    assert_eq!(survival10(&world), flooded);
    world
        .restore_snapshot(serde_json::from_value(replay.take()).unwrap())
        .unwrap();
    world.director_game_event(&event("flood")).unwrap();
    assert_eq!(survival10(&world), flooded);
    assert!(
        world
            .take_manuscript()
            .iter()
            .filter(|fact| fact.text.contains("裁判全知") && fact.text.contains("住宅受灾"))
            .all(|fact| fact.visibility == ManuscriptVisibility::Private),
        "受灾家庭只给玩家全知视野看，「仅公开」不能出现名单"
    );
    let mut empty = serde_json::to_value(world.snapshot()).unwrap();
    empty["game_mode"]["survival10"]["public_rice"] = json!(2);
    for (index, seat) in empty["game_mode"]["survival10"]["seats"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        seat["rice"] = json!(0);
        if index >= 5 {
            seat["out"] = json!("已出局");
        }
    }
    world
        .restore_snapshot(serde_json::from_value(empty).unwrap())
        .unwrap();
    world.director_game_event(&event("rats")).unwrap();
    assert_eq!(survival10(&world).public_rice, 0);
    world.director_game_event(&event("flood")).unwrap();
    let empty = serde_json::to_value(world.snapshot()).unwrap();
    let victims = empty["game_mode"]["survival10"]["disasters"][3]["private_losses"]
        .as_object()
        .unwrap();
    assert!((1..=3).contains(&victims.len()));
    assert!(victims
        .iter()
        .all(|(seat, loss)| seat.parse::<usize>().unwrap() < 5 && loss == 0));
}

/// 按正式调用顺序走几步：每一步取走世界日志和剧本事实，不在测试里攒上一整轮。
fn survival10_ticks(
    world: &mut World,
    events: &mut Vec<Event>,
    facts: &mut Vec<kernel::ManuscriptFact>,
) {
    events.extend(world.take_events());
    facts.extend(world.take_manuscript());
    world.step(1.0);
}

/// 篝火会发言交齐后走到投票；发言只公开一句，不替任何人的选票作决定。
fn survival10_to_vote(
    world: &mut World,
    ids: &[String],
    events: &mut Vec<Event>,
    facts: &mut Vec<kernel::ManuscriptFact>,
) {
    while survival10(world).phase != Survival10Phase::Vote {
        let view = survival10(world);
        if view.phase == Survival10Phase::Speech {
            let seat = view.turn.unwrap() as usize - 1;
            world
                .game_mode_act(
                    &ids[seat],
                    &GameModeAction {
                        action: "speak".into(),
                        target: None,
                        text: Some("按公告安排。".into()),
                        reason: None,
                    },
                )
                .unwrap();
        }
        survival10_ticks(world, events, facts);
    }
    events.clear();
    facts.clear();
}

#[test]
fn survival10_choices_and_ballots_are_visible_to_the_audience_before_they_are_public() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    place_survival10_farms(&mut world, "loc_a");
    for id in &ids {
        world.test_give_rice(id, 5);
    }
    let mut events = Vec::new();
    let mut facts = Vec::new();
    while survival10(&world).phase != Survival10Phase::Day {
        survival10_ticks(&mut world, &mut events, &mut facts);
    }
    events.clear();
    facts.clear();

    survival10_act(&mut world, &ids[0], "work", None).unwrap();
    survival10_ticks(&mut world, &mut events, &mut facts);
    let choice = facts
        .iter()
        .find(|fact| fact.source.starts_with("work:"))
        .unwrap_or_else(|| panic!("登记劳动应进剧本：{facts:?}"));
    assert_eq!(choice.kind, ManuscriptKind::Choice);
    assert_eq!(choice.visibility, ManuscriptVisibility::Public);
    assert!(choice.text.contains("不等于"));
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.source.starts_with("work:"))
            .count(),
        1
    );

    survival10_to_vote(&mut world, &ids, &mut events, &mut facts);
    let day = survival10(&world).day;
    // 还差一票时先看揭票前：剧本能看到提交，旁人还看不到目标。
    let target = if ids[1] == ids[2] { &ids[3] } else { &ids[2] };
    survival10_act(&mut world, &ids[1], "vote", Some(target)).unwrap();
    facts.extend(world.take_manuscript());
    let ballot = facts
        .iter()
        .find(|fact| fact.source.starts_with("ballot:"))
        .unwrap_or_else(|| panic!("提交选票应进剧本：{facts:?}"));
    assert_eq!(ballot.kind, ManuscriptKind::Choice);
    assert_eq!(ballot.visibility, ManuscriptVisibility::Private);
    assert!(ballot.text.contains("揭票前"));
    assert!(!notes(&world, &ids[3])
        .iter()
        .any(|note| note.contains("投给")));

    // 交齐其余选票：最近一拍里只剩一人，揭票后会先进入离场陈词；剧本在选票提交时就收下。
    for (index, id) in ids.iter().enumerate().filter(|(index, _)| *index != 1) {
        let target = if index == 2 { &ids[1] } else { &ids[2] };
        survival10_act(&mut world, id, "vote", Some(target)).unwrap();
    }
    while survival10(&world).day == day && survival10(&world).phase == Survival10Phase::Vote {
        world.step(1.0);
        events.extend(world.take_events());
        facts.extend(world.take_manuscript());
    }
    assert!(
        facts
            .iter()
            .filter(|fact| fact.source.starts_with("ballot:"))
            .count()
            >= 5,
        "每位参赛者的选票都应进剧本：{facts:?}"
    );
}

#[test]
fn survival10_crop_disasters_start_tomorrow_overlap_and_recover_after_reload() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    place_survival10_farms(&mut world, "loc_a");
    disaster_board(&mut world, "loc_b");
    for id in &ids {
        world.test_give_rice(id, 10);
    }
    let event = |kind, days| {
        serde_json::from_value::<protocol::GameModeEvent>(
            json!({"kind":"survival10_disaster","event":{"kind":kind,"days":days}}),
        )
        .unwrap()
    };
    world.step(HOUR);
    world.director_game_event(&event("freeze", 2)).unwrap();
    world.director_game_event(&event("drought", 1)).unwrap();
    let notices = survival10(&world).disasters;
    assert_eq!(
        (notices[0].first_day, notices[0].last_day),
        (Some(2), Some(3))
    );
    let before = serde_json::to_value(world.snapshot()).unwrap();
    assert!(world.director_game_event(&event("freeze", 0)).is_err());
    assert_eq!(serde_json::to_value(world.snapshot()).unwrap(), before);
    for day in 1..=4 {
        for _ in 0..200 {
            let view = survival10(&world);
            if view.day == day && view.phase == Survival10Phase::Day {
                break;
            }
            if view.phase == Survival10Phase::Speech {
                let seat = view.turn.unwrap() as usize - 1;
                let _ = world.game_mode_act(
                    &ids[seat],
                    &GameModeAction {
                        action: "speak".into(),
                        target: None,
                        text: Some("我将按公告安排口粮。".into()),
                        reason: None,
                    },
                );
            }
            let current = survival10(&world);
            if current.phase == Survival10Phase::Vote {
                for seat in current.seats.iter().filter(|seat| seat.out.is_none()) {
                    let target = if seat.seat == 6 { "5" } else { "6" };
                    survival10_act(&mut world, &seat.id, "vote", Some(target)).unwrap();
                }
            }
            world.step(30.0);
        }
        let reply = survival10_act(&mut world, &ids[6], "work", None).unwrap();
        let expected = if day == 2 || day == 3 { 0 } else { 1 };
        assert_eq!(reply.contains("今日天灾绝收"), expected == 0);
        let GameModeContext::Survival10(context) = world.game_mode_context(&ids[6]).unwrap() else {
            panic!()
        };
        assert_eq!(context.now.contains("绝收"), day <= 3);
        let public = survival10(&world).public_rice;
        let saved = serde_json::to_value(world.snapshot()).unwrap();
        world
            .restore_snapshot(serde_json::from_value(saved).unwrap())
            .unwrap();
        for _ in 0..30 {
            if survival10(&world).phase != Survival10Phase::Day {
                break;
            }
            world.step(30.0);
        }
        assert_eq!(survival10(&world).produced_today, expected);
        assert_eq!(survival10(&world).public_rice, public + expected);
    }
    let mut late = serde_json::to_value(world.snapshot()).unwrap();
    late["game_mode"]["survival10"]["day"] = json!(10);
    world
        .restore_snapshot(serde_json::from_value(late).unwrap())
        .unwrap();
    let before = serde_json::to_value(world.snapshot()).unwrap();
    assert!(world.director_game_event(&event("drought", 1)).is_err());
    assert_eq!(serde_json::to_value(world.snapshot()).unwrap(), before);
}

/// 一座圆岛：最高处在 (24, 18)，离它约 14.6 米以外是海。甲站在最高处，乙站在离它 12 米的东岸。
fn island() -> World {
    island_with(&[("a", "甲", "loc_a"), ("b", "乙", "loc_b")])
}

/// 同一座岛，住进这些人：loc_a 的落脚点在最高处，loc_b 的在东岸。
fn island_with(people: &[(&str, &str, &str)]) -> World {
    island_on(common::tiny_topology(), people)
}

fn island_on(topo: kernel::Topology, people: &[(&str, &str, &str)]) -> World {
    let mut w = common::world_with(topo, 7.0 * HOUR, people);
    let (columns, rows) = (41, 41);
    let heights = (0..rows)
        .flat_map(|z| {
            (0..columns)
                .map(move |x| 2.0 - (f64::from(x) - GOAL[0]).hypot(f64::from(z) - GOAL[1]) / 7.5)
        })
        .collect();
    let spot = |locale: &str, position| Spot {
        locale: locale.into(),
        space: "town_outdoor".into(),
        position,
    };
    w.walk_map = Some(Arc::new(WalkMap {
        version: 1,
        svg_units_per_metre: 10.,
        metres_per_minute: 12.,
        outdoor_space: "town_outdoor".into(),
        surfaces: ["loc_a", "loc_b", "edge_ab"]
            .into_iter()
            .map(|locale| Surface {
                locale: locale.into(),
                space: "town_outdoor".into(),
                area: Area::Rect {
                    min: [0., 0.],
                    max: [40., 40.],
                },
            })
            .collect(),
        stations: BTreeMap::from([
            ("st_a".into(), spot("loc_a", GOAL)),
            ("st_b".into(), spot("loc_b", [36., 18.])),
        ]),
        terrain: Some(Terrain {
            origin: [0., 0.],
            step: 1.0,
            columns: columns as usize,
            rows: rows as usize,
            heights,
        }),
        sea_level: 0.0,
        ..Default::default()
    }));
    // 只留精力在变，五天里没人吃喝睡也不会死，方便看潮水多扣的那份。
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: true,
        water: false,
        health: false,
        weather: false,
    });
    w
}

fn logs(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            let Event::Log { text, .. } = e;
            text.clone()
        })
        .collect()
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn tide(w: &World) -> Option<TideView> {
    match w.game_mode_view() {
        Some(GameModeView::Tide(v)) => Some(v),
        _ => None,
    }
}

/// 没开玩法时存档和名册里都没有玩法的字段，和没有玩法这回事时一样。
fn no_trace(w: &World) {
    let snapshot = serde_json::to_value(w.snapshot()).unwrap();
    assert!(snapshot.get("game_mode").is_none());
    let roster = serde_json::to_value(w.roster(1.0)).unwrap();
    assert!(roster.get("game_mode").is_none());
}

fn rested(w: &mut World) {
    for id in ["a", "b"] {
        w.adjust_body(
            id,
            NeedValues {
                fullness: None,
                energy: Some(90.0),
                water: None,
                health: None,
            },
        )
        .unwrap();
    }
}

#[test]
fn tide_rings_shrink_onto_the_high_ground_and_drain_whoever_stands_in_water() {
    let mut w = island();
    assert!(w.game_mode().is_none());
    no_trace(&w);
    // 第 1 天 07:00 打开：今天的 06:00 已过，第 1 圈第 2 天天亮公布。
    w.set_game_mode(Some(&GameModeChoice::Tide { seed: Some(7) }))
        .unwrap();
    assert_eq!(w.game_mode(), Some("tide"));
    let v = tide(&w).unwrap();
    assert_eq!((v.phase, v.ring, v.rings), (TidePhase::Waiting, 0, 5));
    let mut previous: TideCircle = v.edge;
    assert!(dist(previous.center, GOAL) + 0.1 < previous.radius);

    // 第 2 天 08:00：第 1 圈已公布，所有人都收到公告，公告里写着还剩谁；第一只木箱冲上来。
    let events = w.step(25.0 * HOUR);
    assert!(logs(&events).iter().any(
        |t| t.starts_with("【潮水】第 1 圈已公布") && t.ends_with("岛上还剩 2 个人：甲、乙。")
    ));
    let crate_lines = |events: &[Event]| {
        logs(events)
            .iter()
            .filter(|t| t.contains("潮水冲上来一只木箱"))
            .count()
    };
    let mut crates = crate_lines(&events);
    let v = tide(&w).unwrap();
    assert_eq!((v.phase, v.ring), (TidePhase::Announced, 1));
    let first = v.next.unwrap();

    // 15:00 正在涨：水边在两圈之间；存档读档后接着涨到同一个位置。
    w.step(7.0 * HOUR);
    let v = tide(&w).unwrap();
    assert_eq!(v.phase, TidePhase::Rising);
    assert!(v.edge.radius < previous.radius && v.edge.radius > first.radius);
    let mut restored = island();
    restored.restore_snapshot(w.snapshot()).unwrap();
    assert_eq!(tide(&restored), tide(&w));
    w = restored;

    // 旧存档没有时间表：照当时每天一圈接着涨，第 3 天 13:00 还在涨第 2 圈。
    let mut old = serde_json::to_value(w.snapshot()).unwrap();
    old["game_mode"]["tide"]
        .as_object_mut()
        .unwrap()
        .remove("timetable")
        .unwrap();
    let mut daily = island();
    daily
        .restore_snapshot(serde_json::from_value(old).unwrap())
        .unwrap();
    daily.step(2.0 * DAY + 13.0 * HOUR - daily.minutes());
    let v = tide(&daily).unwrap();
    assert_eq!((v.phase, v.ring), (TidePhase::Rising, 2));

    // 后两天上午、下午各收一圈。每一圈涨到时水边停在那一圈：都在上一圈里、都包住最高处；中午涨到的那一刻
    // 下一圈接着公布。第 1 到第 3 圈公布时各冲上来一只木箱，菜刀那箱在前两箱里。
    let mut lines = Vec::new();
    for (k, day, hour) in [
        (1, 1.0, 18.0),
        (2, 2.0, 12.0),
        (3, 2.0, 18.0),
        (4, 3.0, 12.0),
    ] {
        let events = w.step(day * DAY + hour * HOUR + 30.0 - w.minutes());
        crates += crate_lines(&events);
        lines = logs(&events);
        let v = tide(&w).unwrap();
        let expected = if hour < 18.0 {
            (TidePhase::Announced, k + 1)
        } else {
            (TidePhase::Holding, k)
        };
        assert_eq!((v.phase, v.ring), expected);
        assert!(dist(v.edge.center, previous.center) + v.edge.radius <= previous.radius + 1e-6);
        // 终点一带（14 米，圈小时四分之三个半径）一直在圈里：圈心离终点不远于剩下的那点
        let slack = (v.edge.radius - 14.0).max(v.edge.radius * 0.25);
        assert!(dist(v.edge.center, GOAL) <= slack + 1e-6);
        previous = v.edge;
        if k == 1 {
            let Some(GameModeContext::Tide(context)) = w.game_mode_context("a") else {
                panic!("甲的模型该看到潮水这一栏");
            };
            assert_eq!(context.notice, "潮水停在第 1 圈；明天 06:00 公布下一圈。");
        }
        if k == 2 {
            let snapshot = serde_json::to_value(w.snapshot()).unwrap();
            assert!(snapshot["placed_props"]
                .as_object()
                .unwrap()
                .values()
                .any(|p| p["definition"]["id"] == "tide:crate-kitchen_knife"));
        }
    }
    assert_eq!(crates, 3);
    // 第 4 天中午第 4 圈涨到，最后一圈公布，船停到最高处，只坐一个人。
    assert!(lines.iter().any(|t| t
        .starts_with("【潮水】最后一圈已公布：今天 14:00 开始涨，18:00 前水会漫过全岛。")
        && t.contains("只坐得下一个人")));

    // 第 4 圈只剩山顶一小块：乙站在水里，比站在山顶的甲每小时多掉 6 点精力。
    rested(&mut w);
    w.step(HOUR);
    let energy = |w: &World, id: &str| w.actor(id).unwrap().needs.energy.value;
    let (a, b) = (energy(&w, "a"), energy(&w, "b"));
    assert!((a - b - 6.0).abs() < 0.5, "甲 {a} 乙 {b}");
    let Some(GameModeContext::Tide(context)) = w.game_mode_context("b") else {
        panic!("开着潮水圈时乙的模型该看到潮水这一栏");
    };
    assert!(context.standing.unwrap().starts_with("你站在潮水里"));

    // 乙在水里的棚子上睡觉：几分钟内就被泡醒，通知里写着往哪边走能回到圈里
    let hut: kernel::props::Definition = serde_json::from_value(json!({
        "id":"core:island-shelter","version":1,"name":"棚子","shelter":true,
        "actions":[{"id":"sleep","label":"在棚子里睡觉","duration_minutes":480,"completion":"sleep","rest":true}]
    }))
    .unwrap();
    w.place_prop_near(
        serde_json::from_value(json!({
            "instance":{"id":"hut","definition":hut.reference(),"locale":"loc_b"},
            "definition":hut
        }))
        .unwrap(),
        Some(&protocol::PlaceNear {
            space: "town_outdoor".into(),
            position: [36., 18.],
        }),
    )
    .unwrap();
    common::run(
        &mut w,
        "b",
        "nap",
        json!({"prop_id":"hut","intent":{"verb":"在棚子里睡觉"}}),
    )
    .unwrap();
    w.step(10.0);
    assert_eq!(
        w.action_group("b", "nap").unwrap().status,
        GroupStatus::Interrupted
    );
    let woken = w
        .actor("b")
        .unwrap()
        .journal
        .events_after(0)
        .iter()
        .any(|e| {
            e.kind == FactKind::Notice && e.text.contains("潮水漫上来，把你泡醒了：你站在潮水里")
        });
    assert!(woken, "乙该被泡醒");

    // 没人去登船，第 4 天 18:00 水漫过全岛，船带走离它最近的甲。
    let events = w.step(3.0 * DAY + 18.0 * HOUR - w.minutes());
    let lines = logs(&events);
    assert!(lines.iter().any(|t| t == "【潮水】水漫过了全岛。"));
    assert!(lines
        .iter()
        .any(|t| t == "【潮水】甲离船最近，被船带走了，是这一局唯一活着离开的人。"));
    let v = tide(&w).unwrap();
    assert_eq!((v.phase, v.edge.radius), (TidePhase::Over, 0.0));
    assert_eq!(v.edge.center, GOAL);
    assert_eq!(v.winner.as_deref(), Some("a"));
    assert!(!w.actor("a").unwrap().present());

    // 关掉以后不留痕迹
    w.set_game_mode(None).unwrap();
    assert!(w.game_mode().is_none());
    no_trace(&w);
}

/// 乙站的地方只有导航空间知道、行走地图里没有：开局布置的棚子、石屋和船的操作位都是这样，只在导航覆盖里。
/// 在那儿照样算站在水里，睡着了照样被泡醒。第二到第五局这些地方一直不算在水里，泡醒一次也没发生。
#[test]
fn tide_wakes_a_sleeper_whose_spot_only_navigation_knows() {
    let topo = common::topology(&common::two_places_json(|data| {
        data["props"].as_array_mut().unwrap().push(json!({
            "id":"hut","name":"棚子","locale":"loc_b",
            "verbs":[{"verb":"在棚子里睡觉","duration_minutes":480,
              "action":{"id":"sleep","label":"在棚子里睡觉","duration_minutes":480,"completion":"sleep","rest":true,"during":[],"conditions":[]}}]
        }));
    }));
    let mut w = island_on(topo, &[("a", "甲", "loc_a"), ("b", "乙", "loc_b")]);
    let mut map = (**w.walk_map.as_ref().unwrap()).clone();
    map.stations.remove("st_b");
    w.walk_map = Some(Arc::new(map));
    w.topo.navigation = Some(
        serde_json::from_value(json!({
            "navigation_revision": 1, "floors": [], "props": [], "routes": [],
            "stations": [{"id": "st_b", "locale": "loc_b", "position": [36.0, 0.4, 18.0]}]
        }))
        .unwrap(),
    );
    w.set_game_mode(Some(&GameModeChoice::Tide { seed: Some(7) }))
        .unwrap();
    // 第 3 天傍晚第 3 圈涨到，离最高处 12 米的乙早已在水里
    w.step(2.0 * DAY + 18.5 * HOUR - w.minutes());
    let Some(GameModeContext::Tide(context)) = w.game_mode_context("b") else {
        panic!("乙的模型该看到潮水这一栏");
    };
    assert!(context.standing.unwrap().starts_with("你站在潮水里"));
    common::run(
        &mut w,
        "b",
        "nap",
        json!({"prop_id":"hut","intent":{"verb":"在棚子里睡觉"}}),
    )
    .unwrap();
    w.step(10.0);
    assert_eq!(
        w.action_group("b", "nap").unwrap().status,
        GroupStatus::Interrupted
    );
    assert!(w
        .actor("b")
        .unwrap()
        .journal
        .events_after(0)
        .iter()
        .any(|e| {
            e.kind == FactKind::Notice && e.text.contains("潮水漫上来，把你泡醒了：你站在潮水里")
        }));
}

const KNIFE: &str = r#"{"id":"core:kitchen_knife","version":1,"name":"菜刀","description":"一把厚背菜刀，刀口很利。","portable":true,
 "actions":[{"id":"stab","label":"持刀袭击","duration_minutes":3,"completion":"activity",
  "on_complete":[{"kind":"harm","amount":100,"wound":"胸口一处刀伤"}],"recipient":true}]}"#;

#[test]
fn tide_hands_the_boat_to_the_last_one_standing_and_keeps_the_fights_in_its_news() {
    // 三个人都在最高处；只看健康和精力。
    let people = [
        ("a", "甲", "loc_a"),
        ("b", "乙", "loc_a"),
        ("c", "丙", "loc_a"),
    ];
    let mut w = island_with(&people);
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: true,
        water: false,
        health: true,
        weather: false,
    });
    w.set_game_mode(Some(&GameModeChoice::Tide { seed: Some(7) }))
        .unwrap();
    // 开局每人一张纸条，模型一直看得到目标
    let events = w.step(1.0);
    assert!(logs(&events)
        .iter()
        .any(|t| t.starts_with("【纸条】") && t.contains("最后一圈在第 4 天 12:00 公布")));
    let Some(GameModeContext::Tide(context)) = w.game_mode_context("c") else {
        panic!("丙的模型该看到潮水这一栏");
    };
    assert!(context.goal.starts_with("只有一个人能活着离开"));
    assert!(context
        .goal
        .ends_with("眼下岛上还活着 3 个人：甲、乙、丙。"));
    assert_eq!(tide(&w).unwrap().players.len(), 3);

    // 甲拿地上的菜刀刺死乙：战报里有这一刀，名册里写着乙怎么死的
    let knife: kernel::props::Definition = serde_json::from_str(KNIFE).unwrap();
    w.place_prop(
        serde_json::from_value(json!({
            "instance":{"id":"knife","definition":knife.reference(),"locale":"loc_a"},
            "definition":knife
        }))
        .unwrap(),
    )
    .unwrap();
    let stab = |w: &mut World, key: &str, whom: &str| {
        common::run(
            w,
            "a",
            key,
            json!({"prop_id":"knife","intent":{"verb":"持刀袭击"},"recipient":whom}),
        )
        .unwrap();
        common::finish(w, "a", key);
        assert_eq!(
            w.action_group("a", key).unwrap().status,
            GroupStatus::Succeeded
        );
    };
    stab(&mut w, "stab-b", "b");
    w.step(5.0);
    let v = tide(&w).unwrap();
    assert!(v
        .news
        .iter()
        .any(|n| n.kind == TideNewsKind::Clash && n.text == "甲持刀袭击乙，健康 −100，乙倒下了"));
    let fell = |v: &TideView, id: &str| {
        v.players
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.fell.clone())
    };
    assert_eq!(fell(&v, "b").as_deref(), Some("被甲杀死（胸口一处刀伤）"));
    assert!(v.winner.is_none());

    // 甲又刺死丙：只剩甲，船提前开来接走他，这一局结束，读档后结果还在
    stab(&mut w, "stab-c", "c");
    let events = w.step(5.0);
    assert!(logs(&events)
        .iter()
        .any(|t| t == "【潮水】岛上只剩甲一个人了，船提前开了过来。甲是这一局唯一活着离开的人。"));
    let v = tide(&w).unwrap();
    assert_eq!((v.phase, v.winner.as_deref()), (TidePhase::Over, Some("a")));
    assert!(fell(&v, "c").is_some());
    assert!(!w.actor("a").unwrap().present());
    assert_eq!(w.game_mode_tempo(), 1.0);
    let mut restored = island_with(&people);
    restored.restore_snapshot(w.snapshot()).unwrap();
    assert_eq!(tide(&restored), tide(&w));
}

/// 船只认信号弹：第 1 圈冲上来的铁皮匣撬开就拿到，换了人公告全岛、记进战报，题面写着在谁身上；
/// 没带的人登不了船，登船途中被人抢就登不成，拿着的人登上去赢、信号弹打出去用掉。
#[test]
fn tide_boat_takes_only_whoever_carries_the_flare() {
    let people = [
        ("a", "甲", "loc_a"),
        ("b", "乙", "loc_a"),
        ("c", "丙", "loc_a"),
    ];
    let mut w = island_with(&people);
    let rest = |w: &mut World| {
        for id in ["a", "b", "c"] {
            w.adjust_body(
                id,
                NeedValues {
                    fullness: None,
                    energy: Some(90.0),
                    water: None,
                    health: None,
                },
            )
            .unwrap();
        }
    };
    let flare_line = |w: &World, id: &str| {
        let Some(GameModeContext::Tide(context)) = w.game_mode_context(id) else {
            panic!("开着潮水圈时模型该看到潮水这一栏");
        };
        context.flare.unwrap_or_default()
    };
    w.set_game_mode(Some(&GameModeChoice::Tide { seed: Some(7) }))
        .unwrap();
    let events = w.step(1.0);
    assert!(logs(&events)
        .iter()
        .any(|t| t.starts_with("【纸条】") && t.contains("船上的人只认信号弹")));

    // 第 2 天 06:00 第 1 圈公布，铁皮匣跟着木箱冲上来；甲撬开拿到信号弹，全岛都知道
    let events = w.step(DAY + 6.0 * HOUR + 1.0 - w.minutes());
    assert!(logs(&events)
        .iter()
        .any(|t| t.contains("潮水还冲上来一只铁皮匣")));
    assert_eq!(tide(&w).unwrap().flare, Some(TideFlare::Case));
    rest(&mut w);
    common::run(
        &mut w,
        "a",
        "open",
        json!({"prop_id":"tide-flare-case","intent":{"verb":"撬开铁皮匣"}}),
    )
    .unwrap();
    common::finish(&mut w, "a", "open");
    let events = w.step(5.0);
    assert!(logs(&events)
        .iter()
        .any(|t| t == "【潮水】甲撬开了铁皮匣，拿到了信号弹。"));
    let v = tide(&w).unwrap();
    assert_eq!(v.flare, Some(TideFlare::Held { id: "a".into() }));
    assert!(v
        .news
        .iter()
        .any(|n| n.kind == TideNewsKind::Flare && n.text == "甲撬开了铁皮匣，拿到了信号弹。"));
    assert!(flare_line(&w, "a").starts_with("信号弹在你身上"));
    assert_eq!(flare_line(&w, "b"), "信号弹在甲身上，甲此刻在「甲地」。");

    // 第 4 天 12:00 船来了。丙没带信号弹，登不了船
    w.step(3.0 * DAY + 12.0 * HOUR + 1.0 - w.minutes());
    rest(&mut w);
    let board = |w: &mut World, who: &str, key: &str| {
        common::run(
            w,
            who,
            key,
            json!({"prop_id":"tide-boat","intent":{"verb":"登船"}}),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    };
    let refused = board(&mut w, "c", "board-c").unwrap_err();
    assert!(refused.contains("需要随身带着信号弹×1"), "{refused}");

    // 甲在登船，乙当面抢他的信号弹：不管抢没抢到，甲的登船都被打断；乙抢到为止
    board(&mut w, "a", "board-a").unwrap();
    w.step(1.0);
    let flare = w
        .topo
        .props()
        .iter()
        .find(|p| p.definition.as_deref() == Some("tide:flare@1"))
        .unwrap()
        .id
        .clone();
    for i in 0..12 {
        let key = format!("rob-{i}");
        common::run(
            &mut w,
            "b",
            &key,
            json!({"intent":{"verb":"抢"},"recipient":"a","prop_id":flare}),
        )
        .unwrap();
        common::finish(&mut w, "b", &key);
        if w.inventory().carried_by(&flare, "b") {
            break;
        }
    }
    assert_eq!(
        w.action_group("a", "board-a").unwrap().status,
        GroupStatus::Interrupted
    );
    assert!(w.inventory().carried_by(&flare, "b"), "乙该抢到了信号弹");
    let events = w.step(5.0);
    assert!(logs(&events)
        .iter()
        .any(|t| t == "【潮水】信号弹从甲身上到了乙身上。"));
    // 战报里那一下抢记成信号弹的事，不重复记换手
    let news = tide(&w).unwrap().news;
    assert!(news
        .iter()
        .any(|n| n.kind == TideNewsKind::Flare && n.text == "乙从甲手里抢走了信号弹"));
    assert!(!news.iter().any(|n| n.text.starts_with("信号弹从甲身上")));
    let refused = board(&mut w, "a", "board-a2").unwrap_err();
    assert!(refused.contains("需要随身带着信号弹×1"), "{refused}");

    // 乙登上去：赢了，信号弹打了出去
    board(&mut w, "b", "board-b").unwrap();
    common::finish(&mut w, "b", "board-b");
    let events = w.step(5.0);
    assert!(logs(&events)
        .iter()
        .any(|t| t == "【潮水】乙在船边打出信号弹，登上了船，是这一局唯一活着离开的人。"));
    let v = tide(&w).unwrap();
    assert_eq!((v.phase, v.winner.as_deref()), (TidePhase::Over, Some("b")));
    assert!(w
        .topo
        .props()
        .iter()
        .all(|p| p.withdrawn || p.definition.as_deref() != Some("tide:flare@1")));
    let mut restored = island_with(&people);
    restored.restore_snapshot(w.snapshot()).unwrap();
    assert_eq!(tide(&restored), tide(&w));
}

fn werewolf(w: &World) -> WerewolfView {
    match w.game_mode_view() {
        Some(GameModeView::Werewolf(v)) => v,
        other => panic!("开着的该是狼人杀：{other:?}"),
    }
}

fn wolf_context(w: &World, id: &str) -> protocol::WerewolfContext {
    match w.game_mode_context(id) {
        Some(GameModeContext::Werewolf(c)) => c,
        other => panic!("{id} 的模型该看到狼人杀这一栏：{other:?}"),
    }
}

fn mode_act(w: &mut World, id: &str, action: &str, target: Option<u32>, text: Option<&str>) {
    let action = GameModeAction {
        action: action.into(),
        target: target.map(|t| t.to_string()),
        text: text.map(str::to_owned),
        reason: Some(format!("{id} 心里这么想")),
    };
    let result = w.game_mode_act(id, &action);
    assert!(result.is_ok(), "{id} 交 {action:?}：{result:?}");
}

/// 一局 9 人：用给模型看的那一栏（werewolf.actions）当成模型，按固定打法交动作，走完整局。
/// 好人每天都投座号最小的狼，狼人每夜刀座号最小的平民；女巫第 1 夜救人，猎人不开枪。
/// 途中故意让一个人发言超时、一个人不投票（到点往下走），议事中途存档读档。
#[test]
fn werewolf_judge_runs_a_whole_game_through_turns_deadlines_and_a_reload() {
    let names = [
        "阿树", "小禾", "芬恩", "露娜", "石头", "米拉", "诺亚", "阿滩", "阿岚",
    ];
    let ids: Vec<String> = (1..=9).map(|n| format!("r{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .zip(names)
        .map(|(id, name)| (id.as_str(), name, "loc_a"))
        .collect();
    // 吃喝睡关着（预制小镇的默认），四天里没人饿死渴死
    let fresh = || {
        let mut w = island_with(&people);
        w.set_physiology(PhysiologySwitches {
            fullness: false,
            energy: false,
            water: false,
            health: false,
            weather: false,
        });
        w
    };
    let mut w = fresh();
    // 人不够开不了，原来的世界不受影响
    let mut few = common::world_with(common::tiny_topology(), 7.0 * HOUR, &people[..5]);
    assert!(few
        .set_game_mode(Some(&GameModeChoice::Werewolf {
            seed: Some(3),
            day_hours: None
        }))
        .is_err());
    assert!(few.game_mode().is_none());

    w.set_game_mode(Some(&GameModeChoice::Werewolf {
        seed: Some(11),
        day_hours: Some(6),
    }))
    .unwrap();
    assert_eq!(w.game_mode(), Some("werewolf"));
    let v = werewolf(&w);
    assert_eq!(
        (v.phase, v.day, v.seats.len()),
        (WerewolfPhase::Waiting, 0, 9)
    );
    let count = |role| v.seats.iter().filter(|s| s.role == role).count();
    assert_eq!(
        (count(WerewolfRole::Wolf), count(WerewolfRole::Villager)),
        (3, 3)
    );
    let role_of = |seat: u32| v.seats[seat as usize - 1].role;
    let seat_of = |role| v.seats.iter().find(|s| s.role == role).unwrap().seat;
    let (seer, witch, hunter) = (
        seat_of(WerewolfRole::Seer),
        seat_of(WerewolfRole::Witch),
        seat_of(WerewolfRole::Hunter),
    );
    let villagers: Vec<u32> = v
        .seats
        .iter()
        .filter(|s| s.role == WerewolfRole::Villager)
        .map(|s| s.seat)
        .collect();
    let wolves: Vec<u32> = v
        .seats
        .iter()
        .filter(|s| s.role == WerewolfRole::Wolf)
        .map(|s| s.seat)
        .collect();
    // 开局前就有玩法动作工具（整局工具表不变，请求前缀能留在缓存里），但没有要交的
    assert!(w.game_mode_tool("r1").unwrap().contains("vote"));
    assert!(wolf_context(&w, "r1").actions.is_empty());

    // 没开玩法的世界按正常速度走；开局前是空档，走快一点
    assert_eq!(few.game_mode_tempo(), 1.0);
    assert_eq!(w.game_mode_tempo(), 2.0);
    // 第 1 天 10:30 天亮：每人单独收到自己的身份，狼人还知道同伴
    w.step(3.6 * HOUR);
    assert_eq!(werewolf(&w).phase, WerewolfPhase::Day);
    for (k, id) in ids.iter().enumerate() {
        let a = w.actor(id).unwrap();
        let notice = a
            .journal
            .events_after(0)
            .iter()
            .find(|e| e.kind == FactKind::Notice && e.text.contains("身份是"))
            .unwrap_or_else(|| panic!("{id} 没收到身份"))
            .text
            .clone();
        let seat = k as u32 + 1;
        assert_eq!(
            notice.contains("狼人同伴"),
            role_of(seat) == WerewolfRole::Wolf
        );
    }

    let id_of = |seat: u32| ids[seat as usize - 1].clone();
    let alive = |w: &World, seat: u32| werewolf(w).seats[seat as usize - 1].out.is_none();
    let mut skipped = false;
    let mut abstained = false;
    let mut reloaded = false;
    let mut told_to_stay = false;
    let mut died_at_night = false;
    let mut heard_reason = false;
    let mut possessed_exile = None;
    // 世界日志：中途读档换了世界，边走边收
    let mut logged = vec![];
    for _ in 0..5000 {
        logged.extend(w.take_events());
        let v = werewolf(&w);
        if v.phase == WerewolfPhase::Over {
            break;
        }
        if v.phase == WerewolfPhase::LastWords && possessed_exile.is_none() {
            let id = id_of(v.turn.unwrap());
            w.possess(&id).unwrap();
            possessed_exile = Some(id);
        }
        // 发言、投票这些有期限的回合照常速；发言记下了交的时候写的打算（只给旁观的人看）
        if v.phase == WerewolfPhase::Speech {
            assert_eq!(w.game_mode_tempo(), 1.0);
            if let Some(line) = v.lines.last() {
                heard_reason |= line
                    .reason
                    .as_deref()
                    .is_some_and(|r| r.ends_with("心里这么想"));
            }
        }
        // 第 2 夜的刀口没人救：夜里的选择交齐就结算，人已经倒下，天亮前座位上还不公布
        if let Some(n) = v.night.as_ref().filter(|n| n.night == 2 && !died_at_night) {
            if v.phase == WerewolfPhase::Night && w.minutes() % (24.0 * HOUR) >= 20.0 * HOUR {
                let k = n.kill.expect("第 2 夜狼人选了刀口");
                assert!(w.actor(&id_of(k)).unwrap().needs.body.dead);
                assert_eq!(v.seats[k as usize - 1].out, None);
                assert_eq!(w.game_mode_tempo(), 2.0);
                died_at_night = true;
            }
        }
        // 黄昏回家：停留只认时长，法官把离天亮（10:30）还差几分钟算好
        if v.phase == WerewolfPhase::Dusk && !told_to_stay {
            told_to_stay = true;
            let now = wolf_context(&w, "r1").now;
            let left = (24.0 * HOUR + 10.5 * HOUR - (w.minutes() % (24.0 * HOUR))).ceil();
            assert!(
                now.contains(&format!("duration_minutes 填 {left:.0}")),
                "{now}"
            );
        }
        // 第 2 天第三个发言的人不交，到点跳过
        if v.phase == WerewolfPhase::Speech && v.day == 2 && v.lines.len() == 2 && !skipped {
            skipped = true;
            let silent = v.turn.unwrap();
            w.step(16.0);
            let after = werewolf(&w);
            assert_ne!(after.turn, Some(silent));
            assert_eq!(after.lines.len(), 2);
            continue;
        }
        // 第 3 天议事说到一半存档，读进一个新世界接着走
        if v.phase == WerewolfPhase::Speech && v.day == 3 && v.lines.len() == 2 && !reloaded {
            reloaded = true;
            w = common::restore(&w, fresh());
            assert_eq!(werewolf(&w), v);
        }
        let mut acted = false;
        for seat in 1..=9u32 {
            let id = id_of(seat);
            let Some(GameModeContext::Werewolf(c)) = w.game_mode_context(&id) else {
                continue;
            };
            let Some(first) = c.actions.first().cloned() else {
                continue;
            };
            let v = werewolf(&w);
            let lowest = |pred: &dyn Fn(u32) -> bool| (1..=9u32).find(|&s| alive(&w, s) && pred(s));
            match first.as_str() {
                "speak" => mode_act(&mut w, &id, "speak", None, Some("我是好人，大家听我说。")),
                "vote" | "abstain" => {
                    // 第 2 天最后一个人不投，到点算弃票
                    let voted = v.ballots.len();
                    let living = v.seats.iter().filter(|s| s.out.is_none()).count();
                    if v.day == 2 && voted + 1 == living && !abstained {
                        abstained = true;
                        w.step(16.0);
                        assert!(werewolf(&w).revealed);
                        acted = true;
                        break;
                    }
                    let target = if role_of(seat) == WerewolfRole::Wolf {
                        lowest(&|s| !wolves.contains(&s))
                    } else {
                        lowest(&|s| wolves.contains(&s))
                    };
                    mode_act(&mut w, &id, "vote", target, None);
                }
                "kill" => {
                    let target = lowest(&|s| villagers.contains(&s));
                    mode_act(&mut w, &id, "kill", target, Some("刀他。"));
                }
                "check" => {
                    let target = lowest(&|s| s != seat);
                    mode_act(&mut w, &id, "check", target, None);
                }
                "poison" | "save" | "pass" if seat == witch => {
                    let night = v.night.as_ref().map_or(0, |n| n.night);
                    let save = night == 1 && c.actions.iter().any(|a| a == "save");
                    mode_act(&mut w, &id, if save { "save" } else { "pass" }, None, None);
                }
                _ => mode_act(&mut w, &id, "pass", None, None),
            }
            acted = true;
            break;
        }
        if !acted {
            w.step(5.0);
        }
    }
    assert!(skipped && abstained && reloaded && told_to_stay && died_at_night && heard_reason);
    let exiled = w.actor(possessed_exile.as_deref().unwrap()).unwrap();
    assert!(!exiled.present());
    assert!(!exiled.needs.body.dead);
    assert!(w.possessed().is_none(), "被放逐的人不能继续占用玩家控制");
    let v = werewolf(&w);
    // 议事、遗言、狼人频道写进日志时都带着「一段话」：发言人、座号、场合和原话，正文还是原来那一行
    logged.extend(w.take_events());
    let mut occasions = std::collections::BTreeSet::new();
    for Event::Log { text, info, .. } in &logged {
        let Some(said) = &info.speech else {
            continue;
        };
        let seat = said.seat.expect("狼人杀的发言带座号");
        let actor = info.actor.as_ref().expect("发言带发言人");
        assert_eq!(actor.id, id_of(seat));
        let name = &v.seats[seat as usize - 1].name;
        assert_eq!(&actor.name, name);
        assert!(text.contains(&format!("{seat} 号 {name}")), "{text}");
        assert!(text.ends_with(&format!("：{}", said.text)), "{text}");
        occasions.insert(said.occasion.clone());
    }
    for occasion in ["议事", "遗言", "狼人频道"] {
        assert!(
            occasions.contains(occasion),
            "日志里没有{occasion}：{occasions:?}"
        );
    }
    assert_eq!(
        (v.phase, v.winner),
        (WerewolfPhase::Over, Some(WerewolfSide::Good))
    );
    for &wolf in &wolves {
        assert_eq!(v.seats[wolf as usize - 1].out, Some(WerewolfOut::Exiled));
    }
    // 第 1 夜刀的平民被女巫救下，第 2 夜又被刀倒下，第 3 夜倒下第二个；最后一个平民和神职都活着
    assert_eq!(v.seats[villagers[2] as usize - 1].out, None);
    for &k in &villagers[..2] {
        assert_eq!(v.seats[k as usize - 1].out, Some(WerewolfOut::Killed));
        assert!(w.actor(&id_of(k)).unwrap().needs.body.dead);
    }
    for seat in [seer, witch, hunter] {
        assert_eq!(v.seats[seat as usize - 1].out, None);
    }
    let history = wolf_context(&w, &id_of(seer)).history;
    assert!(
        history.iter().any(|h| h == "第 1 夜：平安夜"),
        "{history:?}"
    );
    assert!(history.iter().any(|h| h.contains("弃票")), "{history:?}");
    // 被放逐的人说完遗言走出村子；出局的人和结束后都没有玩法动作工具
    w.step(30.0);
    for &wolf in &wolves {
        assert!(!w.actor(&id_of(wolf)).unwrap().present());
        assert!(w.game_mode_tool(&id_of(wolf)).is_none());
    }
    assert!(w.game_mode_tool(&id_of(seer)).is_none());

    w.set_game_mode(None).unwrap();
    no_trace(&w);
}

/// 同一个刀口：仅被刀的猎人天亮后仍能开枪，同夜被毒则当晚倒下，不能获得开枪回合。
#[test]
fn werewolf_hunter_cannot_shoot_when_killed_and_poisoned_the_same_night() {
    let ids: Vec<String> = (1..=9).map(|n| format!("r{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .map(|id| (id.as_str(), id.as_str(), "loc_a"))
        .collect();
    for poisoned in [false, true] {
        let mut w = common::world_with(common::tiny_topology(), 7.0 * HOUR, &people);
        w.set_physiology(PhysiologySwitches {
            fullness: false,
            energy: false,
            water: false,
            health: false,
            weather: false,
        });
        w.set_game_mode(Some(&GameModeChoice::Werewolf {
            seed: Some(11),
            day_hours: Some(6),
        }))
        .unwrap();
        let v = werewolf(&w);
        let hunter = v
            .seats
            .iter()
            .find(|s| s.role == WerewolfRole::Hunter)
            .unwrap();
        let (hunter_seat, hunter_id) = (hunter.seat, hunter.id.clone());
        let witch = v
            .seats
            .iter()
            .find(|s| s.role == WerewolfRole::Witch)
            .unwrap();
        let witch_seat = witch.seat;

        // 从开局、逐人发言推进到第一夜。只交题面当前允许的动作，不直接改玩法状态。
        for _ in 0..1000 {
            let v = werewolf(&w);
            let mut acted = false;
            for id in &ids {
                let c = wolf_context(&w, id);
                let Some(first) = c.actions.first() else {
                    continue;
                };
                let (action, target) = match first.as_str() {
                    "speak" => ("pass", None),
                    "kill" => ("kill", Some(hunter_seat)),
                    "check" => ("check", Some(witch_seat)),
                    "poison" | "save" | "pass" if v.phase == WerewolfPhase::Night => {
                        if poisoned {
                            ("poison", Some(hunter_seat))
                        } else {
                            ("pass", None)
                        }
                    }
                    other => panic!("第一夜前不该有 {other}"),
                };
                mode_act(&mut w, id, action, target, None);
                acted = true;
                break;
            }
            if !acted {
                w.step(0.1);
                if v.phase == WerewolfPhase::Night {
                    break;
                }
                w.step(5.0);
            }
        }
        let v = werewolf(&w);
        assert_eq!((v.phase, v.day, v.winner), (WerewolfPhase::Night, 1, None));
        let night = v.night.as_ref().unwrap();
        assert_eq!(night.kill, Some(hunter_seat));
        assert_eq!(night.poison, poisoned.then_some(hunter_seat));
        assert!(!night.save);
        assert_eq!(
            w.actor(&hunter_id).unwrap().needs.body.dead,
            poisoned,
            "同夜被毒的猎人必须当晚倒下，不能推迟到天亮开枪"
        );
        assert!(v.seats.iter().all(|s| s.out.is_none()));
        assert!(v.dawn.is_empty(), "夜里倒下的名字须等天亮公布");

        // 到次日 10:30 才公布。被毒不能开枪；仅被刀的猎人先获得回合，放弃后才倒下。
        let events = w.step(DAY + 10.5 * HOUR - w.minutes());
        let v = werewolf(&w);
        assert_eq!(
            v.phase,
            if poisoned {
                WerewolfPhase::Day
            } else {
                WerewolfPhase::Shot
            }
        );
        assert_eq!(v.winner, None);
        assert_eq!(v.dawn, [hunter_seat], "同一个人只公布一次");
        let hunter = &v.seats[hunter_seat as usize - 1];
        assert_eq!(
            hunter.out,
            Some(if poisoned {
                WerewolfOut::Poisoned
            } else {
                WerewolfOut::Killed
            })
        );
        assert_eq!(hunter.out_day, Some(1));
        let dawn_lines: Vec<_> = logs(&events)
            .into_iter()
            .filter(|text| text.starts_with("【狼人杀】天亮了，第 2 天。"))
            .collect();
        assert_eq!(dawn_lines.len(), 1);
        let announced = format!("昨晚倒下的是 {hunter_seat} 号 {hunter_id}。");
        assert!(dawn_lines[0].contains(&announced));
        assert!(!dawn_lines[0].contains("毒") && !dawn_lines[0].contains("猎人"));
        let actions = wolf_context(&w, &hunter_id).actions;
        if poisoned {
            assert_eq!(v.turn, None);
            assert!(actions.is_empty());
            assert!(w.game_mode_tool(&hunter_id).is_none());
            assert!(w
                .game_mode_act(
                    &hunter_id,
                    &GameModeAction {
                        action: "shoot".into(),
                        target: Some(witch_seat.to_string()),
                        text: None,
                        reason: None,
                    }
                )
                .is_err());
        } else {
            assert_eq!(v.turn, Some(hunter_seat));
            assert!(actions.iter().any(|a| a == "shoot"));
            mode_act(&mut w, &hunter_id, "pass", None, None);
        }
        w.step(1.0);
        let v = werewolf(&w);
        assert_eq!((v.phase, v.winner), (WerewolfPhase::Day, None));
        assert_eq!(v.seats.iter().filter(|s| s.out.is_some()).count(), 1);
        let body = &w.actor(&hunter_id).unwrap().needs.body;
        assert!(body.dead);
        assert_eq!(
            body.killed.as_ref().unwrap().by.name,
            if poisoned {
                "女巫的毒药"
            } else {
                "狼人"
            }
        );
        assert_eq!(
            body.history
                .iter()
                .filter(|e| e.kind == protocol::BodyEventKind::Death)
                .count(),
            1,
            "从夜里结算到天亮、继续推进只记一次死亡"
        );
    }
}

fn swordholder(w: &World) -> SwordholderView {
    match w.game_mode_view() {
        Some(GameModeView::Swordholder(v)) => v,
        other => panic!("开着的该是执剑人之夜：{other:?}"),
    }
}

/// 执剑人之夜走一整局：用给模型看的那一栏（swordholder.actions）当成模型，按固定打法交动作。
/// press 时：一个选民表态超时、投票 2 比 2 平票由现任执剑人指定 3 号，交接后执剑人按下开关，三体代表说最后一段话，
/// 投票中途存档读档；不按时：票多的 2 号当选，水滴来袭后执剑人一直不交（到点算没按），三体代表也不开口，法官替他宣布。
fn play_swordholder(press: bool) {
    let names = [
        "罗辑", "程心", "维德", "智子", "艾AA", "老秦", "林朵", "白杨", "小庄",
    ];
    let ids: Vec<String> = (1..=9).map(|n| format!("r{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .zip(names)
        .map(|(id, name)| (id.as_str(), name, "loc_a"))
        .collect();
    let fresh = || {
        let mut w = common::world_with(common::tiny_topology(), 7.0 * HOUR, &people);
        w.set_physiology(PhysiologySwitches {
            fullness: false,
            energy: false,
            water: false,
            health: false,
            weather: false,
        });
        w
    };
    let mut w = fresh();
    // 人不够开不了，原来的世界不受影响
    let mut few = common::world_with(common::tiny_topology(), 7.0 * HOUR, &people[..5]);
    assert!(few
        .set_game_mode(Some(&GameModeChoice::Swordholder {
            seed: Some(3),
            day_hours: None
        }))
        .is_err());
    assert!(few.game_mode().is_none());

    w.set_game_mode(Some(&GameModeChoice::Swordholder {
        seed: Some(5),
        day_hours: Some(8),
    }))
    .unwrap();
    assert_eq!(w.game_mode(), Some("swordholder"));
    let v = swordholder(&w);
    let roles: Vec<SwordholderRole> = v.seats.iter().map(|s| s.role).collect();
    use SwordholderRole::*;
    assert_eq!(
        roles,
        [Holder, Candidate, Candidate, Envoy, Voter, Voter, Voter, Voter, Voter]
    );
    assert_eq!((v.phase, v.swordholder), (SwordholderPhase::Waiting, 1));
    // 开局前就有玩法动作工具（整局工具表不变），但没有要交的；空档走快一点
    assert!(w.game_mode_tool("r5").unwrap().contains("press"));
    assert_eq!(w.game_mode_tempo(), 3.0);
    // 08:30 天亮：每人单独收到自己的座号和身份
    w.step(1.6 * HOUR);
    assert_eq!(swordholder(&w).phase, SwordholderPhase::Day);
    for id in &ids {
        let told = w
            .actor(id)
            .unwrap()
            .journal
            .events_after(0)
            .iter()
            .any(|e| e.kind == FactKind::Notice && e.text.contains("身份是"));
        assert!(told, "{id} 没收到身份");
    }

    let id_of = |seat: u32| ids[seat as usize - 1].clone();
    let mut skipped = false;
    let mut reloaded = false;
    let mut logged = vec![];
    for _ in 0..3000 {
        logged.extend(w.take_events());
        let v = swordholder(&w);
        if v.phase == SwordholderPhase::Over {
            break;
        }
        // 有期限的回合照常速
        if v.phase == SwordholderPhase::Speech {
            assert_eq!(w.game_mode_tempo(), 1.0);
        }
        // 第三个表态的选民不交，到点跳过
        if press && v.phase == SwordholderPhase::Speech && v.lines.len() == 5 && !skipped {
            skipped = true;
            let silent = v.turn.unwrap();
            assert_eq!(v.seats[silent as usize - 1].role, Voter);
            w.step(16.0);
            let after = swordholder(&w);
            assert_ne!(after.turn, Some(silent));
            assert_eq!(after.lines.len(), 5);
            continue;
        }
        // 投票交了两张时存档，读进一个新世界接着走；亮票前看不到票投给了谁
        if press && v.phase == SwordholderPhase::Vote && v.ballots.len() == 2 && !reloaded {
            reloaded = true;
            assert!(v.ballots.iter().all(|b| b.target.is_none()));
            w = common::restore(&w, fresh());
            assert_eq!(swordholder(&w), v);
        }
        let mut acted = false;
        for seat in 1..=9u32 {
            let id = id_of(seat);
            let Some(GameModeContext::Swordholder(c)) = w.game_mode_context(&id) else {
                continue;
            };
            let Some(first) = c.actions.first().cloned() else {
                continue;
            };
            match first.as_str() {
                "speak" if press || v.phase != SwordholderPhase::Aftermath => {
                    mode_act(&mut w, &id, "speak", None, Some("我想说几句。"))
                }
                "vote" => {
                    // press 时 5、6 号投 2 号，7、8 号投 3 号，9 号弃票：平票；不按时都投 2 号
                    let target = match seat {
                        _ if !press => Some(2),
                        5 | 6 => Some(2),
                        7 | 8 => Some(3),
                        _ => None,
                    };
                    let action = if target.is_some() { "vote" } else { "abstain" };
                    mode_act(&mut w, &id, action, target, None);
                }
                "appoint" => mode_act(&mut w, &id, "appoint", Some(3), None),
                "press" if press => mode_act(&mut w, &id, "press", None, Some("按下。")),
                // 执剑人一直不交，到点算没按；三体代表也不开口
                _ => {
                    w.step(5.0);
                }
            }
            acted = true;
            break;
        }
        if !acted {
            w.step(5.0);
        }
    }
    logged.extend(w.take_events());
    let v = swordholder(&w);
    assert_eq!(v.phase, SwordholderPhase::Over);
    let history = match w.game_mode_context("r5") {
        Some(GameModeContext::Swordholder(c)) => c.history,
        other => panic!("{other:?}"),
    };
    let occasions: std::collections::BTreeSet<String> =
        v.lines.iter().map(|l| l.occasion.clone()).collect();
    if press {
        assert!(skipped && reloaded);
        assert_eq!((v.elected, v.appointed, v.swordholder), (Some(3), true, 3));
        assert!(history.iter().any(|h| h.contains("平票")), "{history:?}");
        let d = v.decision.as_ref().expect("执剑人交了决定");
        assert_eq!((d.seat, d.press), (3, true));
        assert!(d
            .reason
            .as_deref()
            .is_some_and(|r| r.ends_with("心里这么想")));
        assert_eq!(v.ending, Some(SwordholderEnding::Broadcast));
        for occasion in ["一句话", "演说", "表态", "交接", "宣布"] {
            assert!(occasions.contains(occasion), "{occasions:?}");
        }
        // 亮票后票型和理由都给旁观的人看
        assert_eq!(v.ballots.len(), 5);
        assert!(v.ballots.iter().any(|b| b.target == Some(3)));
    } else {
        assert_eq!((v.elected, v.appointed, v.swordholder), (Some(2), false, 2));
        assert!(v.decision.is_none());
        assert_eq!(v.ending, Some(SwordholderEnding::Failed));
        assert!(!occasions.contains("宣布"));
        assert!(
            history.iter().any(|h| h.contains("迁往澳大利亚")),
            "{history:?}"
        );
    }
    // 说过的话写进日志时都带着「一段话」：发言人、座号、场合和原话
    let spoken = logged
        .iter()
        .filter(|Event::Log { info, .. }| info.speech.is_some())
        .count();
    assert_eq!(spoken, v.lines.len());
    // 一局结束：没有玩法动作工具了，世界照常走；关掉后存档和名册里没有玩法的痕迹
    assert!(w.game_mode_tool("r3").is_none());
    w.set_game_mode(None).unwrap();
    no_trace(&w);
}

#[test]
fn swordholder_night_elects_hands_over_and_ends_when_the_switch_is_pressed() {
    play_swordholder(true);
}

#[test]
fn swordholder_night_fails_when_the_swordholder_never_presses() {
    play_swordholder(false);
}

fn survival10(w: &World) -> Survival10View {
    match w.game_mode_view() {
        Some(GameModeView::Survival10(v)) => v,
        other => panic!("开着的该是聪明人小镇：{other:?}"),
    }
}

/// 聪明人小镇的回合流程：固定七人、白天提交窗口、悄悄话每天一条、偶数天会议与第十天陪审团。
/// 预置十日口粮，不替人物做真实劳动和取粮；资源结算由下面的真实移动测试覆盖。
#[test]
fn survival10_runs_ten_days_through_the_jury() {
    let ids: Vec<String> = (0..7).map(|n| format!("s{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .enumerate()
        .map(|(n, id)| {
            (
                id.as_str(),
                ["甲", "乙", "丙", "丁", "戊", "己", "庚"][n],
                "loc_a",
            )
        })
        .collect();
    let topology = common::topology(&common::two_places_json(|data| {
        data["locales"][1]["name"] = json!("篝火广场");
    }));
    let mut w = common::world_with(topology, 7.0 * HOUR, &people);
    place_survival10_farms(&mut w, "loc_a");
    w.set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(4) }))
        .unwrap();
    w.test_move_granary("loc_a");
    w.topo.locales.get_mut("loc_b").unwrap().name = "篝火广场".into();
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    for id in &ids {
        w.test_give_rice(id, 10);
    }
    // 六个人不够：固定七位，宁可拒绝也不拿半个阵容开一局
    let mut few = common::world_with(common::tiny_topology(), 7.0 * HOUR, &people[..6]);
    assert!(few
        .set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(4) }))
        .is_err());
    assert!(few.game_mode().is_none());

    let conserved = |w: &World| {
        let v = survival10(w);
        v.public_rice + v.seats.iter().map(|s| s.rice).sum::<u32>()
    };
    assert_eq!(survival10(&w).phase, Survival10Phase::Waiting);
    w.step(HOUR);
    assert_eq!(survival10(&w).phase, Survival10Phase::Day);
    w.step(30.0);
    assert_eq!(
        survival10(&w).phase,
        Survival10Phase::Day,
        "白天窗口太短，模型来不及提交"
    );

    let mut whispered = false;
    let mut jury_seen = false;
    let mut day10_vote_seen = false;
    let mut previous = survival10(&w);
    let mut remaining = 98;
    let mut meetings = Vec::new();
    let mut gatherings = Vec::new();
    for _ in 0..20000 {
        let v = survival10(&w);
        if previous.phase == Survival10Phase::Day && v.phase != Survival10Phase::Day {
            remaining -= previous
                .seats
                .iter()
                .filter(|seat| seat.out.is_none())
                .count() as u32;
        }
        if v.phase == Survival10Phase::Speech && previous.phase != Survival10Phase::Speech {
            meetings.push(v.day);
            assert_eq!(kernel::time_of_day(w.minutes()), 18.5 * HOUR);
            for seat in v.seats.iter().filter(|seat| seat.out.is_none()) {
                assert_eq!(w.actor(&seat.id).unwrap().locus.locale, "loc_b");
            }
        }
        if v.phase == Survival10Phase::Gather && previous.phase != Survival10Phase::Gather {
            gatherings.push(v.day);
            assert_eq!(kernel::time_of_day(w.minutes()), 18.0 * HOUR);
            assert_eq!(kernel::time_of_day(v.until.unwrap()), 18.5 * HOUR);
            for id in &ids {
                assert!(survival10_act(&mut w, id, "vote", Some("2")).is_err());
            }
        }
        previous = v.clone();
        assert_eq!(conserved(&w), remaining, "每天只能在晚餐时消耗一次口粮");
        if v.phase == Survival10Phase::Over {
            break;
        }
        if v.phase == Survival10Phase::Jury {
            jury_seen = true;
        }
        if v.day == 10 && v.phase == Survival10Phase::Vote {
            day10_vote_seen = true;
        }
        let mut acted = false;
        for id in &ids {
            let Some(GameModeContext::Survival10(c)) = w.game_mode_context(id) else {
                continue;
            };
            if v.seats
                .iter()
                .find(|seat| &seat.id == id)
                .unwrap()
                .out
                .is_none()
            {
                assert!(c.goal.contains("自己保持参赛资格，活到第十天终局"));
            } else {
                assert!(c.goal.contains("你已出局，自己的生存目标未完成"));
            }
            let Some(first) = c.actions.first().map(String::as_str) else {
                continue;
            };
            acted = true;
            match first {
                "work" | "take_rice" => {
                    mode_act(&mut w, id, "take_rice", None, None);
                }
                "whisper" => {
                    if v.day == 1 && !whispered {
                        whispered = true;
                        mode_act(&mut w, id, "whisper", Some(2), Some("今晚别睡太沉。"));
                        let again = GameModeAction {
                            action: "whisper".into(),
                            target: Some(3.to_string()),
                            text: Some("再说一遍".into()),
                            reason: None,
                        };
                        assert!(w.game_mode_act(id, &again).is_err(), "一天只能发一条悄悄话");
                    }
                }
                "speak" => mode_act(&mut w, id, "speak", None, Some("我先说说粮食的账。")),
                "vote" | "jury_vote" => {
                    let target = v
                        .seats
                        .iter()
                        .find(|s| s.out.is_none() && &s.id != id)
                        .map(|s| s.seat)
                        .expect("总有人可投");
                    mode_act(&mut w, id, first, Some(target), None);
                }
                // 不交睡觉、守家或偷粮：没完成动作的登记不会产生任何结果。
                _ => mode_act(&mut w, id, first, None, None),
            }
        }
        if acted {
            w.step(1.0);
        } else {
            let step = w.quiet_minutes(|_| Some(10.0)).unwrap_or(10.0).min(60.0);
            w.step(step);
        }
    }
    let v = survival10(&w);
    assert_eq!(v.phase, Survival10Phase::Over);
    assert_eq!(v.day, 10);
    assert_eq!(meetings, vec![2, 4, 6, 8, 10]);
    assert_eq!(gatherings, vec![2, 4, 6, 8, 10]);
    assert!(jury_seen);
    assert!(day10_vote_seen, "第十天不能跳过第五次篝火投票");
    assert!(v.winner.is_some(), "十日实验必须有人或明说无人获胜");
    let ended_at = w.minutes();
    w.step(HOUR);
    assert_eq!(w.minutes(), ended_at);
    w.set_game_mode(None).unwrap();
}

#[test]
fn survival10_meeting_requires_every_real_speech_and_presence_before_voting() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    for id in &ids {
        world.test_give_rice(id, 10);
    }
    while survival10(&world).phase != Survival10Phase::Speech {
        world.step(30.0);
    }
    assert_eq!(kernel::time_of_day(world.minutes()), 18.5 * HOUR);
    assert!(survival10(&world).lines.is_empty());
    for (seat, id) in ids.iter().enumerate() {
        // 发言前先提交一段想法：它只进本人见闻，不能被当成公开发言。
        world.note_thought(id, "我觉得该公开粮食账。", "测试想法");
        assert!(survival10(&world)
            .lines
            .iter()
            .all(|line| line.seat != seat as u32 + 1));
        mode_act(&mut world, id, "speak", None, Some("公开说明粮食账。"));
        if let Some(next) = survival10(&world).turn {
            assert!(next > seat as u32 + 1, "只按座位顺序逐个交接");
        }
    }
    let view = survival10(&world);
    assert_eq!(view.phase, Survival10Phase::Vote);
    assert_eq!(view.lines.len(), 7);
    assert!(view
        .lines
        .iter()
        .all(|line| line.text == "公开说明粮食账。"));
    assert!(survival10_act(&mut world, &ids[0], "speak", None).is_err());
    let saved = world.snapshot();
    world.restore_snapshot(saved).unwrap();
    assert_eq!(survival10(&world).phase, Survival10Phase::Vote);
    assert_eq!(survival10(&world).lines.len(), 7);
    walk(&mut world, &ids[1], "leave-meeting", "loc_a");
    assert!(survival10_act(&mut world, &ids[1], "vote", Some("1"))
        .unwrap_err()
        .to_string()
        .contains("篝火广场"));
    walk(&mut world, &ids[1], "return-meeting", "loc_b");
    let night = survival10_act(&mut world, &ids[1], "vote", Some("3")).unwrap();
    assert!(night.contains("揭票前不向其他参赛者公开目标"), "{night}");
    for (index, id) in ids.iter().enumerate().filter(|(index, _)| *index != 1) {
        let target = if index == 0 { "2" } else { "1" };
        survival10_act(&mut world, id, "vote", Some(target)).unwrap();
    }
    assert_eq!(survival10(&world).ballots.len(), 7);
    assert!(survival10(&world)
        .ballots
        .iter()
        .all(|ballot| ballot.target.is_none()));
    // 超时缺票不能冒充本人决定：揭票前不公开代投目标，统一揭票才给出完整记录。
    let (mut late, late_ids) = survival10_world(7.0 * HOUR);
    for id in &late_ids {
        late.test_give_rice(id, 10);
    }
    survival10_to_vote(&mut late, &late_ids, &mut Vec::new(), &mut Vec::new());
    for (index, id) in late_ids.iter().enumerate().filter(|(index, _)| *index != 1) {
        survival10_act(
            &mut late,
            id,
            "vote",
            Some(if index == 0 { "2" } else { "1" }),
        )
        .unwrap();
    }
    while survival10(&late).phase == Survival10Phase::Vote {
        late.step(1.0);
    }
    let view = survival10(&late);
    // 世界先前收到的私密选票也在同一队列里；只看迟到者这一场揭票前保密的代投记录。
    let automatic = late
        .take_manuscript()
        .into_iter()
        .find(|fact| fact.source.ends_with("-automatic"))
        .unwrap_or_else(|| panic!("迟到者的系统代投必须还能追溯：{view:?}"));
    assert_eq!(automatic.kind, ManuscriptKind::Announcement);
    assert_eq!(automatic.visibility, ManuscriptVisibility::Private);
    assert!(automatic.text.contains("2号") && automatic.text.contains("代投给"));
    assert!(!notes(&late, &late_ids[0])
        .iter()
        .any(|text| text.contains("2号") && text.contains("代投")));
    assert!(
        view.history.iter().any(|notice| {
            notice.contains("2号")
                && notice.contains("记名投票给")
                && notice.contains("（系统代投）")
        }),
        "统一揭票时必须写清代投目标：{:?}",
        view.history
    );
    world.step(1.0);
    let view = survival10(&world);
    assert!(
        matches!(
            view.phase,
            Survival10Phase::LastWords | Survival10Phase::Night
        ),
        "七票揭票后进入后续阶段：{view:?}"
    );
    assert!(view
        .history
        .iter()
        .any(|notice| notice.contains("揭票汇总")));
}

#[test]
fn survival10_ties_repeat_speeches_and_restrict_each_runoff_to_its_candidates() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    for id in &ids {
        world.test_give_rice(id, 10);
    }
    while survival10(&world).phase != Survival10Phase::Speech {
        world.step(30.0);
    }
    for id in &ids {
        mode_act(&mut world, id, "speak", None, Some("先讨论粮食与投票。"));
    }
    let rounds = [
        ([2, 1, 1, 2, 3, 3, 4], vec![1, 2, 3]),
        ([2, 1, 1, 1, 2, 2, 3], vec![1, 2]),
    ];
    let mut speech_keys = std::collections::BTreeSet::new();
    for (targets, candidates) in rounds {
        for (id, target) in ids.iter().zip(targets) {
            mode_act(&mut world, id, "vote", Some(target), None);
        }
        world.step(0.1);
        let view = survival10(&world);
        assert_eq!(view.phase, Survival10Phase::Speech);
        assert!(
            view.until.unwrap() > world.minutes(),
            "重投的发言截止必须从投票结束时刻重新计时：{view:?}"
        );
        assert!(view.seats.iter().all(|seat| seat.out.is_none()));
        assert_eq!(view.turn, candidates.first().copied());
        let saved = world.snapshot();
        world.restore_snapshot(saved).unwrap();
        for &candidate in &candidates {
            mode_act(
                &mut world,
                &ids[candidate as usize - 1],
                "speak",
                None,
                Some("平票后重新说明。"),
            );
            let protocol::ServerMsg::Roster { locales, .. } = world.roster(1.0) else {
                panic!("名册")
            };
            let speech = locales
                .iter()
                .flat_map(|locale| &locale.members)
                .find(|member| member.id == ids[candidate as usize - 1])
                .unwrap()
                .speech
                .as_ref()
                .unwrap();
            assert!(speech_keys.insert(format!("{}:{}", speech.chat_id, speech.seq)));
        }
        let view = survival10(&world);
        assert_eq!(view.phase, Survival10Phase::Vote);
        assert!(
            view.until.unwrap() > world.minutes(),
            "重投截止不能沿用上一轮：{view:?}"
        );
        assert!(view.ballots.is_empty());
        assert!(survival10_act(&mut world, &ids[0], "vote", Some("4")).is_err());
    }
    for (id, target) in ids.iter().zip([2, 1, 1, 1, 1, 2, 2]) {
        mode_act(&mut world, id, "vote", Some(target), None);
    }
    world.step(0.1);
    let view = survival10(&world);
    assert_eq!(view.phase, Survival10Phase::LastWords);
    mode_act(
        &mut world,
        &ids[0],
        "farewell",
        Some(2),
        Some("我怀疑二号，但并无确证。"),
    );
    assert_eq!(survival10(&world).phase, Survival10Phase::Night);
    let view = survival10(&world);
    assert!(view.seats[0].out.is_some());
    assert_eq!(
        view.seats.iter().filter(|seat| seat.out.is_none()).count(),
        6
    );
}

#[test]
fn survival10_vote_finishing_past_midnight_still_gives_the_night_its_full_deadline() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    world.test_give_rice(&ids[0], 1);
    while survival10(&world).phase != Survival10Phase::Speech {
        world.step(30.0);
    }
    for id in &ids {
        mode_act(&mut world, id, "speak", None, Some("先把粮食账说清楚。"));
    }
    // 最后一票在揭票期限之后才交齐：投票和陈词都拖过了原定午夜，仍应完整经过这一夜，
    // 而不是一进入夜间就被截止判成“天亮”。
    world.step(survival10(&world).until.unwrap() - world.minutes() + 1.0);
    while survival10(&world).phase == Survival10Phase::LastWords {
        let seat = survival10(&world).turn.expect("轮到一个出局者") as usize - 1;
        mode_act(
            &mut world,
            &ids[seat],
            "farewell",
            None,
            Some("我把这一票的经过说清楚。"),
        );
    }
    let round = survival10(&world).day;
    let night = survival10(&world);
    assert_eq!(night.phase, Survival10Phase::Night);
    assert_eq!(night.day, round);
    assert!(
        night.until.unwrap() > world.minutes() + 30.0,
        "进入夜间后不能立刻到期：{night:?}"
    );
    world.step(1.0);
    assert_eq!(survival10(&world).phase, Survival10Phase::Night);
}

#[test]
fn survival10_two_survivors_enter_jury_and_one_survivor_wins_immediately() {
    for survivors in [0, 1, 2, 3] {
        let (mut world, ids) = survival10_legacy_world(7.0 * HOUR);
        for id in &ids[..survivors] {
            world.test_give_rice(id, 10);
        }
        while survival10(&world).day < 2
            || matches!(
                survival10(&world).phase,
                Survival10Phase::Day | Survival10Phase::Speech
            )
        {
            world.step(30.0);
        }
        let view = survival10(&world);
        assert_eq!(
            view.seats.iter().filter(|seat| seat.out.is_none()).count(),
            survivors
        );
        if survivors <= 1 {
            // 濒死那一轮的所有人都先走完陈词，之后才按最终存活人数终局。
            while survival10(&world).phase == Survival10Phase::LastWords {
                let seat = survival10(&world).turn.expect("轮到一位出局者") as usize - 1;
                mode_act(
                    &mut world,
                    &ids[seat],
                    "farewell",
                    None,
                    Some("我把这一轮的选择交代清楚。"),
                );
            }
            let view = survival10(&world);
            assert_eq!(
                view.seats.iter().filter(|seat| seat.out.is_none()).count(),
                survivors
            );
            assert_eq!(view.phase, Survival10Phase::Over);
            assert_eq!(view.winner, (survivors == 1).then_some(1));
            assert!(view.until.is_none());
            let ended = world.minutes();
            world.step(24.0 * HOUR);
            assert_eq!(world.minutes(), ended, "一人获胜后世界不再继续过夜");
            assert!(world
                .game_mode_act(
                    &ids[0],
                    &GameModeAction {
                        action: "whisper".into(),
                        target: Some(ids[1].clone()),
                        text: Some("不该再被接受".into()),
                        reason: None,
                    },
                )
                .is_err());
            assert!(world
                .restore_snapshot(
                    serde_json::from_value(serde_json::to_value(world.snapshot()).unwrap())
                        .unwrap()
                )
                .is_ok());
            world.step(HOUR);
            assert_eq!(world.minutes(), ended, "保存重开后终局仍保持结束");
            let mut legacy = serde_json::to_value(world.snapshot()).unwrap();
            legacy["game_mode"]["survival10"]["until"] = serde_json::Value::Null;
            world
                .restore_snapshot(serde_json::from_value(legacy).unwrap())
                .unwrap();
            world.step(HOUR);
            assert_eq!(world.minutes(), ended);
            continue;
        }
        let pending = survival10(&world);
        assert_eq!(pending.phase, Survival10Phase::LastWords);
        let seat = pending.turn.unwrap() as usize - 1;
        mode_act(
            &mut world,
            &ids[seat],
            "farewell",
            Some(1),
            Some("这只是我的推测，不是裁判认定。"),
        );
        let saved = serde_json::to_value(world.snapshot()).unwrap();
        world
            .restore_snapshot(serde_json::from_value(saved).unwrap())
            .unwrap();
        if survivors > 2 {
            // 三人以上：从第二天的同一轮离场之后继续走真实的会议、投票与陈词，
            // 直到只剩两人进入陪审团。每人只交自己的原话和选票；只是等待世界走完，
            // 一旦久停就失败。
            let mut guard = 0;
            let mut last = None;
            while survival10(&world).phase != Survival10Phase::Jury && guard < 3000 {
                guard += 1;
                let view = survival10(&world);
                match view.phase {
                    Survival10Phase::Gather => {
                        world.step(30.0);
                    }
                    Survival10Phase::Speech => {
                        let seat = view.turn.expect("轮到一位发言人") as usize - 1;
                        assert_ne!(last, Some(seat), "同一场会议的发言人不能重复");
                        last = Some(seat);
                        mode_act(
                            &mut world,
                            &ids[seat],
                            "speak",
                            None,
                            Some("说明我的选择。"),
                        );
                    }
                    Survival10Phase::Vote => {
                        let live: Vec<usize> = view
                            .seats
                            .iter()
                            .filter(|seat| seat.out.is_none())
                            .map(|seat| seat.seat as usize - 1)
                            .collect();
                        // 揭票视图留着上一轮票，新轮只看动作栏谁还能投。先让三人互相换票
                        // 走一轮平票；之后所有人集中投最靠前的在场者，兑现重投的淘汰。
                        let open = |world: &World, seat: usize| {
                            world.game_mode_context(&ids[seat]).is_some_and(|context| {
                                matches!(context, GameModeContext::Survival10(context) if context.actions.iter().any(|a| a == "vote"))
                            })
                        };
                        let runoff = view
                            .history
                            .first()
                            .is_some_and(|notice| notice.contains("重投"));
                        for &seat in &live {
                            if !open(&world, seat) {
                                continue;
                            }
                            let target = if runoff {
                                live.iter()
                                    .copied()
                                    .find(|&target| target != seat)
                                    .expect("重投还有另一位在场者")
                            } else {
                                live[(live.iter().position(|&s| s == seat).unwrap() + 1)
                                    % live.len()]
                            };
                            let action = GameModeAction {
                                action: "vote".into(),
                                target: Some((target as u32 + 1).to_string()),
                                text: None,
                                reason: Some(format!("{}的选票", ids[seat])),
                            };
                            if world.game_mode_act(&ids[seat], &action).is_err() {
                                live.iter()
                                    .copied()
                                    .filter(|&other| other != seat)
                                    .find(|&other| {
                                        world
                                            .game_mode_act(
                                                &ids[seat],
                                                &GameModeAction {
                                                    target: Some((other as u32 + 1).to_string()),
                                                    ..action.clone()
                                                },
                                            )
                                            .is_ok()
                                    })
                                    .expect("总有一位可投且合规的在场者");
                            }
                        }
                        world.step(0.2);
                    }
                    Survival10Phase::LastWords => {
                        let seat = view.turn.expect("轮到一位出局者") as usize - 1;
                        mode_act(
                            &mut world,
                            &ids[seat],
                            "farewell",
                            None,
                            Some("我接受这一轮结果。"),
                        );
                    }
                    Survival10Phase::Day | Survival10Phase::Night => {
                        world.step(30.0);
                    }
                    other => panic!("意外进入 {other:?} @{}，{view:?}", world.minutes()),
                }
            }
            assert!(guard < 3000, "三人流程应有界地到达陪审团");
            assert_eq!(survival10(&world).phase, Survival10Phase::Jury);
        }
        while survival10(&world).phase == Survival10Phase::LastWords {
            let seat = survival10(&world).turn.expect("轮到一位出局者") as usize - 1;
            mode_act(
                &mut world,
                &ids[seat],
                "farewell",
                None,
                Some("我把这一轮的选择交代清楚。"),
            );
        }
        let view = survival10(&world);
        assert_eq!(view.phase, Survival10Phase::Jury);
        assert_eq!(
            view.seats.iter().filter(|seat| seat.out.is_none()).count(),
            2
        );
        let saved = world.snapshot();
        world.restore_snapshot(saved).unwrap();
        let winner = view
            .seats
            .iter()
            .find(|seat| seat.out.is_none())
            .unwrap()
            .seat;
        for seat in view.seats.iter().filter(|seat| seat.out.is_some()) {
            mode_act(&mut world, &seat.id, "jury_vote", Some(winner), None);
        }
        world.step(0.1);
        let view = survival10(&world);
        assert_eq!(view.phase, Survival10Phase::Over);
        assert_eq!(view.winner, Some(winner));
        let ended = world.minutes();
        let saved = serde_json::to_value(world.snapshot()).unwrap();
        world
            .restore_snapshot(serde_json::from_value(saved).unwrap())
            .unwrap();
        world.step(DAY);
        assert_eq!(world.minutes(), ended);
    }
}

/// 陪审投票也宣布“不能弃票”：超时缺票由系统代投，揭票、剧本和终局记录都能看出这一票不是本人提交的。
#[test]
fn survival10_missing_jury_votes_are_auto_cast_and_traceable() {
    let (mut world, ids) = survival10_legacy_world(7.0 * HOUR);
    for id in &ids[..=2] {
        world.test_give_rice(id, 10);
    }
    let mut guard = 0;
    while survival10(&world).phase != Survival10Phase::Jury {
        guard += 1;
        assert!(guard < 2000, "九天内应进入陪审团");
        let view = survival10(&world);
        match view.phase {
            Survival10Phase::Waiting
            | Survival10Phase::Gather
            | Survival10Phase::Day
            | Survival10Phase::Night => {
                world.step(30.0);
            }
            Survival10Phase::Speech => {
                let seat = view.turn.expect("轮到一位发言人") as usize - 1;
                mode_act(
                    &mut world,
                    &ids[seat],
                    "speak",
                    None,
                    Some("说明我的选择。"),
                );
            }
            Survival10Phase::Vote => {
                let live: Vec<usize> = view
                    .seats
                    .iter()
                    .filter(|seat| seat.out.is_none())
                    .map(|seat| seat.seat as usize - 1)
                    .collect();
                for &seat in &live {
                    let target = live
                        .iter()
                        .copied()
                        .find(|&target| target != seat)
                        .expect("还有另一位在场者");
                    mode_act(
                        &mut world,
                        &ids[seat],
                        "vote",
                        Some(target as u32 + 1),
                        None,
                    );
                }
                world.step(0.2);
            }
            Survival10Phase::LastWords => {
                let seat = view.turn.expect("轮到一位出局者") as usize - 1;
                mode_act(
                    &mut world,
                    &ids[seat],
                    "farewell",
                    None,
                    Some("我接受这一轮结果。"),
                );
            }
            other => panic!("意外进入 {other:?} @{}", world.minutes()),
        }
    }
    let view = survival10(&world);
    let juries: Vec<u32> = view
        .seats
        .iter()
        .filter(|seat| seat.out.is_some())
        .map(|seat| seat.seat)
        .collect();
    assert!(!juries.is_empty());
    let winner = view
        .seats
        .iter()
        .find(|seat| seat.out.is_none())
        .expect("还有候选人")
        .seat;
    // 只让一位陪审员本人投票；其余人等到截止，检查系统代投和揭票。
    mode_act(
        &mut world,
        &view.seats[juries[0] as usize - 1].id,
        "jury_vote",
        Some(winner),
        None,
    );
    world.step(39.0);
    world.step(1.0);
    let facts = world
        .take_manuscript()
        .into_iter()
        .filter(|entry| entry.source.starts_with(&format!("jury:{}-", view.day)))
        .filter(|entry| entry.source.ends_with("-automatic"))
        .count();
    let view = survival10(&world);
    assert_eq!(view.phase, Survival10Phase::Over);
    assert!(view.revealed);
    for &seat in &juries[1..] {
        let ballot = view
            .ballots
            .iter()
            .find(|ballot| ballot.voter == seat)
            .expect("超时者必须有一张系统代投");
        assert!(ballot.automatic);
        assert!(view
            .seats
            .iter()
            .any(|s| { s.seat == ballot.target.unwrap() && s.out.is_none() }));
    }
    assert!(view
        .history
        .iter()
        .any(|line| line.contains("（系统代投）")));
    assert_eq!(
        facts,
        juries.len() - 1,
        "每位超时者都要有可追溯的代投剧本事实"
    );
}

/// 同一顿晚饭有多人连续断粮：每人都要有陈词机会；全部离场后只开一次新阶段，
/// 不能把其余出局者直接甩给夜晚，也不能让原投票截止继续推进下一阶段。
#[test]
fn survival10_several_starving_players_finish_every_farewell_before_the_next_phase() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    while survival10(&world).phase != Survival10Phase::Day {
        world.step(30.0);
    }
    // 不等到晚饭：只把结算状态摆到第二天晚饭前一刻，检查同一拍的所有出局是否被收齐。
    let mut saved = serde_json::to_value(world.snapshot()).unwrap();
    saved["game_mode"]["survival10"]["day"] = json!(2);
    saved["game_mode"]["survival10"]["phase"] = json!("day");
    saved["game_mode"]["survival10"]["until"] = json!(world.minutes() + 1.0);
    for seat in saved["game_mode"]["survival10"]["seats"]
        .as_array_mut()
        .unwrap()
    {
        seat["rice"] = json!(0);
        seat["hungry_streak"] = json!(1);
    }
    world
        .restore_snapshot(serde_json::from_value(saved).unwrap())
        .unwrap();
    world.step(2.0);
    let mut seen = Vec::new();
    for _ in 0..ids.len() + 1 {
        let view = survival10(&world);
        assert_eq!(
            view.phase,
            Survival10Phase::LastWords,
            "七人同夜断粮时先把陈词走完：{view:?}"
        );
        let turn = view.turn.expect("轮到一位出局者") as usize - 1;
        seen.push(turn);
        mode_act(
            &mut world,
            &ids[turn],
            "farewell",
            None,
            Some("我接受出局。"),
        );
        if survival10(&world).phase != Survival10Phase::LastWords {
            break;
        }
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 7, "每位出局者都有一次陈词：{seen:?}");
    assert_eq!(survival10(&world).phase, Survival10Phase::Over);
    assert_eq!(survival10(&world).winner, None);
}

#[test]
fn survival10_dinner_is_private_and_hunger_counts_evenings_not_mornings() {
    let (mut world, ids) = survival10_legacy_world(7.0 * HOUR);
    world.test_give_rice(&ids[0], 3);
    world.test_give_rice(&ids[1], 1);
    world.step(11.0 * HOUR);
    let dinner = survival10(&world);
    assert_eq!(dinner.phase, Survival10Phase::Night);
    assert_eq!(dinner.seats[0].rice, 2);
    assert_eq!(dinner.seats[1].rice, 0);
    assert_eq!(dinner.seats[2].hungry_streak, 1);
    assert!(dinner.seats.iter().all(|seat| seat.out.is_none()));
    assert!(notes(&world, &ids[0])
        .iter()
        .any(|text| text.contains("晚餐消耗1袋米，剩余2袋")));
    world.step(14.0 * HOUR);
    let morning = survival10(&world);
    assert_eq!((morning.day, morning.phase), (2, Survival10Phase::Day));
    assert_eq!(morning.seats, dinner.seats);
    assert!(!morning.history.iter().any(|text| text.contains("剩余2袋")));
    assert!(!notes(&world, &ids[1])
        .iter()
        .any(|text| text.contains("剩余2袋")));
    world.step(10.0 * HOUR);
    let next_dinner = survival10(&world);
    assert_eq!(next_dinner.seats[0].rice, 1);
    assert_eq!(next_dinner.seats[1].hungry_streak, 1);
    assert!(next_dinner.seats[1].out.is_none());
    assert!(next_dinner.seats[2..].iter().all(|seat| {
        seat.hungry_streak == 2 && seat.out.as_deref() == Some("连续两晚未进食")
    }));
    let facts = world.take_manuscript();
    let dinner = facts
        .iter()
        .find(|fact| {
            fact.source.ends_with("-1")
                && fact.source.starts_with("dinner:")
                && fact.text.contains("晚餐")
        })
        .expect("晚餐要有旁观者可见的剧本结果");
    assert_eq!(dinner.visibility, ManuscriptVisibility::Private);
    assert!(dinner.text.contains("1号甲消耗晚餐1袋，私粮剩余2袋"));
    assert!(dinner.text.matches("没有米可吃，已连续1晚未进食").count() >= 1);
    let elimination = facts
        .iter()
        .find(|fact| fact.source.starts_with("eliminate:"))
        .expect("出局私粮归公仓要在剧本里可追溯");
    assert!(elimination.text.contains("出局前持有私粮0袋"));
    assert_eq!(
        elimination.visibility,
        ManuscriptVisibility::Private,
        "出局者私粮余额不能出现在「仅公开」"
    );
    assert!(
        !notes(&world, &ids[1])
            .iter()
            .any(|text| text.contains("出局前持有私粮") || text.contains("晚餐：")),
        "剧本的全知账目不能写进别人的记忆"
    );
}

/// 测试地图没有粮仓地点：大家在甲地，粮仓也摆在甲地，玩法的粮仓位置用测试钩子指到甲地。
fn survival10_world(start: f64) -> (World, Vec<String>) {
    let ids: Vec<String> = (0..7).map(|n| format!("h{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .enumerate()
        .map(|(n, id)| {
            (
                id.as_str(),
                ["甲", "乙", "丙", "丁", "戊", "己", "庚"][n],
                "loc_a",
            )
        })
        .collect();
    let mut w = common::world_with(common::tiny_topology(), start, &people);
    // 只测玩法的粮食：关掉通用生理，免得渴、累这些把人和比赛用的米搅在一起
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    w.place_prop(kernel::props::Placement {
        instance: kernel::props::Instance {
            id: "granary".into(),
            definition: "core:public-granary@2".into(),
            locale: "loc_a".into(),
            name: None,
            board: None,
        },
        definition: kernel::props::Catalog::from_json(include_str!(
            "../../../data/props/core/definitions.json"
        ))
        .unwrap()
        .latest("core:public-granary")
        .unwrap()
        .clone(),
    })
    .unwrap();
    w.set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(3) }))
        .unwrap();
    w.test_move_granary("loc_a");
    w.topo.locales.get_mut("loc_b").unwrap().name = "篝火广场".into();
    (w, ids)
}

fn survival10_legacy_world(start: f64) -> (World, Vec<String>) {
    let (mut w, ids) = survival10_world(start);
    let mut empty = serde_json::to_value(w.snapshot()).unwrap();
    for seat in empty["game_mode"]["survival10"]["seats"]
        .as_array_mut()
        .unwrap()
    {
        seat["rice"] = json!(0);
    }
    w.restore_snapshot(serde_json::from_value(empty).unwrap())
        .unwrap();
    (w, ids)
}

fn granary_on_map(w: &World) -> u32 {
    let found: Option<u32> = w
        .prop_views()
        .into_iter()
        .find(|p| p.name == "公共粮仓")
        .and_then(|p| {
            p.actions.iter().find_map(|a| match a {
                protocol::PropActionEntry::Defined(v) => v.remaining.flatten(),
                protocol::PropActionEntry::Legacy(_) => None,
            })
        });
    found.expect("粮仓没余量")
}

/// 甲、乙是出发点，每人一处住宅，公仓和初始落脚在丙地，田在乙地。
fn survival10_real_topology() -> kernel::Topology {
    common::topology(&common::two_places_json(|data| {
        data["locales"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "loc_c", "name": "粮仓广场", "kind": "dwell"}));
        for index in 1..=7 {
            let id = format!("test_{index}");
            let node = format!("n_home_{index}");
            let name = format!("{index}号住宅");
            data["locales"].as_array_mut().unwrap().push(json!({
                "id": id, "name": name, "kind": "dwell"
            }));
            data["stations"].as_array_mut().unwrap().push(json!({
                "id": format!("st_{index}"), "locale": id, "name": name,
                "kind": "arrival", "node": node
            }));
            data["places"].as_array_mut().unwrap().push(json!({
                "name": name, "locale": id, "arrival_station": format!("st_{index}"),
                "type": "tiny", "summary": ""
            }));
            data["passages"].as_array_mut().unwrap().push(json!({
                "id": format!("p_{index}"), "a": "loc_c", "b": id,
                "node_a": "n_c", "node_b": node, "minutes": 0.0, "capabilities": ["通行"]
            }));
        }
        data["entry"] = json!({"locale": "loc_c", "station": "st_c"});
        data["navigation"] = json!(null);
        data["precise_navigation"] = json!(false);
        data["places"].as_array_mut().unwrap().push(json!({
            "name": "路东空地", "locale": "loc_c", "arrival_station": "st_c",
            "type": "tiny", "summary": ""
        }));
        data["stations"].as_array_mut().unwrap().push(json!({
            "id": "st_c", "locale": "loc_c", "name": "粮仓广场", "kind": "arrival", "node": "n_c"
        }));
        data["passages"].as_array_mut().unwrap().push(json!({
            "id": "p_granary", "a": "loc_c", "b": "loc_a",
            "node_a": "n_c", "node_b": "n_a", "minutes": 0.0, "capabilities": ["通行"]
        }));
        let mut props = data["props"].as_array().unwrap().clone();
        props.push(json!({
            "id": "test:farm", "name": "田地", "locale": "loc_b",
            "verbs": [{"verb": "下地干活", "duration_minutes": 120.0,
                       "effects": {"累": 1.0}}]
        }));
        props.push(json!({
            "id": "test:bed", "name": "单人床", "locale": "test_1",
            "verbs": [{"verb": "睡觉", "duration_minutes": 480.0,
                       "completion": "sleep"}]
        }));
        data["props"] = json!(props);
    }))
}

fn place_survival10_real_props(w: &mut World, with_bed: bool) {
    place_survival10_farms(w, "loc_b");
    let catalog = kernel::props::Catalog::from_json(include_str!(
        "../../../data/props/core/definitions.json"
    ))
    .unwrap();
    let mut props = vec![("granary", "core:public-granary", "loc_c")];
    let homes = (1..=7).map(|n| format!("test_{n}")).collect::<Vec<_>>();
    for (index, home) in homes.iter().enumerate() {
        props.push((
            format!("user:s10-home_pantry_v2_{}", index + 1).leak(),
            "core:private-ration-sack",
            home,
        ));
    }
    if with_bed {
        for (index, home) in homes.iter().enumerate() {
            props.push((format!("bed_{}", index + 1).leak(), "core:bed", home));
        }
    }
    for (id, definition, locale) in props {
        w.place_prop(kernel::props::Placement {
            instance: kernel::props::Instance {
                id: id.into(),
                definition: format!(
                    "{definition}@{}",
                    catalog.latest(definition).unwrap().version
                ),
                locale: locale.into(),
                name: None,
                board: None,
            },
            definition: catalog.latest(definition).unwrap().clone(),
        })
        .unwrap();
    }
}

fn place_survival10_farms(w: &mut World, locale: &str) {
    let catalog = kernel::props::Catalog::from_json(include_str!(
        "../../../data/props/core/definitions.json"
    ))
    .unwrap();
    for number in 1..=4 {
        w.place_prop(kernel::props::Placement {
            instance: kernel::props::Instance {
                id: format!("user:s10-farm-v2_{number}"),
                definition: "core:survival10-farm@2".into(),
                locale: locale.into(),
                name: Some(format!("公共农田{number}号田块")),
                board: None,
            },
            definition: catalog.latest("core:survival10-farm").unwrap().clone(),
        })
        .unwrap();
    }
}

fn survival10_night_world(seed: u32) -> (World, Vec<String>) {
    let ids: Vec<String> = (0..7).map(|n| format!("h{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .map(|id| (id.as_str(), id.as_str(), "loc_c"))
        .collect();
    let mut w = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    place_survival10_real_props(&mut w, true);
    for (index, id) in ids.iter().enumerate() {
        w.actor_for_test(id).home = Some(format!("test_{}", index + 1));
    }
    w.set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(seed) }))
        .unwrap();
    w.test_move_granary("loc_c");
    w.step(HOUR);
    while survival10(&w).phase != Survival10Phase::Night {
        w.step(30.0);
    }
    (w, ids)
}

/// 玩家把夜里正在睡觉的人叫去别处：必须产生一次私密的“没有有效睡觉”结果，
/// 不能因为动作被中断就从剧本里消失，也不能把系统中断改写成别人已知的事。
#[test]
fn survival10_interrupted_sleep_is_reported_as_a_private_failed_result() {
    let (mut world, ids) = survival10_night_world(3);
    survival10_act(&mut world, &ids[1], "sleep", None).unwrap();
    let home = world.actor(&ids[1]).unwrap().home.clone().unwrap();
    walk(&mut world, &ids[1], "sleep-home", &home);
    common::run(
        &mut world,
        &ids[1],
        "sleep-bed",
        json!({"intent": {"verb": "睡觉"}, "prop_id": "bed_2"}),
    )
    .unwrap();
    world.step(10.0);
    // 外部动作打断睡眠，通用中断在这里给当事人落一次真实回执。
    world.interrupt_actions(&ids[1], "测试：中途起床离开");
    world.step(1.0);
    world.take_manuscript();
    let day = survival10(&world).day;
    while survival10(&world).day == day {
        world.step(30.0);
    }
    let facts = world.take_manuscript();
    let failed = facts
        .iter()
        .find(|fact| fact.text.contains("登记了睡觉") && fact.text.contains("没有有效睡觉"))
        .unwrap_or_else(|| panic!("中断的睡眠必须有失败结果：{facts:?}"));
    assert_eq!(failed.visibility, ManuscriptVisibility::Private);
    assert_eq!(failed.kind, ManuscriptKind::Action);
    for other in [&ids[0], &ids[2], &ids[3]] {
        assert!(
            !notes(&world, other)
                .iter()
                .any(|text| text.contains("没有有效睡觉")),
            "睡眠失败不能写进旁人的记忆：{other}"
        );
    }
}

fn survival10_act(
    w: &mut World,
    id: &str,
    action: &str,
    target: Option<&str>,
) -> Result<String, kernel::KernelError> {
    w.game_mode_act(
        id,
        &GameModeAction {
            action: action.into(),
            target: target.map(str::to_owned),
            text: None,
            reason: None,
        },
    )
}

#[test]
fn survival10_whisper_archive_survives_delivery_and_recipient_elimination_without_leaking() {
    let (mut world, ids) = survival10_legacy_world(7.0 * HOUR);
    for id in ids.iter().filter(|id| *id != &ids[2]) {
        world.test_give_rice(id, 10);
    }
    world.step(HOUR);
    let first = "仅甲乙可知的密信正文";
    mode_act(&mut world, &ids[0], "whisper", Some(2), Some(first));
    let saved = serde_json::to_value(world.snapshot()).unwrap();
    let archive = &saved["game_mode"]["survival10"]["whisper_archive"];
    assert_eq!(archive[0]["content"], first);
    assert_eq!(archive[0]["status"], "pending");
    assert_eq!(archive[0]["sent_at"], world.minutes());
    assert!(!notes(&world, &ids[1])
        .iter()
        .any(|note| note.contains(first)));
    world
        .restore_snapshot(serde_json::from_value(saved).unwrap())
        .unwrap();
    let mut events = Vec::new();
    let mut kept = Vec::new();
    while survival10(&world).day < 2 {
        events.extend(world.step(1.0));
        kept.extend(world.take_manuscript());
    }
    assert!(notes(&world, &ids[1])
        .iter()
        .any(|note| note.contains(first)));
    let second = "因接收者出局而未投递的密信正文";
    mode_act(&mut world, &ids[0], "whisper", Some(3), Some(second));
    while survival10(&world).day < 3 {
        let view = survival10(&world);
        kept.extend(world.take_manuscript());
        if view.phase == Survival10Phase::Speech {
            let seat = view.turn.unwrap() as usize - 1;
            let actor = world.actor(&ids[seat]).unwrap();
            if actor.locus.locale != "loc_b"
                || matches!(actor.current.kind, kernel::world::ActKind::Travel { .. })
            {
                events.extend(world.step(1.0));
                continue;
            }
            mode_act(
                &mut world,
                &ids[seat],
                "speak",
                None,
                Some("我会核对粮食与承诺。"),
            );
        }
        events.extend(world.step(1.0));
    }
    kept.extend(world.take_manuscript());
    let saved = serde_json::to_value(world.snapshot()).unwrap();
    let archive = &saved["game_mode"]["survival10"]["whisper_archive"];
    assert_eq!(archive.as_array().unwrap().len(), 2);
    assert_eq!(archive[0]["status"], "delivered");
    assert_eq!(archive[1]["status"], "recipient_eliminated");
    assert_eq!(archive[1]["content"], second);
    assert_eq!(archive[1]["from"], ids[0]);
    assert_eq!(archive[1]["to"], ids[2]);
    assert!(archive[1]["resolved_at"].is_number());
    kept.extend(world.take_manuscript());
    assert_eq!(
        saved["game_mode"]["survival10"]["pending_whispers"],
        json!([])
    );
    world
        .restore_snapshot(serde_json::from_value(saved).unwrap())
        .unwrap();
    assert!(
        kept.iter()
            .any(|fact| fact.text.contains(first) && fact.text.contains("收到")),
        "收信人的剧本应有真实投递：{kept:?}"
    );
    let undelivered = kept
        .iter()
        .find(|fact| fact.source.ends_with("-recipient-out"))
        .unwrap_or_else(|| panic!("剧本应说明悄悄话没有投递：{kept:?}"));
    assert_eq!(undelivered.visibility, ManuscriptVisibility::Private);
    assert!(undelivered.text.contains("没有投递"), "{undelivered:?}");
    assert_eq!(undelivered.about, vec![ids[0].clone()]);
    assert!(!notes(&world, &ids[2])
        .iter()
        .any(|note| note.contains(second)));
    for secret in [first, second] {
        assert!(!format!("{events:?}").contains(secret));
        assert!(!serde_json::to_string(&world.game_mode_view())
            .unwrap()
            .contains(secret));
        assert!(!serde_json::to_string(&world.game_mode_context(&ids[3]))
            .unwrap()
            .contains(secret));
    }
}

#[test]
fn survival10_director_changes_authoritative_rice_during_collection_and_survives_reload() {
    use kernel::props::SupplyAdjustment;
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    world.step(HOUR);
    survival10_act(&mut world, &ids[0], "take_rice", None).unwrap();
    common::run(
        &mut world,
        &ids[0],
        "take-through-event",
        json!({
            "intent": {"verb": "取粮", "locale": "loc_a"}, "prop_id": "granary"
        }),
    )
    .unwrap();
    world.step(5.0);
    assert_eq!(
        world
            .director_adjust_supply(
                "granary",
                Some("take"),
                SupplyAdjustment::Halve,
                "雨水毁坏部分存粮"
            )
            .unwrap(),
        ("take".into(), 21, 10)
    );
    assert_eq!(granary_on_map(&world), 10);
    assert_eq!(survival10(&world).public_rice, 10);
    assert!(!notes(&world, &ids[1])
        .iter()
        .any(|note| note.contains("雨水毁坏")));
    let saved = serde_json::to_value(world.snapshot()).unwrap();
    assert_eq!(
        saved["game_mode"]["survival10"]["supply_changes"][0]["before"],
        21
    );
    assert_eq!(
        saved["game_mode"]["survival10"]["supply_changes"][0]["after"],
        10
    );
    world
        .restore_snapshot(serde_json::from_value(saved).unwrap())
        .unwrap();
    world.step(5.0);
    assert_eq!(granary_on_map(&world), 9);
    assert_eq!(survival10(&world).seats[0].rice, 2);
    let before = serde_json::to_value(world.snapshot()).unwrap();
    assert!(world
        .director_adjust_supply(
            "granary",
            Some("take"),
            SupplyAdjustment::Subtract(20),
            "不能扣成负数"
        )
        .is_err());
    assert_eq!(serde_json::to_value(world.snapshot()).unwrap(), before);
}

#[test]
fn survival10_night_reminder_is_once_per_night_across_reload_and_legacy_saves() {
    let (mut world, ids) = survival10_world(7.0 * HOUR);
    world.step(HOUR);
    while survival10(&world).phase != Survival10Phase::Night {
        world.step(30.0);
    }
    for id in &ids {
        world.test_give_rice(id, 10);
    }
    survival10_act(&mut world, &ids[0], "guard_home", None).unwrap();
    let until = survival10(&world).until.unwrap();
    world.step(until - world.minutes() - 60.0);
    for _ in 0..20 {
        world.step(0.1);
    }
    let reminders = |world: &World, id: &str| {
        notes(world, id)
            .into_iter()
            .filter(|text| text.contains("还没有提交睡觉、偷公仓、偷别人家或守家"))
            .count()
    };
    assert_eq!(reminders(&world, &ids[0]), 0);
    assert_eq!(reminders(&world, &ids[1]), 1);
    let saved = serde_json::to_value(world.snapshot()).unwrap();
    world
        .restore_snapshot(serde_json::from_value(saved.clone()).unwrap())
        .unwrap();
    world.step(1.0);
    assert_eq!(reminders(&world, &ids[1]), 1);
    let mut legacy = saved;
    legacy["game_mode"]["survival10"]
        .as_object_mut()
        .unwrap()
        .remove("night_reminded");
    for event in legacy["actors"][&ids[1]]["journal"]["events"]
        .as_array_mut()
        .unwrap()
    {
        if let Some(text) = event["text"].as_str() {
            event["text"] =
                json!(text.replace("夜间行动将在一小时内结束", "夜间行动还有一个多小时结束"));
        }
    }
    world
        .restore_snapshot(serde_json::from_value(legacy).unwrap())
        .unwrap();
    let before = world.actor(&ids[1]).unwrap().journal.events.clone();
    world.step(1.0);
    assert_eq!(world.actor(&ids[1]).unwrap().journal.events, before);
    assert_eq!(reminders(&world, &ids[1]), 1);
    world.step(until - world.minutes());
    while survival10(&world).phase != Survival10Phase::Night {
        let view = survival10(&world);
        if view.phase == Survival10Phase::Speech {
            let seat = view.turn.unwrap() as usize - 1;
            let actor = world.actor(&ids[seat]).unwrap();
            if actor.locus.locale != "loc_b"
                || matches!(actor.current.kind, kernel::world::ActKind::Travel { .. })
            {
                world.step(1.0);
                continue;
            }
            let speech = GameModeAction {
                action: "speak".into(),
                text: Some("我会看清规则再行动。".into()),
                target: None,
                reason: None,
            };
            world.game_mode_act(&ids[seat], &speech).unwrap();
        }
        world.step(30.0);
    }
    // 白天会议的截止和入夜的重设发生在同一分钟；先让内核按原截止推进，
    // 再以新进入的夜间截止推进一眼，才是下一夜真正开始。
    let until = survival10(&world).until.unwrap();
    world.step(until - world.minutes() + 1.0);
    assert_eq!(survival10(&world).phase, Survival10Phase::Night);
    let until = survival10(&world).until.unwrap();
    world.step(until - world.minutes() - 59.0);
    assert_eq!(reminders(&world, &ids[1]), 2);
}

/// 公共粮仓是地图上的实体：开局 21 袋，玩法的米数每次变动都写回袋子。
#[test]
fn survival10_granary_on_the_map_tracks_rice() {
    let (mut w, ids) = survival10_world(7.0 * HOUR);
    w.place_prop(kernel::props::Placement {
        instance: kernel::props::Instance {
            id: "user:s10-home_pantry_v2_1".into(),
            definition: "core:private-ration-sack@2".into(),
            locale: "loc_a".into(),
            name: None,
            board: None,
        },
        definition: kernel::props::Catalog::from_json(include_str!(
            "../../../data/props/core/definitions.json"
        ))
        .unwrap()
        .latest("core:private-ration-sack")
        .unwrap()
        .clone(),
    })
    .unwrap();
    assert_eq!(granary_on_map(&w), 21);
    w.step(HOUR);
    assert_eq!(survival10(&w).phase, Survival10Phase::Day);
    assert!(common::run(
        &mut w,
        &ids[0],
        "unregistered-take",
        json!({
            "intent": {"verb": "取粮", "locale": "loc_a"}, "prop_id": "granary"
        })
    )
    .is_err());
    // 站在粮仓的人能领
    assert!(survival10_act(&mut w, &ids[0], "take_rice", None).is_ok());
    let pantry = w
        .prop_views()
        .into_iter()
        .find(|prop| prop.id == "user:s10-home_pantry_v2_1")
        .unwrap();
    assert!(pantry.actions.iter().any(|action| matches!(
        action,
        protocol::PropActionEntry::Defined(view) if view.remaining == Some(Some(1))
    )));
    assert!(w.affordances.for_prop("granary", "take").is_none());
    assert!(w.affordances.for_prop("granary", "取粮").is_some());
    assert!(w
        .affordances
        .for_prop("user:s10-home_pantry_v2_1", "ledger")
        .is_none());
    assert!(w
        .affordances
        .for_prop("user:s10-home_pantry_v2_1", "取粮")
        .is_some());
    assert_eq!(w.inventory_view(&ids[0]).used, 0);
    // 账改小，袋子跟着变
    w.set_map_supply(Some("granary"), "loc_a", "take", 5)
        .unwrap();
    assert_eq!(granary_on_map(&w), 5);
    w.set_map_supply(Some("granary"), "loc_a", "take", 25)
        .unwrap();
    assert_eq!(granary_on_map(&w), 25);
    w.step(1.0);
    assert_eq!(granary_on_map(&w), survival10(&w).public_rice);
    // 换个没有粮仓的地点：找不到就什么也不改
    assert_eq!(w.set_map_supply(None, "loc_b", "take", 3).unwrap(), None);
    let revision = w.actor(&ids[0]).unwrap().actions.revision;
    w.run_actions(
        &ids[0],
        "take-and-leave",
        revision,
        &[
            serde_json::from_value(json!({
                "intent": {"verb": "取粮", "locale": "loc_a"}, "prop_id": "granary"
            }))
            .unwrap(),
            serde_json::from_value(json!({"intent": {"verb": "前往", "locale": "loc_b"}})).unwrap(),
        ],
    )
    .unwrap();
    w.step(9.0);
    assert_eq!(survival10(&w).seats[0].rice, 1);
    assert_eq!(granary_on_map(&w), 21);
    let (fresh, _) = survival10_world(7.0 * HOUR);
    w = common::restore(&w, fresh);
    w.step(1.0);
    assert_eq!(survival10(&w).seats[0].rice, 2);
    assert_eq!(granary_on_map(&w), 20);
    let group = w.action_group(&ids[0], "take-and-leave").unwrap();
    assert_eq!(group.results[0].status, protocol::ActionStatus::Succeeded);
    assert!(group.results[0]
        .changes
        .iter()
        .any(|change| change.contains("已取回1袋米")));
    common::finish(&mut w, &ids[0], "take-and-leave");
    let actions = w
        .action_group(&ids[0], "take-and-leave")
        .unwrap()
        .actions
        .clone();
    let revision = w.actor(&ids[0]).unwrap().actions.revision;
    assert_eq!(
        w.run_actions(&ids[0], "take-and-leave", revision, &actions)
            .unwrap()
            .status,
        GroupStatus::Succeeded
    );
    assert_eq!(w.actor(&ids[0]).unwrap().locus.locale, "loc_b");
    assert_eq!(survival10(&w).seats[0].rice, 2);
    let pantry = w
        .prop_views()
        .into_iter()
        .find(|prop| prop.id == "user:s10-home_pantry_v2_1")
        .unwrap();
    assert!(pantry.actions.iter().any(|action| matches!(action,
        protocol::PropActionEntry::Defined(view) if view.remaining == Some(Some(2))
    )));
    let (fresh, _) = survival10_world(7.0 * HOUR);
    w = common::restore(&w, fresh);
    walk(&mut w, &ids[0], "back-to-granary", "loc_a");
    assert!(common::run(
        &mut w,
        &ids[0],
        "repeat-take",
        json!({
            "intent": {"verb": "取粮", "locale": "loc_a"}, "prop_id": "granary"
        })
    )
    .unwrap_err()
    .to_string()
    .contains("不能重复领取"));
    assert_eq!(granary_on_map(&w), 20);
    w.set_game_mode(None).unwrap();
}

#[test]
fn survival10_granary_interruption_and_last_bag_do_not_create_claims() {
    let (mut w, ids) = survival10_world(7.0 * HOUR);
    w.step(HOUR);
    survival10_act(&mut w, &ids[0], "take_rice", None).unwrap();
    common::run(
        &mut w,
        &ids[0],
        "interrupted-take",
        json!({
            "intent": {"verb": "collect", "locale": "loc_a"}, "prop_id": "granary"
        }),
    )
    .unwrap();
    w.step(9.0);
    w.interrupt_actions(&ids[0], "离开取粮");
    assert_eq!(
        w.action_group(&ids[0], "interrupted-take").unwrap().status,
        GroupStatus::Interrupted
    );
    w.step(1.0);
    assert_eq!(survival10(&w).seats[0].rice, 1);
    assert_eq!(granary_on_map(&w), 21);
    let mut save = serde_json::to_value(w.snapshot()).unwrap();
    save["game_mode"]["survival10"]["public_rice"] = json!(1);
    w.restore_snapshot(serde_json::from_value(save).unwrap())
        .unwrap();
    common::run(&mut w, &ids[0], "retry-take", json!({
        "intent": {"verb": "collect", "locale": "loc_a", "duration_minutes": 60}, "prop_id": "granary"
    })).unwrap();
    survival10_act(&mut w, &ids[1], "take_rice", None).unwrap();
    common::run(
        &mut w,
        &ids[1],
        "concurrent-take",
        json!({
            "intent": {"verb": "collect", "locale": "loc_a"}, "prop_id": "granary"
        }),
    )
    .unwrap();
    w.step(10.0);
    assert_eq!(
        w.action_group(&ids[0], "retry-take").unwrap().status,
        GroupStatus::Succeeded
    );
    assert_eq!(survival10(&w).seats[0].rice, 2, "最后一袋按完成时余额到账");
    assert_eq!(survival10(&w).seats[1].rice, 1);
    assert_eq!(granary_on_map(&w), 0);
    assert!(common::run(
        &mut w,
        &ids[1],
        "empty-take",
        json!({
            "intent": {"verb": "collect", "locale": "loc_a"}, "prop_id": "granary"
        })
    )
    .unwrap_err()
    .to_string()
    .contains("已经空了"));
    let save = serde_json::to_value(w.snapshot()).unwrap();
    assert_eq!(
        save["game_mode"]["survival10"]["day_orders"]["1"]["claimed"],
        json!(false)
    );
    assert_eq!(
        save["game_mode"]["survival10"]["day_orders"]["1"]["collected"],
        json!(false)
    );
    w.step(30.0);
    assert_eq!(granary_on_map(&w), 0);
    assert_eq!(survival10(&w).seats[0].rice, 2);
}

#[test]
fn survival10_granary_night_collection_only_informs_present_witnesses() {
    let (mut w, ids) = survival10_world(7.0 * HOUR);
    w.test_move_granary("loc_a");
    w.step(HOUR);
    while survival10(&w).phase != Survival10Phase::Night {
        w.step(30.0);
    }
    walk(&mut w, &ids[2], "bystander-leaves", "loc_b");
    survival10_act(&mut w, &ids[0], "steal_public", None).unwrap();
    common::run(
        &mut w,
        &ids[0],
        "steal-public",
        json!({
            "intent": {"verb": "collect", "locale": "loc_a"}, "prop_id": "granary"
        }),
    )
    .unwrap();
    w.step(9.0);
    assert_eq!(survival10(&w).seats[0].rice, 0);
    w.step(1.0);
    assert_eq!(
        w.action_group(&ids[0], "steal-public").unwrap().status,
        GroupStatus::Succeeded
    );
    assert_eq!(granary_on_map(&w), 20);
    assert_eq!(survival10(&w).seats[0].rice, 1);
    assert!(notes(&w, &ids[1])
        .iter()
        .any(|note| note.contains("在公仓取米时")));
    assert!(!notes(&w, &ids[2])
        .iter()
        .any(|note| note.contains("在公仓取米时")));
    let Some(GameModeContext::Survival10(context)) = w.game_mode_context(&ids[2]) else {
        panic!("缺少玩法题面")
    };
    assert!(!context
        .known
        .iter()
        .chain(&context.history)
        .any(|note| note.contains("在公仓取米时")));
    assert!(common::run(
        &mut w,
        &ids[0],
        "repeat-steal",
        json!({
            "intent": {"verb": "collect", "locale": "loc_a"}, "prop_id": "granary"
        })
    )
    .is_err());
    w.step(30.0);
    assert_eq!(granary_on_map(&w), 20);
}

/// 夜里进门偷粮：只有真正走到对方住宅、完成入室动作才记账；守家按取粮完成那一刻判断。
#[test]
fn survival10_stealing_into_a_home_is_silent_unless_guarded() {
    let run = |guard: bool, leave: bool| {
        let (mut w, ids) = survival10_night_world(3);
        w.test_give_rice(&ids[0], 1);
        assert!(survival10_act(&mut w, &ids[1], "take_rice", None).is_err());
        assert!(survival10_act(&mut w, &ids[1], "work", None).is_err());
        survival10_act(&mut w, &ids[1], "steal_home", Some("1")).unwrap();
        assert!(survival10_act(&mut w, &ids[1], "steal_public", None).is_err());
        assert!(survival10_act(&mut w, &ids[1], "sleep", None).is_err());
        if guard {
            survival10_act(&mut w, &ids[0], "guard_home", None).unwrap();
        }
        let target_home = w.actor(&ids[0]).unwrap().home.clone().unwrap();
        walk(&mut w, &ids[1], "steal-goto", &target_home);
        assert!(common::run(
            &mut w,
            &ids[1],
            "thief-cannot-sleep",
            json!({
                "intent": {"verb":"sleep", "locale": target_home}, "prop_id":"bed_1"
            })
        )
        .is_err());
        if leave {
            walk(&mut w, &ids[0], "guard-leave", "loc_c");
        }
        w.step(20.0);
        assert_eq!(survival10(&w).seats[0].rice, 1, "仅到场和空等不会自动偷粮");
        common::run(
            &mut w,
            &ids[1],
            "home-collect",
            json!({
                "intent": {"verb": "collect", "locale": target_home},
                "prop_id": "user:s10-home_pantry_v2_1"
            }),
        )
        .unwrap();
        w.step(9.0);
        assert_eq!(survival10(&w).seats[0].rice, 1, "十分钟动作未完成不能偷");
        w.step(1.0);
        if guard && !leave {
            assert!(common::run(
                &mut w,
                &ids[0],
                "guard-cannot-sleep",
                json!({
                    "intent": {"verb":"sleep", "locale": target_home}, "prop_id":"bed_1"
                })
            )
            .is_err());
        }
        // 登记只有一次夜间行动：下一个动作组必须在启动前就被拒绝，不能先排队到其他
        // 已经结束的动作后面、等一会儿才失败。
        assert!(common::run(
            &mut w,
            &ids[1],
            "repeat-home-collect",
            json!({
                "intent": {"verb": "collect", "locale": target_home},
                "prop_id": "user:s10-home_pantry_v2_1"
            }),
        )
        .is_err());
        let result = (
            notes(&w, &ids[1]),
            notes(&w, &ids[0]),
            notes(&w, &ids[2]),
            survival10(&w).seats[0].rice,
            survival10(&w).seats[1].rice,
        );
        while survival10(&w).day < 2 {
            w.step(30.0);
        }
        result
    };
    let (thief, victim, bystander, victim_rice, thief_rice) = run(false, false);
    assert_eq!((victim_rice, thief_rice), (0, 1));
    assert!(
        thief
            .iter()
            .any(|t| t.contains("从1号h0家的私粮袋取得1袋米")),
        "{thief:?}"
    );
    assert!(victim.iter().any(|t| t.contains("少了1袋")), "{victim:?}");
    assert!(
        !victim.iter().any(|t| t.contains("2号h1")),
        "被偷的人不该知道是谁：{victim:?}"
    );
    assert!(
        !bystander
            .iter()
            .any(|t| t.contains("看见") || t.contains("翻私粮袋")),
        "{bystander:?}"
    );

    let (thief, victim, _, victim_rice, thief_rice) = run(true, false);
    assert_eq!((victim_rice, thief_rice), (1, 0));
    assert!(
        thief.iter().any(|t| t.contains("被对方守家阻止")),
        "{thief:?}"
    );
    assert!(
        victim.iter().any(|t| t.contains("阻止了对方")),
        "{victim:?}"
    );

    let (thief, _, _, victim_rice, thief_rice) = run(true, true);
    assert_eq!((victim_rice, thief_rice), (0, 1), "离开守家后偷窃应成功");
    assert!(
        thief
            .iter()
            .any(|t| t.contains("从1号h0家的私粮袋取得1袋米")),
        "{thief:?}"
    );
}

/// 白天行动走完整路径：劳动中不能交谈，主动结束劳动后仍可继续累计到八小时；
/// 领米动作没完成不到账，同一机会不能改选。存档重开后劳动分钟和已领取状态都还在。
#[test]
fn survival10_day_actions_follow_real_work_and_granary_visits() {
    let ids: Vec<String> = (0..7).map(|n| format!("h{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .enumerate()
        .map(|(n, id)| {
            let locale: &'static str = ["loc_a", "loc_b", "loc_c"][n % 3];
            (
                id.as_str(),
                ["甲", "乙", "丙", "丁", "戊", "己", "庚"][n],
                locale,
            )
        })
        .collect();
    let mut w = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    for (n, (id, _)) in ids.iter().zip(people.iter()).enumerate() {
        let locale = format!("test_{}", n + 1);
        w.actor_for_test(id).home = Some(locale.clone());
    }
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    place_survival10_real_props(&mut w, true);
    w.set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(5) }))
        .unwrap();
    w.test_move_granary("loc_c");
    w.step(HOUR);
    assert_eq!(survival10(&w).phase, Survival10Phase::Day);
    assert!(
        common::run(
            &mut w,
            &ids[0],
            "day-pantry",
            json!({
                "intent": {"verb": "collect", "locale": "test_1"},
                "prop_id": "user:s10-home_pantry_v2_1"
            })
        )
        .is_err(),
        "白天不能用私粮袋绕过二选一"
    );
    let answer = survival10_act(&mut w, &ids[1], "work", None).unwrap();
    assert!(answer.contains("白天结束时按报名结算"), "{answer}");
    assert!(survival10_act(&mut w, &ids[1], "take_rice", None).is_err());

    // 2 号走到分配的田块，八小时动作中途聊天，回来只需补足剩余时间。
    walk(&mut w, &ids[1], "farm-goto", "loc_b");
    let farm = survival10(&w).seats[1].farm.clone().unwrap();
    common::run(
        &mut w,
        &ids[1],
        "farm-work",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": farm
        }),
    )
    .unwrap();
    w.step(4.0 * HOUR);
    let working = &survival10(&w).seats[1];
    assert_eq!(working.worked_minutes, 240.0);
    assert!(
        (240.0..=241.0).contains(&working.labor_progress),
        "未到下一分钟结算的劳动也要进入右上角进度：{}",
        working.labor_progress
    );
    assert!(working.working_now, "右上角的劳动进度必须对应当前真实动作");
    walk(&mut w, &ids[0], "farm-visitor", "loc_b");
    let talk = |w: &mut World, key: &str, actor: &str, partner: &str| {
        w.interact(
            actor,
            key,
            w.actor(actor).unwrap().actions.revision,
            &protocol::InteractionCommand::Offer {
                partner_id: partner.into(),
                interaction: protocol::InteractionKind::Talk,
                message: "聊两句".into(),
            },
        )
    };
    assert!(talk(&mut w, "farm-work-talk", &ids[0], &ids[1])
        .unwrap_err()
        .to_string()
        .contains("不能开始交谈"));
    assert!(
        survival10(&w).seats[1].working_now,
        "交谈被拒绝后仍在真实劳动"
    );
    w.step(10.0);
    let working = &survival10(&w).seats[1];
    assert!(
        (250.0..=253.0).contains(&working.labor_progress),
        "被拒绝的交谈不能停表：{}",
        working.labor_progress
    );
    w.interrupt_actions(&ids[1], "本人结束劳动");
    w.step(5.0);
    let before_break = survival10(&w).seats[1].worked_minutes;
    let invited = talk(&mut w, "farm-break-talk", &ids[0], &ids[1]).unwrap();
    w.interact(
        &ids[1],
        "farm-break-reply",
        w.actor(&ids[1]).unwrap().actions.revision,
        &protocol::InteractionCommand::Reply {
            interaction_id: invited.id.clone(),
            accept: true,
            message: "好".into(),
        },
    )
    .unwrap();
    w.step(30.0);
    let talking = &survival10(&w).seats[1];
    assert_eq!(
        talking.worked_minutes, before_break,
        "主动停劳动后交谈期间不计劳动"
    );
    assert_eq!(
        talking.labor_progress, talking.worked_minutes,
        "聊天期间界面不能继续涨时间"
    );
    assert!(!talking.working_now, "聊天中不能再显示农田劳作中");
    assert!(survival10_act(&mut w, &ids[1], "take_rice", None).is_err());
    let mut fresh = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut fresh, true);
    w = common::restore(&w, fresh);
    assert_eq!(survival10(&w).seats[1].farm.as_ref(), Some(&farm));
    w.step(1.0);
    assert_eq!(survival10(&w).seats[1].worked_minutes, before_break);
    w.interrupt_actions(&ids[1], "交谈结束，恢复田里劳动");
    common::run(
        &mut w,
        &ids[1],
        "farm-resume",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": farm
        }),
    )
    .unwrap();
    w.step(8.0 * HOUR - before_break - 1.0);
    assert_eq!(survival10(&w).seats[1].worked_minutes, 479.0);
    w.step(1.0);
    assert_eq!(
        w.action_group(&ids[1], "farm-resume").unwrap().status,
        GroupStatus::Succeeded
    );
    assert_eq!(survival10(&w).seats[1].worked_minutes, 480.0);
    assert_eq!(survival10(&w).public_rice, 21, "劳动完成也要到晚餐才入账");

    // 1 号只在原地登记工作；3 号登记领米却没有取粮。
    survival10_act(&mut w, &ids[0], "work", None).unwrap();
    survival10_act(&mut w, &ids[4], "work", None).unwrap();
    survival10_act(&mut w, &ids[2], "take_rice", None).unwrap();
    w.step(30.0);
    assert_eq!(survival10(&w).seats[0].worked_minutes, 0.0);
    assert_eq!(survival10(&w).seats[2].rice, 1);

    // 4 号真实走到粮仓，做「取粮」；动作完成那一刻立即到账。
    walk(&mut w, &ids[3], "take-goto", "loc_c");
    survival10_act(&mut w, &ids[3], "take_rice", None).unwrap();
    let public = survival10(&w).public_rice;
    common::run(
        &mut w,
        &ids[3],
        "take-rice",
        json!({
            "intent": {"verb": "取粮", "locale": "loc_c"}, "prop_id": "granary"
        }),
    )
    .unwrap();
    w.step(9.0);
    assert_eq!(survival10(&w).seats[3].rice, 1, "取粮还没完成");
    w.step(2.0);
    let v = survival10(&w);
    assert_eq!(v.seats[3].rice, 2, "取粮完成才到账");
    assert_eq!(v.public_rice, public - 1, "到账同时从公仓扣除");

    while survival10(&w).phase == Survival10Phase::Day {
        w.step(30.0);
    }
    assert_eq!(
        survival10(&w).produced_today,
        3,
        "后台按报名结算，实际劳动进度不影响产粮"
    );
    assert_eq!(survival10(&w).public_rice, public - 1 + 3);
    // 存档重开：阶段、劳动和已领取的米仍在。
    let mut fresh = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut fresh, true);
    let mut restored = common::restore(&w, fresh);
    let r = survival10(&restored);
    assert_eq!(r.phase, survival10(&w).phase);
    assert_eq!(r.public_rice, survival10(&w).public_rice);
    assert!(r
        .seats
        .iter()
        .all(|s| s.rice == survival10(&w).seats[s.seat as usize - 1].rice));
    restored.step(1.0);
    assert_eq!(
        survival10(&restored).public_rice,
        survival10(&w).public_rice
    );
}

#[test]
fn survival10_guidance_resumes_idle_and_saved_plans_before_real_actions() {
    let ids: Vec<_> = (0..7).map(|number| format!("guided_{number}")).collect();
    let locales: Vec<_> = (1..=7).map(|number| format!("test_{number}")).collect();
    let people: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            (
                id.as_str(),
                id.as_str(),
                if index == 0 {
                    "loc_a"
                } else {
                    locales[index].as_str()
                },
            )
        })
        .collect();
    let mut world = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut world, true);
    for (index, id) in ids.iter().enumerate() {
        world.actor_for_test(id).home = Some(format!("test_{}", index + 1));
    }
    world.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    world
        .set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(5) }))
        .unwrap();
    world.step(HOUR);
    survival10_act(&mut world, &ids[0], "work", None).unwrap();
    world.step(10.0);
    let farmer = world.actor(&ids[0]).unwrap();
    assert_eq!(farmer.locus.locale, "loc_b");
    assert!(farmer.plan.is_empty());
    assert_eq!(farmer.current.verb, "停留");
    assert_eq!(survival10(&world).seats[0].worked_minutes, 0.0);
    common::run(
        &mut world,
        &ids[0],
        "real-work",
        json!({"intent": {"verb": "work"}, "prop_id": "user:s10-farm-v2_1"}),
    )
    .unwrap();
    world.step(60.0);
    assert_eq!(survival10(&world).seats[0].worked_minutes, 60.0);
    while survival10(&world).phase != Survival10Phase::Night {
        world.step(30.0);
    }
    survival10_act(&mut world, &ids[1], "sleep", None).unwrap();
    world.step(10.0);
    let sleeper = world.actor(&ids[1]).unwrap();
    let home = sleeper.locus.locale.clone();
    let idle = sleeper.current.clone();
    assert!(sleeper.plan.is_empty());
    assert_eq!(home, "test_2");
    assert_eq!(sleeper.current.verb, "停留");
    world
        .actor_for_test(&ids[1])
        .plan
        .push_back(kernel::action_state::QueuedAction::Untracked(
            kernel::intent::Intent::Goto {
                locale: home,
                station: None,
            },
        ));
    world.actor_for_test(&ids[1]).current = idle;
    let before = survival10(&world);
    let mut fresh = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut fresh, true);
    world = common::restore(&world, fresh);
    assert_eq!(survival10(&world), before);
    world.step(1.0);
    assert!(world.actor(&ids[1]).unwrap().plan.is_empty());
    common::run(
        &mut world,
        &ids[1],
        "real-sleep",
        json!({"intent": {"verb": "睡觉"}, "prop_id": "bed_2"}),
    )
    .unwrap();
    world.step(1.0);
    assert_eq!(world.actor(&ids[1]).unwrap().current.verb, "睡觉");
    world.step(480.0);
    assert_eq!(world.actor(&ids[1]).unwrap().current.verb, "停留");
    assert_eq!(
        world.action_group(&ids[1], "real-sleep").unwrap().status,
        GroupStatus::Succeeded
    );
}

#[test]
fn survival10_four_daily_registrations_work_in_parallel_and_survive_reload() {
    let ids: Vec<_> = (0..7).map(|number| format!("farmer_{number}")).collect();
    let people: Vec<_> = ids
        .iter()
        .map(|id| (id.as_str(), id.as_str(), "loc_b"))
        .collect();
    let mut w = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut w, false);
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    w.set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(5) }))
        .unwrap();
    for id in &ids {
        w.test_give_rice(id, 10);
    }
    w.step(HOUR);
    for id in &ids[..3] {
        survival10_act(&mut w, id, "work", None).unwrap();
    }
    for (index, id) in ids[..4].iter().enumerate() {
        common::run(
            &mut w,
            id,
            "work",
            json!({
                "intent": {"verb": "work", "locale": "loc_b"},
                "prop_id": format!("user:s10-farm-v2_{}", index + 1)
            }),
        )
        .unwrap();
    }
    w.step(2.0 * HOUR);
    let before = survival10(&w);
    assert_eq!(before.registered_today, 4);
    assert!(before.seats[..4]
        .iter()
        .all(|seat| seat.worked_minutes == 120.0 && seat.farm.is_some()));
    let rejected = survival10_act(&mut w, &ids[4], "work", None).unwrap_err();
    assert!(rejected.to_string().contains("名额已满"), "{rejected}");
    assert!(common::run(
        &mut w,
        &ids[4],
        "bypass",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": "user:s10-farm-v2_4"
        })
    )
    .is_err());
    assert!(common::run(
        &mut w,
        &ids[0],
        "wrong-farm",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": "user:s10-farm-v2_2"
        })
    )
    .is_err());
    let mut fresh = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut fresh, false);
    w = common::restore(&w, fresh);
    assert_eq!(survival10(&w), before);
    w.interrupt_actions(&ids[3], "中途休息");
    assert!(survival10_act(&mut w, &ids[4], "work", None).is_err());
    survival10_act(&mut w, &ids[4], "take_rice", None).unwrap();
    assert!(common::run(
        &mut w,
        &ids[4],
        "take-bypass",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": "user:s10-farm-v2_4"
        })
    )
    .is_err());
    common::run(
        &mut w,
        &ids[3],
        "resume",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": "user:s10-farm-v2_4"
        }),
    )
    .unwrap();
    w.step(6.0 * HOUR);
    assert!(survival10(&w).seats[..4]
        .iter()
        .all(|seat| seat.worked_minutes == 480.0));
    assert_eq!(survival10(&w).public_rice, 21);
    assert!(common::run(
        &mut w,
        &ids[0],
        "repeat-work",
        json!({
            "intent": {"verb": "work", "locale": "loc_b"}, "prop_id": "user:s10-farm-v2_1"
        })
    )
    .is_err());
    while survival10(&w).phase == Survival10Phase::Day {
        w.step(30.0);
    }
    assert_eq!(survival10(&w).produced_today, 4);
    assert_eq!(survival10(&w).public_rice, 25);
    w.step(30.0);
    assert_eq!(survival10(&w).public_rice, 25);
    while survival10(&w).day < 2 {
        w.step(30.0);
    }
    let next = survival10(&w);
    assert_eq!(next.registered_today, 0);
    assert_eq!(next.produced_today, 0);
    assert!(next
        .seats
        .iter()
        .all(|seat| seat.farm.is_none() && seat.worked_minutes == 0.0));
    survival10_act(&mut w, &ids[4], "work", None).unwrap();
    assert_eq!(survival10(&w).registered_today, 1);
}

/// 偷同一目标的真实取粮区间重叠，多方都不转账；先后完成、不同目标仍能正常取得。
/// 判定在后到的动作开始时统一完成，先开始的人也不能先拿米。
#[test]
fn survival10_overlapping_theft_collides_before_any_rice_moves() {
    for home in [false, true] {
        for order in [[0, 1], [1, 0]] {
            let (mut w, ids) = survival10_night_world(7);
            w.test_give_rice(&ids[2], 3);
            let (action, target, locale, prop) = if home {
                (
                    "steal_home",
                    Some("3"),
                    "test_3",
                    "user:s10-home_pantry_v2_3",
                )
            } else {
                ("steal_public", None, "loc_c", "granary")
            };
            for &seat in &order {
                survival10_act(&mut w, &ids[seat], action, target).unwrap();
                walk(&mut w, &ids[seat], "reach-target", locale);
            }
            let before = survival10(&w);
            for (index, &seat) in order.iter().enumerate() {
                common::run(
                    &mut w,
                    &ids[seat],
                    "collect",
                    json!({
                        "intent": {"verb": "collect", "locale": locale}, "prop_id": prop
                    }),
                )
                .unwrap();
                w.step(if index == 0 { 4.0 } else { 1.0 });
                let saved = serde_json::to_value(w.snapshot()).unwrap();
                w.restore_snapshot(serde_json::from_value(saved).unwrap())
                    .unwrap();
            }
            w.step(10.0);
            for &seat in &order {
                assert!(matches!(
                    w.action_group(&ids[seat], "collect").unwrap().status,
                    GroupStatus::Failed | GroupStatus::Interrupted
                ));
                assert!(notes(&w, &ids[seat])
                    .iter()
                    .any(|text| text.contains("偷粮撞车")));
                assert!(common::run(
                    &mut w,
                    &ids[seat],
                    "retry",
                    json!({
                        "intent": {"verb": "collect", "locale": locale}, "prop_id": prop
                    })
                )
                .is_err());
            }
            let after = survival10(&w);
            assert_eq!(after.public_rice, before.public_rice);
            assert_eq!(
                after.seats.iter().map(|seat| seat.rice).collect::<Vec<_>>(),
                before
                    .seats
                    .iter()
                    .map(|seat| seat.rice)
                    .collect::<Vec<_>>()
            );
            assert!(!notes(&w, &ids[3])
                .iter()
                .any(|text| text.contains("偷粮撞车")));
            walk(&mut w, &ids[order[0]], "leave-collision", "loc_b");
            assert_eq!(w.actor(&ids[order[0]]).unwrap().locus.locale, "loc_b");
        }
    }
}

#[test]
fn survival10_simultaneous_thefts_at_distinct_targets_conserve_rice() {
    let (mut world, ids) = survival10_night_world(7);
    for id in &ids[3..5] {
        world.test_give_rice(id, 2);
    }
    let targets = [
        ("steal_public", None, "loc_c", "granary"),
        (
            "steal_home",
            Some("4"),
            "test_4",
            "user:s10-home_pantry_v2_4",
        ),
        (
            "steal_home",
            Some("5"),
            "test_5",
            "user:s10-home_pantry_v2_5",
        ),
    ];
    for (seat, &(action, target, locale, _)) in targets.iter().enumerate() {
        survival10_act(&mut world, &ids[seat], action, target).unwrap();
        walk(&mut world, &ids[seat], "reach-target", locale);
    }
    for (seat, &(_, _, locale, prop)) in targets.iter().enumerate() {
        common::run(
            &mut world,
            &ids[seat],
            "collect",
            json!({
                "intent": {"verb": "collect", "locale": locale}, "prop_id": prop
            }),
        )
        .unwrap();
    }
    world.step(11.0);
    let view = survival10(&world);
    assert_eq!(view.public_rice, 20);
    assert!(view.seats[..3].iter().all(|seat| seat.rice == 1));
    assert!(view.seats[3..5].iter().all(|seat| seat.rice == 1));
    assert_eq!(
        view.public_rice + view.seats.iter().map(|seat| seat.rice).sum::<u32>(),
        25
    );
}

/// 开局配给只在新局发生；晚餐照扣，保存重开甚至旧档里手写 0 袋也不会再补发。
#[test]
fn survival10_initial_ration_is_once_per_new_game() {
    let (mut w, ids) = survival10_world(7.0 * HOUR);
    let view = survival10(&w);
    assert_eq!(view.public_rice, 21);
    assert_eq!(view.seats.iter().map(|seat| seat.rice).sum::<u32>(), 7);
    w.step(11.0 * HOUR);
    let dinner = survival10(&w);
    assert_eq!(dinner.phase, Survival10Phase::Night);
    assert_eq!(dinner.seats.iter().map(|s| s.rice).sum::<u32>(), 0);
    assert!(notes(&w, &ids[0])
        .iter()
        .any(|text| text.contains("开局配给每人1袋私粮")));
    let people: Vec<_> = ids
        .iter()
        .map(|id| (id.as_str(), id.as_str(), "loc_a"))
        .collect();
    let fresh = common::world_with(common::tiny_topology(), 7.0 * HOUR, &people);
    let mut restored = common::restore(&w, fresh);
    assert!(survival10(&restored)
        .seats
        .iter()
        .all(|seat| seat.rice == 0));
    let mut legacy = serde_json::to_value(restored.snapshot()).unwrap();
    for seat in legacy["game_mode"]["survival10"]["seats"]
        .as_array_mut()
        .unwrap()
    {
        seat["rice"] = json!(0);
    }
    restored
        .restore_snapshot(serde_json::from_value(legacy).unwrap())
        .unwrap();
    assert!(survival10(&restored)
        .seats
        .iter()
        .all(|seat| seat.rice == 0));
}

/// 守家登记后，下一晚既不在动作列表里、硬调也失败；失败、离家和重开都不能绕过。
#[test]
fn survival10_guard_cooldown_survives_reload_and_old_saves() {
    let (mut w, ids) = survival10_night_world(3);
    for id in &ids {
        w.test_give_rice(id, 5);
    }
    survival10_act(&mut w, &ids[0], "guard_home", None).unwrap();
    let mut legacy = serde_json::to_value(w.snapshot()).unwrap();
    legacy["game_mode"]["survival10"]["seats"][0]
        .as_object_mut()
        .unwrap()
        .remove("last_guard_day");
    w.restore_snapshot(serde_json::from_value(legacy).unwrap())
        .unwrap();
    while survival10(&w).phase != Survival10Phase::Day {
        w.step(30.0);
    }
    for _ in 0..100 {
        if survival10(&w).phase == Survival10Phase::Night {
            break;
        }
        if survival10(&w).phase == Survival10Phase::Speech {
            let seat = survival10(&w).turn.unwrap() as usize - 1;
            mode_act(
                &mut w,
                &ids[seat],
                "speak",
                None,
                Some("我会权衡资源与信誉。"),
            );
        }
        w.step(30.0);
    }
    let context = w.game_mode_context(&ids[0]).unwrap();
    let GameModeContext::Survival10(context) = context else {
        panic!("缺少玩法题面")
    };
    assert!(!context.actions.iter().any(|action| action == "guard_home"));
    let rejected = survival10_act(&mut w, &ids[0], "guard_home", None).unwrap_err();
    assert!(rejected.to_string().contains("上一晚已经登记守家"));
    let people: Vec<_> = ids
        .iter()
        .map(|id| (id.as_str(), id.as_str(), "loc_c"))
        .collect();
    let mut fresh = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    place_survival10_real_props(&mut fresh, true);
    let mut restored = common::restore(&w, fresh);
    assert!(survival10_act(&mut restored, &ids[0], "guard_home", None).is_err());
    let cooldown = restored.snapshot();
    let mut legacy = serde_json::to_value(restored.snapshot()).unwrap();
    legacy["game_mode"]["survival10"]["seats"][0]
        .as_object_mut()
        .unwrap()
        .remove("last_guard_day");
    restored
        .restore_snapshot(serde_json::from_value(legacy).unwrap())
        .unwrap();
    assert!(survival10_act(&mut restored, &ids[0], "guard_home", None).is_ok());
    restored.restore_snapshot(cooldown).unwrap();
    for _ in 0..100 {
        if survival10(&restored).day == 3 && survival10(&restored).phase == Survival10Phase::Night {
            break;
        }
        restored.step(30.0);
    }
    assert_eq!(survival10(&restored).day, 3);
    assert!(survival10_act(&mut restored, &ids[0], "guard_home", None).is_ok());
}

/// 夜间目击只看取粮完成那一刻真正在场、醒着的人：两人都取到才互相看见，
/// 只走了半路的人看不到；不相干的人收到的事只属于自己的经历。
#[test]
fn survival10_night_witnesses_only_people_actually_there() {
    let ids: Vec<String> = (0..7).map(|n| format!("h{n}")).collect();
    let people: Vec<(&str, &str, &str)> = ids
        .iter()
        .map(|id| (id.as_str(), id.as_str(), "loc_c"))
        .collect();
    let mut w = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    w.set_physiology(PhysiologySwitches {
        fullness: false,
        energy: false,
        water: false,
        health: false,
        weather: false,
    });
    place_survival10_real_props(&mut w, true);
    w.set_game_mode(Some(&GameModeChoice::Survival10 { seed: Some(7) }))
        .unwrap();
    w.test_move_granary("loc_c");
    for id in &ids[2..] {
        walk(&mut w, id, "leave-granary", "loc_b");
    }
    w.step(HOUR);
    while survival10(&w).phase != Survival10Phase::Night {
        w.step(30.0);
    }
    for id in ids.iter().skip(3) {
        survival10_act(&mut w, id, "sleep", None).unwrap();
    }
    for id in [&ids[0], &ids[1], &ids[2]] {
        survival10_act(&mut w, id, "steal_public", None).unwrap();
    }
    for id in &ids[2..] {
        walk(&mut w, id, "stay-away", "loc_b");
    }
    // 1、2 号真的在粮仓取粮；3 号到粮仓后不取。
    for id in [&ids[0], &ids[1]] {
        walk(&mut w, id, "night-goto", "loc_c");
        common::run(
            &mut w,
            id,
            "night-take",
            json!({
                "intent": {"verb": "取粮", "locale": "loc_c"}, "prop_id": "granary"
            }),
        )
        .unwrap();
        w.step(12.0);
    }
    walk(&mut w, &ids[2], "night-goto", "loc_c");
    w.step(30.0);
    assert!(notes(&w, &ids[0])
        .iter()
        .any(|t| t.contains("看见了") && t.contains("2号h1")));
    assert!(notes(&w, &ids[1])
        .iter()
        .any(|t| t.contains("看见了") && t.contains("1号h0")));
    assert!(
        !notes(&w, &ids[2]).iter().any(|t| t.contains("看见了")),
        "{:?}",
        notes(&w, &ids[2])
    );
    assert!(!notes(&w, &ids[3])
        .iter()
        .any(|t| t.contains("看见了") || t.contains("公仓取米")));

    // 存档重开不能重新提交当夜的选择，也不能重复取米。
    let fresh = common::world_with(survival10_real_topology(), 7.0 * HOUR, &people);
    let mut restored = common::restore(&w, fresh);
    assert!(survival10_act(&mut restored, &ids[1], "steal_public", None).is_err());
    let rice = survival10(&restored).seats[1].rice;
    restored.step(60.0);
    assert_eq!(survival10(&restored).seats[1].rice, rice);
}
