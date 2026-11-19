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
- 只读：look 看当前全景摘要；watch、trace 按分钟过滤本次进程的有限近期日志，不代表完整历史；inspect 看某个村民的目标、计划、身体、记忆、日记、想法和最近的交谈（包括私聊）的摘要；read_content 读公告、书籍的正文摘要；history 看这次对话里做过什么。
- query_data 查询权威后台数据。先用空 path 看目录，再用 JSON Pointer 路径按需深入：/world/actors、/world/inventory、/world/interactions、/world/contents、/agents/<居民编号>/goals、/agents/<居民编号>/memory、/residents、/topology、/catalog、/settings；有玩法时 /world/game_mode 包含玩法自己的后台状态。不要把摘要或最近记录当作完整数据。返回 json_text 正文，next_offset 非空表示尚未读完，用相同 path 和 next_offset 继续，不得把截断当作数据不存在。同轮 query_data 使用首次查询的快照，注意 snapshot_minutes，不要与实时工具混为同一时刻。
- 改世界：投放道具库里已有的道具、移除已投放的道具、张贴公告、通知村民、预约之后发生的事、改天气、调整道具的有限库存。
- 你可以读到村民的想法与私聊，但要把它当成判断剧情的原料，最终回答只总结局势、动机与对世界的影响；
  不要复述私聊或心声的原文，也不要把村民不知道的事当作他们知道的事实。
你不能：改写村民的目标、记忆、人设或心声，不能加人或删人，不能改村民的身体数值，不能重置世界，不能改时钟与速度。
要让村民改变主意，就通过通知、公告、道具和环境让他们自己看到、自己决定。
所有改动都只是提议，由世界内核校验后才会生效；道具只能投放道具库里已有的定义，不能凭空造新道具。
道具可用名字，但同名有歧义或名字不在库里时按定义编号来；地点用下面列出的地点编号。
每次最多调 4 个改变世界的工具。已经明确的操作直接做；玩家问“最近怎么样”“总结剧情”“谁在做什么”时，先用一个或多个只读工具看世界，再用正文回答。
调整库存使用 adjust_supply，id 填实例编号，action 填库存动作编号；只有一个库存动作可省略 action。支持设置、增加、扣减和减半；减半按执行时的数量向下取整。玩法管理的库存必须由玩法账本校验；普通道具不能超过初始容量。reason 是给玩家的后台操作原因，不会自动通知居民；要让居民知道事件原因，另用 announce 或公告。
聪明人小镇天灾必须调用 trigger_game_event，kind=survival10_disaster，event={kind:rats|flood|freeze|drought}。鼠患公仓随机损失3至10袋；洪涝公仓腐坏三分之一（向下取整），随机1至3名仍在场参赛者私粮清零，空仓者也可能被抽中。freeze和drought另填days（1至2），默认雪灾2天、旱灾1天，从执行时的下一比赛日起绝收，不回收已结算收成；正常劳动规则保留，受灾日不产粮。内核自动在真实公告栏发公告并通知所有在场参赛者；洪涝姓名及私粮损失仅告知本人，不向其他居民泄露。没有公告栏或对局结束时拒绝，不用通知或改天气假装已执行天灾。天灾不自动改天气、不损伤身体。预约天灾同样使用schedule，事件在预约执行时抽样、生效。
预约改天气、摆道具、贴公告或调整库存时，必须调用 schedule 并给 action 和 arguments；只填 text 的预约仅会发通知，不会把描述变成实际世界操作。预约以模拟时间为准，暂停时不计时；用 after_minutes 表示从现在起几分钟，或 at_minutes 表示世界的绝对分钟数。不要先执行再预约，也不要承诺未被工具确认的结果。
最终回答直接回应玩家，通常 1–3 句；只有玩家明确要求详细回顾时才展开。不要逐项汇报查看过程、复述清单、工具名、内部编号、私聊或心声原文。写操作的实际结果以工具回执为准，失败或撤回不能说成功。";

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
            if effect.ok && super::is_read(&effect.tool) {
                continue;
            }
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
        "max_tokens": 2200,
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
        if effect.ok && super::is_read(&effect.tool) {
            continue;
        }
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
            "name":"trigger_game_event",
            "description":"执行当前玩法支持的事件。聪明人小镇天灾自动公告、通知、修改权威账本并持久化；洪涝私粮损失保密，绝收从下一比赛日开始。",
            "parameters":{"type":"object","properties":{
                "kind":{"type":"string","enum":["survival10_disaster"]},
                "event":{"type":"object","properties":{
                    "kind":{"type":"string","enum":["rats","freeze","flood","drought"]},
                    "days":{"type":"integer","minimum":1,"maximum":2,"description":"仅freeze与drought使用；雪灾默认2天、旱灾默认1天"}
                },"required":["kind"],"additionalProperties":false}
            },"required":["kind","event"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"query_data",
            "description":"只读查询当前世界与后台游戏数据，不受摘要清单条数限制。空 path 查看目录；JSON Pointer 路径深入字段、居民编号或数组下标。数据包括完整身体、目标、长期记忆、背包、交谈、作品版本、地图道具、库存、世界观、导演预约及已启用玩法的后台状态。只访问明确的游戏数据，不读取文件、密钥或模型配置。每轮首次查询冻结快照，分页一致。",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string","description":"默认空字符串查看目录。例如 /world/actors 或 /agents/居民编号/goals。路径中的 ~ 和 / 转义为 ~0 和 ~1"},
                "offset":{"type":"integer","minimum":0,"description":"JSON 文本的字符偏移，默认 0；下一页填返回的 next_offset"},
                "limit":{"type":"integer","minimum":1,"maximum":12000,"description":"本页字符数，默认 6000。可优先深入更精确的路径减少输出"}
            },"additionalProperties":false}
        }}),
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
            "description":"按模拟分钟过滤本次进程保留的近期世界日志；与 watch 共用有限缓存，不是完整历史。更早的线索请查居民日记、记忆、交谈或玩法档案。",
            "parameters":{"type":"object","properties":{
                "minutes":{"type":"number","description":"回看多少模拟分钟；不给默认 12 小时"}
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
            "description":"马上告诉某个地点的在场村民一件事；不能用文字假装天气或道具已经改变。",
            "parameters":{"type":"object","properties":{
                "text":{"type":"string","description":"通知正文，1–800 字"},
                "locale":{"type":"string","description":"地点编号；不给就通知入口地点的在场村民"}
            },"required":["text"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"schedule",
            "description":"预约模拟时间到点后的操作。真实改天气、投放或移除道具、贴公告、调整库存须提供 action 和 arguments；只给 text 时仅发送通知。到点重新校验并执行，不依赖再次调用模型。暂停时不计时。",
            "parameters":{"type":"object","properties":{
                "text":{"type":"string","description":"事项摘要；未指定 action 时作为通知正文，1–800 字"},
                "after_minutes":{"type":"number","description":"多少模拟分钟后触发，1 到 10080"},
                "at_minutes":{"type":"number","description":"世界绝对分钟数；与 after_minutes 二选一，须在当前模拟时间之后"},
                "locale":{"type":"string","description":"通知地点编号；不给就通知入口地点。操作地点请写在 arguments 内"},
                "action":{"type":"string","enum":["announce","set_weather","place_prop","remove_prop","post_bulletin","adjust_supply","trigger_game_event"],"description":"到点执行的操作工具名"},
                "arguments":{"type":"object","description":"与 action 对应工具相同的参数。例如预约下雨：action=set_weather，arguments={mode:fixed,kind:rain}"}
            },"required":["text"],"additionalProperties":false}
        }}),
        json!({"type":"function","function":{
            "name":"adjust_supply",
            "description":"真实调整地图上道具的有限库存，支持玩法公共粮仓。减半以执行时库存为准并向下取整；不自动通知居民。修改与账本、场景、存档同步，失败整轮撤回。",
            "parameters":{"type":"object","properties":{
                "id":{"type":"string","description":"道具实例编号"},
                "action":{"type":"string","description":"有限库存动作编号；只有一个库存动作时可省略"},
                "operation":{"type":"string","enum":["set","add","subtract","halve"]},
                "quantity":{"type":"integer","minimum":0,"maximum":u32::MAX,"description":"set、add、subtract 必填；halve 不填"},
                "reason":{"type":"string","description":"后台审计原因，不会自动成为居民已知事实"}
            },"required":["id","operation","reason"],"additionalProperties":false}
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
