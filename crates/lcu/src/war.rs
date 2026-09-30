//! 兵法心战系统(War of Psychology)。
//!
//! 把对位拆成两层并用三十六计语言固化:
//! - **战略**(固定思路): 按英雄职业分型给出, 每局不变 —— 这类英雄凭何赢一局
//! - **战术**(实践方式): 按对位压力档给出, 每局校准 —— 眼下这一局怎么打
//!
//! 与 counter.rs / tips.rs 的分工:
//!   counter.rs —— 符文怎么换(武器)
//!   tips.rs    —— 对面在想什么(知彼)
//!   war.rs     —— 双方的战略定力 + 我的战术动作(用兵)
//!
//! 三十六计正名表是全量校验表: 覆盖卡里拼错的 stratagem 会被丢弃并写警告。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::counter::Pressure;
use crate::web::ChampInfo;

// ---------------------------------------------------------------------------
//  三十六计全表(unique legal values)
// ---------------------------------------------------------------------------

pub const STRATAGEM_NAMES: [&str; 36] = [
    "瞒天过海", "围魏救赵", "借刀杀人", "以逸待劳", "趁火打劫", "声东击西",
    "无中生有", "暗渡陈仓", "隔岸观火", "笑里藏刀", "李代桃僵", "顺手牵羊",
    "打草惊蛇", "借尸还魂", "调虎离山", "欲擒故纵", "抛砖引玉", "擒贼擒王",
    "釜底抽薪", "混水摸鱼", "金蝉脱壳", "关门捉贼", "远交近攻", "假道伐虢",
    "偷梁换柱", "指桑骂槐", "假痴不癫", "上屋抽梯", "树上开花", "反客为主",
    "美人计", "空城计", "反间计", "苦肉计", "连环计", "走为上计",
];

// ---------------------------------------------------------------------------
//  职业分型(Archetype)与战略
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Archetype {
    Assassin,
    Mage,
    Tank,
    Marksman,
    Support,
    Fighter,
    Unknown,
}

impl Archetype {
    pub fn zh(&self) -> &'static str {
        match self {
            Self::Assassin => "刺客",
            Self::Mage => "法师",
            Self::Tank => "坦克",
            Self::Marksman => "射手",
            Self::Support => "辅助",
            Self::Fighter => "战士",
            Self::Unknown => "放逐者",
        }
    }

    /// LoL tags 中挑第一个命中的主定位; 多标签同现按文本顺序取第一个命中项。
    pub fn from_champion(info: &ChampInfo) -> Self {
        for tag in &info.tags {
            match tag.as_str() {
                "Assassin" => return Self::Assassin,
                "Mage" => return Self::Mage,
                "Tank" => return Self::Tank,
                "Marksman" => return Self::Marksman,
                "Support" => return Self::Support,
                "Fighter" => return Self::Fighter,
                _ => {}
            }
        }
        Self::Unknown
    }
}

/// 固定战略 = 一句话 doctrine + 两道主计; 主计来自三十六计正名表。
pub struct Doctrine {
    pub doctrine: &'static str,
    pub mains: [(&'static str, &'static str); 2],
}

pub fn doctrine_of(archetype: Archetype) -> Doctrine {
    match archetype {
        Archetype::Assassin => Doctrine {
            doctrine: "一击决胜, 不恋战",
            mains: [
                ("瞒天过海", "用日常走位麻痹对手, 把起手的实勇隐藏到出手前一瞬"),
                ("擒贼擒王", "只盯输出核心、一击收割中枢; 不打无谓之团"),
            ],
        },
        Archetype::Mage => Doctrine {
            doctrine: "阵地控场, 消耗即战果",
            mains: [
                ("无中生有", "poke 持续造压, 让其情绪先于血量失守, 再挑他先交技能的那个出手"),
                ("隔岸观火", "让互搏先发生, 技能留给那个先沉不住气的"),
            ],
        },
        Archetype::Tank => Doctrine {
            doctrine: "以身为墙, 打开一道口子就算赢",
            mains: [
                ("李代桃僵", "用自己的身位换队友的输出空间——被集火是你的本职"),
                ("关门捉贼", "一旦抓人即闭环: 控完再接后手, 不留逃生口"),
            ],
        },
        Archetype::Marksman => Doctrine {
            doctrine: "苟线为赢, 后期掌整部局",
            mains: [
                ("假痴不癫", "前期示怂不是低头, 是收合成——示弱本身就是武器的蓄积"),
                ("趁火打劫", "敌方强开失败的那半秒, 立即反向收割塔与人头"),
            ],
        },
        Archetype::Support => Doctrine {
            doctrine: "让 AD 发光, 以节奏牵动战局",
            mains: [
                ("欲擒故纵", "兵线略放一点诱敌深探, 起抓时机排在我方打野到位之后"),
                ("顺手牵羊", "敌方撤退的间隙, 顺手拿眼位、拿河蟹、拿节奏"),
            ],
        },
        Archetype::Fighter => Doctrine {
            doctrine: "分带拉扯, 以侧线牵扯按下敌手的团之心",
            mains: [
                ("围魏救赵", "你接不到团时——带边线, 敌方的团自然不成"),
                ("打草惊蛇", "带线途中顺手惊动草丛探反应, 拿到的信息本身就是胜场份"),
            ],
        },
        Archetype::Unknown => Doctrine {
            doctrine: "静观为先, 后手定夺",
            mains: [
                ("声东击西", "敌未动我先作势——假动作本身就是测敌器"),
                ("以逸待劳", "先手让给对面出, 技能留后手接管局势"),
            ],
        },
    }
}

// ---------------------------------------------------------------------------
//  战术(对位压力档驱动)
// ---------------------------------------------------------------------------

/// 身弱/均势/优势的三至两条战术动作, 全部是三十六计实招。
pub fn tactics_for(pressure: &Pressure) -> Vec<(&'static str, &'static str)> {
    match pressure {
        Pressure::HardLose | Pressure::Lose => vec![
            ("走为上", "让兵不让命——命在塔在, 才有翻盘票; 技能只交防守不交进攻"),
            ("假痴不癫", "示现不敌的弱点诱他交关键技能——他交完那一刻就是反击起点"),
        ],
        Pressure::Even => vec![
            ("打草惊蛇", "先探草丛与眼位再下决心——把他先手吓出来即回落"),
            ("声东击西", "佯装换血引他走位, 真目标是兵线经验与等级差"),
        ],
        Pressure::Win => vec![
            ("趁火打劫", "对面任何一次走位踩空, 立即过线加压——拿塔与龙不等他回神"),
            ("擒贼擒王", "优势期每次小规模交火以击杀为唯一目标, 不给他喘息节奏"),
        ],
        Pressure::Unknown => vec![
            ("假痴不癫", "信息不足先示弱——等他出手, 暴露惯用招式的从来不是先动作的人"),
            ("以逸待劳", "技能全留后手; 对面不接先手, 局势就自己往你这边走"),
        ],
    }
}

// ---------------------------------------------------------------------------
//  覆盖卡存储(每英雄一级, 高频英雄专有连招/心理)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WarDoc {
    #[serde(rename = "unit", default)]
    units: Vec<WarUnit>,
}

/// 每个英雄一张心战卡(只对影响深刻的招牌英雄; 无卡的由 Archetype 补位)。
#[derive(Debug, Clone, Deserialize)]
pub struct WarUnit {
    pub id: String,
    pub stratagem: String,
    #[serde(default)]
    pub combo: String,
    #[serde(default)]
    pub psych: String,
    #[serde(default)]
    pub usage: String,
    #[serde(default)]
    pub reason: String,
}

/// 系统主构件: 装载 + 校验 + 组装心战卡。
pub struct WarSystem {
    units: HashMap<String, WarUnit>,
    pub warnings: Vec<String>,
}

impl WarSystem {
    fn from_units(units: Vec<WarUnit>) -> Self {
        let mut warnings = Vec::new();
        let mut map = HashMap::new();
        for unit in units {
            let id = unit.id.trim().to_lowercase();
            if id.is_empty() {
                warnings.push("war-strategy 忽略: unit.id 为空".to_string());
                continue;
            }
            if !STRATAGEM_NAMES.contains(&unit.stratagem.as_str()) {
                warnings.push(format!(
                    "war-strategy 忽略 {id}: stratagem 不在三十六计正名表"
                ));
                continue;
            }
            map.insert(id, unit);
        }
        Self { units: map, warnings }
    }

    fn from_rules_text(text: &str) -> Self {
        match toml::from_str::<WarDoc>(text) {
            Ok(doc) => Self::from_units(doc.units),
            Err(err) => {
                let mut sys = Self::default_units();
                sys.warnings
                    .push(format!("war-strategy TOML 解析失败, 回退内置: {err}"));
                sys
            }
        }
    }

    fn default_units() -> Self {
        let doc: WarDoc =
            toml::from_str(include_str!("../data/war_strategy.default.toml"))
                .expect("内置 war_strategy.default.toml 必须可解析");
        Self::from_units(doc.units)
    }

    /// 进程级单例: 读 %APPDATA%\champr\war-strategy.toml; 失败/缺失回退内置。
    pub fn load() -> &'static Self {
        static INSTANCE: OnceLock<WarSystem> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let path = dirs::config_dir()
                .map(|d| d.join("champr").join("war-strategy.toml")).unwrap_or_default();
            let sys = match std::fs::read_to_string(path) {
                Ok(text) => Self::from_rules_text(&text),
                Err(_) => Self::default_units(),
            };
            for warning in &sys.warnings {
                log::warn!("war-strategy: {warning}");
            }
            sys
        })
    }

    /// 测试入口: 从任意文本构造。
    pub fn from_rules_text_for_test(text: &str) -> Self {
        Self::from_rules_text(text)
    }

    pub fn unit_for(&self, info: &ChampInfo) -> Option<&WarUnit> {
        let id = info.id.to_lowercase();
        self.units.get(&id)
    }

    pub fn war_card(&self, local: &ChampInfo, opp: &ChampInfo, pressure: &Pressure) -> WarCard {
        let render_side = |side_zh: &str, info: &ChampInfo| {
            let archetype = Archetype::from_champion(info);
            let doctrine = doctrine_of(archetype);
            let (main_name, _) = doctrine.mains[0];
            if let Some(unit) = self.unit_for(info) {
                format!(
                    "战略({side_zh} {}·{}): {} · 主计: {} — {}",
                    info.name, archetype.zh(), doctrine.doctrine, unit.stratagem, unit.usage
                )
            } else {
                format!(
                    "战略({side_zh} {}·{}): {} · 主计: {} — {}",
                    info.name, archetype.zh(), doctrine.doctrine, main_name, doctrine.mains[0].1
                )
            }
        };

        let opp_unit = self.unit_for(opp);
        WarCard {
            own_strategy: render_side("我方", local),
            opp_strategy: render_side("敌方", opp),
            opp_psych: opp_unit.and_then(|u| {
                (!u.psych.is_empty()).then(|| format!("对面心理: {}", u.psych))
            }),
            opp_combo: opp_unit.and_then(|u| {
                (!u.combo.is_empty()).then(|| format!("连招签名: {}", u.combo))
            }),
            tactics: tactics_for(pressure)
                .into_iter()
                .map(|(sg, action)| format!("战术({})·{sg} — {action}", pressure.label_zh()))
                .collect(),
        }
    }
}

/// 一张心战卡: 战略(我方 + 敌方) + 敌方心理/连招(有卡时) + 战术两条。
#[derive(Debug, Clone)]
pub struct WarCard {
    pub own_strategy: String,
    pub opp_strategy: String,
    pub opp_psych: Option<String>,
    pub opp_combo: Option<String>,
    pub tactics: Vec<String>,
}

// ---------------------------------------------------------------------------
//  渲染
// ---------------------------------------------------------------------------

/// UI 渲染: (标题, 正文)。标题承担卡片名, 正文逐条罗列。
pub fn render_ui(card: &WarCard) -> (String, String) {
    let mut body = vec![card.own_strategy.clone(), card.opp_strategy.clone()];
    if let Some(psych) = &card.opp_psych {
        body.push(psych.clone());
    }
    if let Some(combo) = &card.opp_combo {
        body.push(combo.clone());
    }
    body.extend(card.tactics.iter().cloned());
    ("兵法 · 知之与用之".to_string(), body.join("\n"))
}

/// prompt 渲染: 三行以内, 供 LLM 把心战舶背景用起来。
pub fn render_for_prompt(card: &WarCard) -> String {
    let mut text = card.own_strategy.clone();
    text.push('\n');
    text.push_str(&card.opp_strategy);
    if let Some(psych) = &card.opp_psych {
        text.push('\n');
        text.push_str(psych);
    }
    text.push('\n');
    text.push_str(card.tactics.first().map(String::as_str).unwrap_or_default());
    if let Some(second) = card.tactics.get(1) {
        text.push('\n');
        text.push_str(second);
    }
    text
}

// ---------------------------------------------------------------------------
//  测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::ChampInfoStats;

    fn champ(id: &str, tags: &[&str]) -> ChampInfo {
        ChampInfo {
            version: String::new(),
            id: id.to_string(),
            key: "0".to_string(),
            name: id.to_string(),
            title: String::new(),
            info: ChampInfoStats::default(),
            image: crate::web::Image::default(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn stratagem_table_has_36_unique_entries() {
        assert_eq!(STRATAGEM_NAMES.len(), 36);
        let mut seen = std::collections::HashSet::new();
        for name in STRATAGEM_NAMES {
            assert!(seen.insert(name.to_string()), "三十六计表重复: {name}");
        }
    }

    #[test]
    fn all_doctrine_mains_are_legal_stratagems() {
        let all = [
            Archetype::Assassin,
            Archetype::Mage,
            Archetype::Tank,
            Archetype::Marksman,
            Archetype::Support,
            Archetype::Fighter,
            Archetype::Unknown,
        ];
        for archetype in all {
            for (name, _) in doctrine_of(archetype).mains {
                assert!(
                    STRATAGEM_NAMES.contains(&name),
                    "套用之计名称必须在三十六计表中: {name}"
                );
            }
        }
    }

    #[test]
    fn tactics_press_pressure_into_action() {
        let lose = tactics_for(&Pressure::HardLose);
        assert_eq!(lose[0].0, "走为上");
        assert!(lose[0].1.contains("命"), "硬劣势战术应念叨保命: {}", lose[0].1);

        let even = tactics_for(&Pressure::Even);
        assert!(even.iter().any(|(n, _)| *n == "声东击西"));

        let win = tactics_for(&Pressure::Win);
        assert!(win.iter().any(|(n, _)| *n == "擒贼擒王"));
    }

    #[test]
    fn archetype_falls_back_to_unknown() {
        let monkey = champ("monkeyking", &[]);
        assert_eq!(Archetype::from_champion(&monkey), Archetype::Unknown);
    }

    #[test]
    fn default_units_parse_and_hold_fields() {
        let system = WarSystem::default_units();
        assert!(system.units.len() >= 10, "内置英雄卡不应少于 10 张");
        for (id, unit) in &system.units {
            assert_eq!(id, &unit.id.to_lowercase());
            assert!(!unit.psych.is_empty(), "{id} 需要填写心理特点");
            assert!(!unit.combo.is_empty(), "{id} 需要填写连招签名");
            assert!(!unit.usage.is_empty(), "{id} 需要填写实战用法");
            assert!(!unit.reason.is_empty(), "{id} 需要填写收录理由");
        }
    }

    #[test]
    fn bad_stratagem_name_is_dropped_with_warning() {
        let sys = WarSystem::from_rules_text(
            r#"[[unit]]
id = "foo"
stratagem = "不存在的计"
"#,
        );
        assert!(sys.units.is_empty());
        assert!(sys.warnings.iter().any(|w| w.contains("三十六计正名表")));
    }

    #[test]
    fn render_ui_keeps_header_budget() {
        let sys = WarSystem::default_units();
        let own = champ("zed", &["Assassin"]);
        let opp = champ("yasuo", &["Fighter", "Assassin"]);
        let card = sys.war_card(&own, &opp, &Pressure::HardLose);
        let (header, body) = render_ui(&card);
        assert_eq!(header, "兵法 · 知之与用之");
        let lines = body.lines().count();
        assert!(
            lines <= 2 + 2 + 3,
            "默认卡正文应不超过 7 行(我方战略×2 + 敌方+计 2 + 战术 2-3): {body}"
        );
        assert!(card.opp_psych.is_some(), "敌方招牌卡必须带心理分析");
        assert!(card.opp_combo.is_some(), "敌方招牌卡必须带连招签名");
    }

    #[test]
    fn prompt_render_stays_within_line_budget() {
        let sys = WarSystem::default_units();
        let own = champ("zed", &["Assassin"]);
        let opp = champ("yasuo", &["Assassin"]);
        let card = sys.war_card(&own, &opp, &Pressure::Unknown);
        let text = render_for_prompt(&card);
        assert!(
            text.lines().count() <= 5,
            "prompt 版心战应在 5 行内:\n{text}"
        );
        assert!(text.contains("战术"), "提示语应含战术行");
    }

    #[test]
    fn unknown_opponent_uses_default_archetype_line() {
        let sys = WarSystem::default_units();
        let own = champ("zed", &["Assassin"]);
        // 合成 id 保证永远无卡(全英雄真卡已覆盖, 不能用真实 id 测无话回事态)
        let opp = champ("synthetic-faker", &["Mage"]);
        let card = sys.war_card(&own, &opp, &Pressure::Win);
        assert!(card.opp_strategy.contains("法师"));
        assert!(card.opp_psych.is_none(), "无卡英雄不应摊心理");
    }
}
