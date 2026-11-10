//! 导演的题面：规则、工具表（OpenAI 函数格式）、当前世界与最近对话。只读世界，不碰村民会话。
use crate::runtime::Runtime;
use protocol::DirectorEntry;
use serde_json::{json, Value};

/// 摘要最多留这么多字；超过就把最早的一半换掉。
const SUMMARY_MAX_CHARS: usize = 1200;
/// 题面末尾最多列多少件道具；库更大时模型仍可按 world_view 里的编号投放。
const CATALOG_INLINE: usize = 240;

const RULES: &str = "\
你是这个小镇游戏的导演 Agent，替玩家查看和控制世界。玩家用一句口语下指令，你把它变成对世界的一项项操作或一段有依据的剧情回顾。

你可以：
- 只读：look 看当前全景；watch 看最近动静；trace 按分钟回看更长时间线；inspect 看某个村民的目标、计划、身体、记忆、日记、想法和最近的交谈（包括私聊）；read_content 读公告、书籍的正文；history 看这次对话里做过什么。
- 改世界：投放道具库里已有的道具、移除已投放的道具、张贴公告、通知村民、预约之后发生的事、改天气。
- 你可以读到村民的想法与私聊，但要把它当成判断剧情的原料，最终回答只总结局势、动机与对世界的影响；
  不要复述私聊或心声的原文，也不要把村民不知道的事当作他们知道的事实。
你不能：改写村民的目标、记忆、人设或心声，不能加人或删人，不能改村民的身体数值，不能重置世界，不能改时钟与速度。
要让村民改变主意，就通过通知、公告、道具和环境让他们自己看到、自己决定。
所有改动都只是提议，由世界内核校验后才会生效；道具只能投放道具库里已有的定义，不能凭空造新道具。
道具可用名字，但同名有歧义或名字不在库里时按定义编号来；地点用下面列出的地点编号。
每次最多调 4 个改变世界的工具。已经明确的操作直接做；玩家问“最近怎么样”“总结剧情”“谁在做什么”时，先用一个或多个只读工具看世界，再用正文回答。
最终回答必须有正文，简短、具体：说明你看到了什么、判断了什么；若有操作，再说明做了什么、为什么，不要复述数据。只调用工具而不给正文，玩家就看不到你的回应。";

pub(crate) fn request_body(runtime: &Runtime, text: &str, model: String) -> Value {
    let mut messages = vec![json!({"role": "system", "content": RULES})];
    if !runtime.director.summary().is_empty() {
        messages.push(json!({
            "role": "system",
            "content": format!("此前对话的摘要：\n{}", runtime.director.summary())
        }));
    }
    for entry in runtime.director.entries() {
        messages.push(json!({"role": "user", "content": entry.player}));
        let mut reply = entry.reply.clone();
        for effect in &entry.effects {
            reply.push_str(&format!(
                "\n- {}：{}",
                if effect.ok { "已做" } else { "未做" },
                effect.text
            ));
        }
        if let Some(error) = &entry.error {
            reply.push_str(&format!("\n- 失败：{error}"));
        }
        if reply.trim().is_empty() {
            continue;
        }
        messages.push(json!({"role": "assistant", "content": reply}));
    }
    // world_view 已含道具库全表；单独把名字→定义编号的对照放在末尾，模型更容易照抄编号。
    let catalog: Vec<_> = runtime
        .prop_library
        .placeable()
        .take(CATALOG_INLINE)
        .map(|d| format!("{} {}", d.name, d.id))
        .collect();
    messages.push(json!({
        "role": "user",
        "content": format!(
            "当前世界：\n{}\n\n道具名字与定义编号的对照（也可只用名字；以道具库为准）：\n{}\n\n玩家刚才说：{}",
            crate::conductor::context::world_view(runtime),
            if catalog.is_empty() { "（空）".into() } else { catalog.join("\n") },
            text
        )
    }));
    json!({
        "model": model,
        "max_tokens": 1500,
        "messages": messages,
        "tools": tools(),
        "tool_choice": "auto",
    })
}

/// 补正文：在题面末尾加一段系统提示，保留已经读到的工具结果。
pub(crate) fn append_exchange(body: &mut Value, note: &str) {
    if let Some(items) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
        items.push(json!({"role": "system", "content": note}));
    }
}

/// 把太早的一轮折进摘要：只留玩家原话与做了什么，丢掉模型的长篇解释。
pub(crate) fn fold(summary: &mut String, entry: &DirectorEntry) {
    summary.push_str(&format!("\n玩家：{}", entry.player));
    if !entry.reply.trim().is_empty() {
        summary.push_str(&format!("\n导演：{}", entry.reply));
    }
    for effect in &entry.effects {
        summary.push_str(&format!(
            "\n- {}：{}",
            if effect.ok { "已做" } else { "未做" },
            effect.text
        ));
    }
    let count = summary.chars().count();
    if count > SUMMARY_MAX_CHARS {
        *summary = summary
            .chars()
            .skip(count - SUMMARY_MAX_CHARS / 2)
            .collect();
    }
}

fn tools() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{
            "name":"look",
            "description":"重新看当前世界：时钟、天气、地点、在场村民、已投放的道具、道具库里能放的、待触发的事。",
            "parameters":{"type":"object","properties":{},"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"survey",
            "description":"快速全景：每个在场村民在哪、在做什么、接下来想做什么，以及最近的世界动静。适合回答“现在怎么样了”。",
            "parameters":{"type":"object","properties":{},"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"watch",
            "description":"看最近发生的世界动静（时间线）。总结剧情时先用它。",
            "parameters":{"type":"object","properties":{
                "minutes":{"type":"number","description":"回看多少模拟分钟；不给默认 12 小时"}
            },"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"trace",
            "description":"更长的剧情时间线，适合追溯几天的走向；只读，不影响村民。",
            "parameters":{"type":"object","properties":{
                "minutes":{"type":"number","description":"回看多少模拟分钟；不给默认 24 小时"}
            },"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"inspect",
            "description":"读一个村民：目标、计划、身体、记忆、今天做过什么、想法、最近交谈（包括私聊）。用来理解动机和剧情；回答时总结，不要原样复述私聊或心声。",
            "parameters":{"type":"object","properties":{
                "actor":{"type":"string","description":"村民名字或编号"}
            },"required":["actor"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"read_content",
            "description":"读一份公告、书籍或告示的正文。",
            "parameters":{"type":"object","properties":{
                "id":{"type":"string","description":"公告栏道具编号或作品编号"}
            },"required":["id"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"history",
            "description":"看这次导演对话里玩家下过什么指令、你做过什么。复盘时用。",
            "parameters":{"type":"object","properties":{},"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"announce",
            "description":"马上告诉村民一件事。给了地点就只告诉那里的村民。",
            "parameters":{"type":"object","properties":{
                "text":{"type":"string","description":"通知正文，1–800 字"},
                "locale":{"type":"string","description":"地点编号；不给就通知全镇在场村民"}
            },"required":["text"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"schedule",
            "description":"预约之后发生一件事：到点才通知。",
            "parameters":{"type":"object","properties":{
                "text":{"type":"string","description":"通知正文，1–800 字"},
                "after_minutes":{"type":"number","description":"多少模拟分钟后触发，1 到 10080"},
                "locale":{"type":"string","description":"地点编号；不给就通知全镇"}
            },"required":["text","after_minutes"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"place_prop",
            "description":"在某个地点投放道具库里已有的一件道具。",
            "parameters":{"type":"object","properties":{
                "definition":{"type":"string","description":"道具库里的定义编号；也可以给库里唯一的道具名"},
                "locale":{"type":"string","description":"地点编号"},
                "name":{"type":"string","description":"这件道具的名字；不给就用定义名"}
            },"required":["definition","locale"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"remove_prop",
            "description":"移除一件已投放的道具。",
            "parameters":{"type":"object","properties":{
                "id":{"type":"string","description":"道具实例编号"}
            },"required":["id"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"post_bulletin",
            "description":"在某个地点已有的公告栏上贴一张公告。",
            "parameters":{"type":"object","properties":{
                "locale":{"type":"string","description":"地点编号"},
                "board":{"type":"string","description":"公告栏道具编号"},
                "title":{"type":"string","description":"标题"},
                "text":{"type":"string","description":"正文"}
            },"required":["locale","board","title","text"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"set_weather",
            "description":"改天气：fixed 指定天气，natural 交给天气规则。",
            "parameters":{"type":"object","properties":{
                "mode":{"type":"string","enum":["fixed","natural"]},
                "kind":{"type":"string","enum":["clear","overcast","light_rain","rain","heavy_rain","thunderstorm","snow"]}
            },"required":["mode"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"narrate",
            "description":"只给玩家看的气氛文字，不进入村民的世界。",
            "parameters":{"type":"object","properties":{
                "text":{"type":"string"}
            },"required":["text"],"additionalProperties":false}
        }}),
    ]
}
