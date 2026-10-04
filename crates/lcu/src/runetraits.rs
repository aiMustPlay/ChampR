//! 符文特性词典 + 扬长避短建议(确定性, 不走 LLM)。
//!
//! 用户 2026-10-04 的诉求: 选完符文后, 用一句到几句话告诉我
//! "我这套符文是什么路数 / 对面是什么路数 / 该怎么扬长避短"。
//!
//! 设计取舍:
//! - 只做 id → 特性 → 建议的确定性映射: 选人/开局瞬间就要能出结果, 不等模型。
//! - 对手符文在对局内由 Live Client Data 提供(基石 + 主/副系; 完整六格只有自己可见),
//!   所以建议按"风格配对"给, 而不是逐格对比。
//! - 文案控制在 1-3 句, 且必须是能立刻执行的指令(距离/换血/等冷却)。

/// 符文系别 id → 特性
pub fn tree_trait(tree_id: i64) -> &'static str {
    match tree_id {
        8000 => "持续输出(平A/叠层)",
        8100 => "爆发与游走",
        8200 => "技能消耗",
        8300 => "耐久与坦克",
        8400 => "功能与经济",
        _ => "未知系别",
    }
}

/// 基石 id → 特性(带触发条件/冷却, 因为这些才决定"什么时候能上")
pub fn keystone_trait(id: i64) -> &'static str {
    match id {
        8005 => "强攻: 三次平A后增伤",
        8008 => "致命节奏: 叠攻速打持续",
        8021 => "迅捷步法: 平A续航赖线",
        8010 => "征服者: 叠层后持续作战",
        8112 => "电刑: 三连击爆发",
        8124 => "掠食者: 提速游走",
        8128 => "黑暗收割: 残血收割",
        9923 => "丛刃: 起手爆发",
        8214 => "召唤艾黎: 持续消耗并给盾",
        8229 => "奥术彗星: 远程技能消耗",
        8230 => "相位猛冲: 连招后拉开",
        8437 => "不灭之握: 换血续航",
        8439 => "余震: 控制后双抗爆发(约15s冷却)",
        8465 => "守护者: 保护队友",
        8351 => "冰川增幅: 减速控制",
        8360 => "启封的秘籍: 换召唤师技能",
        8369 => "先攻: 抢经济",
        _ => "未知基石",
    }
}

/// 风格标签(用于配对出建议)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// 远程/技能消耗
    Poke,
    /// 一套爆发
    Burst,
    /// 持续作战/续航
    Sustain,
    /// 耐久/坦克
    Tank,
    /// 功能/经济
    Utility,
    Unknown,
}

pub fn style_label(s: Style) -> &'static str {
    match s {
        Style::Poke => "消耗型",
        Style::Burst => "爆发型",
        Style::Sustain => "续航型",
        Style::Tank => "耐久型",
        Style::Utility => "功能型",
        Style::Unknown => "常规型",
    }
}

/// 基石优先, 系别兜底
pub fn style_of(keystone_id: i64, primary_tree_id: i64) -> Style {
    match keystone_id {
        8229 | 8214 => Style::Poke,
        8112 | 8128 | 9923 => Style::Burst,
        8005 | 8008 | 8010 | 8021 | 8437 => Style::Sustain,
        8439 | 8465 => Style::Tank,
        8351 | 8360 | 8369 => Style::Utility,
        _ => match primary_tree_id {
            8200 => Style::Poke,
            8100 => Style::Burst,
            8000 => Style::Sustain,
            8300 => Style::Tank,
            8400 => Style::Utility,
            _ => Style::Unknown,
        },
    }
}

/// 双方风格 → 1-3 句可执行提醒(扬长避短)
pub fn advice(mine: Style, theirs: Style) -> Vec<&'static str> {
    use Style::*;
    match (mine, theirs) {
        (Poke, Tank) | (Poke, Sustain) => vec![
            "你是消耗型, 他是耐打型: 别贴脸, 用射程和技能磨血。",
            "等他防御类符文(余震/不灭/骸骨)进冷却再上。",
        ],
        (Poke, Burst) => vec![
            "你是消耗型, 他是爆发型: 保持距离, 别让他一套起手命中。",
            "他技能交空就是你压制的窗口, 用平A加技能白嫖。",
        ],
        (Poke, Poke) => vec![
            "双方都是消耗型: 比谁先空技能, 站小兵侧面减少被消耗。",
        ],
        (Burst, Poke) | (Burst, Utility) => vec![
            "你是爆发型, 他是消耗型: 别被磨, 攒好一套直接上。",
            "等他关键技能交掉再冲; 一波打不死就撤, 别拉锯。",
        ],
        (Burst, Tank) | (Burst, Sustain) => vec![
            "你是爆发型, 他是耐打型: 别拉长单挑, 打完一套就走。",
            "把伤害留给他的队友, 或等他血量过半再收。",
        ],
        (Burst, Burst) => vec![
            "双方都是爆发型: 谁先手谁赢, 视野与走位优先于换血。",
        ],
        (Sustain, Burst) => vec![
            "你是续航型, 他是爆发型: 吃他一套不亏, 用回复和换血磨他。",
            "他爆发有冷却, 撑过去就是你的回合。",
        ],
        (Sustain, Poke) => vec![
            "你是续航型, 他是消耗型: 别站着挨消耗, 逼他近身换血。",
        ],
        (Sustain, Tank) | (Sustain, Sustain) => vec![
            "双方都耐打: 别互相磨, 抓炮车线与支援机会, 团战才是胜负手。",
        ],
        (Tank, Burst) | (Tank, Poke) | (Tank, Sustain) => vec![
            "你是耐久型, 他偏输出/续航: 顶住第一波, 用控制链把他留在你队友面前。",
        ],
        (Tank, Tank) => vec![
            "双方都耐打: 换血意义不大, 优先争夺资源与做视野。",
        ],
        (Utility, _) | (_, Utility) => vec![
            "对方带功能性符文: 他靠机制而非伤害, 注意他的减速/护盾节奏, 别被骗技能。",
        ],
        (Unknown, _) | (_, Unknown) => vec![
            "符文信息不全: 先按英雄对位常识打, 注意对方关键符文触发时机。",
        ],
    }
}

/// 一页符文的可读描述(基石 / 主副系 / 属性碎片), 供 UI 与 prompt 复用
pub fn describe_page(
    keystone_id: i64,
    keystone_zh: &str,
    primary_tree_id: i64,
    primary_tree_zh: &str,
    _secondary_tree_id: i64,
    secondary_tree_zh: &str,
    stat_zh: &[String],
) -> String {
    let mut text = format!(
        "{keystone_zh}({}) · {primary_tree_zh}/{secondary_tree_zh} · 属性: {}",
        keystone_trait(keystone_id),
        if stat_zh.is_empty() {
            "未知".to_string()
        } else {
            stat_zh.join("/")
        }
    );
    if primary_tree_id != 0 {
        text.push_str(&format!(" · 主系特性: {}", tree_trait(primary_tree_id)));
    }
    text
}
