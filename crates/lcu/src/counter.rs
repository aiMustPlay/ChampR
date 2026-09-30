//! Local counter rune system: a deterministic, user-editable rule engine.
//!
//! Inputs: the lane matchup from the crawled OP.GG counters table (win rate
//! from the local champion's perspective) and champion profiles built from
//! Data Dragon (tags + attack/magic ratings). Outputs: a concrete rune page
//! adjustment plan (sub tree swap + defense shard) with human-readable
//! reasons. This deliberately does NOT depend on the LLM; the AI only sees
//! the result as prompt context.
//!
//! Coverage guarantee: every pressure tier (hard_lose/lose/even/win) has a
//! matching default rule, so any matchup with >= SAMPLE_MIN games yields a
//! plan. Unknown/undersampled matchups return None instead of guessing.
//!
//! De-duplication: `plan_for_matchup` drops a plan that is identical
//! (9 perks + both trees) to either of the champion's mainstream OP.GG pages
//! — recommending it would be noise. Differing plans always carry per-slot
//! diffs (`旧名→新名`) plus the rule's own rationale.
//!
//! Rules ship in `data/counter_runes.default.toml` (compiled in) and can be
//! overridden by `%APPDATA%/champr/counter-runes.toml`; a malformed override
//! falls back to the embedded defaults. Rune-legality checks (tree/row) use
//! the embedded `data/rune_lattice.json`. For the full design see
//! `analysis_and_design/counter-rune-system.md`.

use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;

use crate::builds::{BuildSection, Matchup, Rune};
use crate::web::ChampInfo;

const DEFAULT_RULES_TOML: &str = include_str!("../data/counter_runes.default.toml");
const RUNE_LATTICE_JSON: &str = include_str!("../data/rune_lattice.json");

/// Matchups with fewer games than this are too noisy to act on.
pub const SAMPLE_MIN: i64 = 25;

// ---------------------------------------------------------------------------
//  Pressure classification
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pressure {
    Unknown,
    /// win rate < 45%
    HardLose,
    /// 45%..48.5%
    Lose,
    /// 48.5%..51.5%
    Even,
    /// > 51.5%
    Win,
}

impl Pressure {
    pub fn from_win_rate(win_rate_pct: f64, play: i64) -> Self {
        if play < SAMPLE_MIN {
            return Self::Unknown;
        }
        if win_rate_pct < 45.0 {
            Self::HardLose
        } else if win_rate_pct < 48.5 {
            Self::Lose
        } else if win_rate_pct <= 51.5 {
            Self::Even
        } else {
            Self::Win
        }
    }

    pub fn from_matchup(m: &Matchup) -> Self {
        let pct = m
            .win_rate
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .unwrap_or(50.0);
        Self::from_win_rate(pct, m.play)
    }

    pub fn key(&self) -> &'static str {
        match self {
            Self::HardLose => "hard_lose",
            Self::Lose => "lose",
            Self::Even => "even",
            Self::Win => "win",
            Self::Unknown => "unknown",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::HardLose => "大劣势",
            Self::Lose => "劣势",
            Self::Even => "均势",
            Self::Win => "优势",
            Self::Unknown => "未知",
        }
    }
}

// ---------------------------------------------------------------------------
//  Champion profile (tags + damage typing)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamageClass {
    Ad,
    Ap,
    Mixed,
}

impl DamageClass {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Ad => "ad",
            Self::Ap => "ap",
            Self::Mixed => "mixed",
        }
    }

    pub fn label_zh(&self) -> &'static str {
        match self {
            Self::Ad => "物理",
            Self::Ap => "魔法",
            Self::Mixed => "混合",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChampionProfile {
    pub tags: Vec<String>,
    pub damage: DamageClass,
}

pub fn profile_of(info: &ChampInfo) -> ChampionProfile {
    let attack = info.info.attack as f64;
    let magic = info.info.magic as f64;
    // DDragon rates each dimension 0-10; a >=3 gap decides the dominant type.
    let damage = if magic >= attack + 3.0 {
        DamageClass::Ap
    } else if attack >= magic + 3.0 {
        DamageClass::Ad
    } else {
        DamageClass::Mixed
    };
    ChampionProfile {
        tags: info.tags.clone(),
        damage,
    }
}

// ---------------------------------------------------------------------------
//  Rune lattice (id -> tree/row/zh name), embedded at compile time
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct LatticeEntry {
    id: i64,
    tree: i64,
    row: u32,
    name: String,
}

#[derive(Debug, Clone)]
pub struct RuneLattice {
    entries: Vec<LatticeEntry>,
}

impl RuneLattice {
    pub fn builtin() -> &'static RuneLattice {
        static LATTICE: OnceLock<RuneLattice> = OnceLock::new();
        LATTICE.get_or_init(|| {
            // Some editors re-save JSON with a UTF-8 BOM; serde_json rejects it.
            let raw = RUNE_LATTICE_JSON.trim_start_matches('\u{feff}');
            let entries: Vec<LatticeEntry> = serde_json::from_str(raw).unwrap_or_else(|e| {
                log::warn!("rune lattice parse failed: {e}");
                Vec::new()
            });
            Self::from(entries)
        })
    }

    fn entry(&self, rune_id: i64) -> Option<&LatticeEntry> {
        self.entries.iter().find(|e| e.id == rune_id)
    }

    pub fn name_of(&self, rune_id: i64) -> Option<&str> {
        self.entry(rune_id).map(|e| e.name.as_str())
    }

    pub fn tree_of(&self, rune_id: i64) -> Option<i64> {
        self.entry(rune_id).map(|e| e.tree)
    }

    fn tree_name(&self, tree_id: i64) -> &'static str {
        match tree_id {
            8000 => "精密",
            8100 => "主宰",
            8200 => "巫术",
            8300 => "启迪",
            8400 => "坚决",
            _ => "未知系",
        }
    }

    /// The defense shard ids we may write; rows are client-fixed so we only
    /// check membership in this curated list.
    fn defense_shard_ok(rune_id: i64) -> bool {
        matches!(rune_id, 5001 | 5002 | 5003 | 5011 | 5013)
    }
}

impl From<Vec<LatticeEntry>> for RuneLattice {
    fn from(entries: Vec<LatticeEntry>) -> Self {
        Self { entries }
    }
}

// ---------------------------------------------------------------------------
//  Rule schema (TOML)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct ShardChoice {
    ad: i64,
    ap: i64,
    default: i64,
}

impl ShardChoice {
    fn pick(&self, damage: Option<DamageClass>) -> i64 {
        match damage {
            Some(DamageClass::Ad) => self.ad,
            Some(DamageClass::Ap) => self.ap,
            _ => self.default,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RuleSet {
    sub_tree: Option<i64>,
    #[serde(default)]
    sub_runes: Vec<i64>,
    #[serde(default)]
    shard: Option<ShardChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct Rule {
    name: String,
    #[serde(default)]
    priority: i64,
    #[serde(default)]
    pressure: Vec<String>,
    #[serde(default)]
    opponent_tags: Vec<String>,
    #[serde(default)]
    opponent_damage: Vec<String>,
    #[serde(default)]
    explain: String,
    #[serde(default)]
    set: RuleSet,
}

#[derive(Debug, Deserialize)]
struct RulesFile {
    #[serde(default)]
    rules: Vec<Rule>,
}

// ---------------------------------------------------------------------------
//  Engine
// ---------------------------------------------------------------------------

pub struct CounterSystem {
    rules: Vec<Rule>,
    /// Non-fatal problems found while validating rules (shown in logs/tests).
    pub warnings: Vec<String>,
    source_desc: String,
}

impl CounterSystem {
    pub fn load() -> &'static CounterSystem {
        static SYSTEM: OnceLock<CounterSystem> = OnceLock::new();
        SYSTEM.get_or_init(Self::load_fresh)
    }

    /// Load rules fresh: user override first, embedded default as fallback.
    pub fn load_fresh() -> Self {
        let user_path = dirs::config_dir()
            .unwrap_or_default()
            .join("champr")
            .join("counter-runes.toml");

        let (text, source_desc) = match std::fs::read_to_string(&user_path) {
            Ok(text) => (text, format!("{}", user_path.display())),
            Err(_) => (DEFAULT_RULES_TOML.to_string(), "内置默认规则".to_string()),
        };

        Self::from_rules_text(&text, source_desc)
    }

    /// Parse + validate a rules TOML document; on parse failure falls back to
    /// the embedded defaults so a broken user file never empties the system.
    fn from_rules_text(text: &str, source_desc: String) -> Self {
        let mut warnings = Vec::new();
        let rules: Vec<Rule> = match toml::from_str::<RulesFile>(text) {
            Ok(file) => file.rules,
            Err(e) => {
                warnings.push(format!(
                    "counter-runes 解析失败({source_desc}), 回退内置规则: {e}"
                ));
                toml::from_str::<RulesFile>(DEFAULT_RULES_TOML)
                    .map(|f| f.rules)
                    .unwrap_or_default()
            }
        };

        let mut system = Self {
            rules: Vec::new(),
            warnings,
            source_desc,
        };
        for rule in rules {
            if let Err(reason) = system.validate(&rule) {
                system
                    .warnings
                    .push(format!("counter-runes 规则[{}]已忽略: {reason}", rule.name));
                continue;
            }
            system.rules.push(rule);
        }
        system.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
        for warning in &system.warnings {
            log::warn!("counter-runes: {warning}");
        }
        system
    }

    fn validate(&self, rule: &Rule) -> Result<(), String> {
        let lattice = RuneLattice::builtin();
        for p in &rule.pressure {
            if !["hard_lose", "lose", "even", "win"].contains(&p.as_str()) {
                return Err(format!("未知 pressure: {p}"));
            }
        }
        for d in &rule.opponent_damage {
            if !["ad", "ap", "mixed"].contains(&d.as_str()) {
                return Err(format!("未知 opponent_damage: {d}"));
            }
        }
        if let Some(tree) = rule.set.sub_tree {
            if RuneLattice::builtin().tree_name(tree) == "未知系" {
                return Err(format!("未知符文树: {tree}"));
            }
            if rule.set.sub_runes.len() != 2 {
                return Err(format!(
                    "sub_runes 必须恰为2个(副系选两枚), 实际{}个",
                    rule.set.sub_runes.len()
                ));
            }
            let mut rows: HashSet<u32> = HashSet::new();
            for rune_id in &rule.set.sub_runes {
                let entry = lattice
                    .entry(*rune_id)
                    .ok_or_else(|| format!("未知符文id: {rune_id}"))?;
                if entry.tree != tree {
                    return Err(format!(
                        "符文{rune_id}({})不属于树{tree}",
                        entry.name
                    ));
                }
                if entry.row == 0 {
                    return Err(format!("符文{rune_id}({})是基石, 不能放副系槽", entry.name));
                }
                if !rows.insert(entry.row) {
                    return Err(format!("副系两枚符文在同一行: {}", entry.name));
                }
            }
        } else if !rule.set.sub_runes.is_empty() {
            return Err("设置了 sub_runes 但没设置 sub_tree".to_string());
        }
        if let Some(shard) = &rule.set.shard {
            for id in [shard.ad, shard.ap, shard.default] {
                if !RuneLattice::defense_shard_ok(id) {
                    return Err(format!("防御碎片id非法: {id}"));
                }
            }
        }
        Ok(())
    }

    /// Produce an adjusted rune page for the matchup. `base` is the champion's
    /// usual best page (e.g. the OP.GG most-popular page for the lane); the
    /// primary tree/keystone are kept, sub tree + defense shard may change.
    pub fn plan(
        &self,
        base: &Rune,
        opponent: Option<&ChampionProfile>,
        pressure: Pressure,
    ) -> Option<RunePlan> {
        if base.selected_perk_ids.len() < 9 {
            return None;
        }

        let opp_damage = opponent.map(|p| p.damage);
        let opp_tags: HashSet<&str> = opponent
            .map(|p| p.tags.iter().map(String::as_str).collect())
            .unwrap_or_default();

        let matched: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|r| {
                (r.pressure.is_empty()
                    || r.pressure.iter().any(|p| p.as_str() == pressure.key()))
                    && (r.opponent_tags.is_empty()
                        || r.opponent_tags
                            .iter()
                            .any(|t| opp_tags.contains(t.as_str())))
                    && (r.opponent_damage.is_empty()
                        || opp_damage
                            .map(|d| r.opponent_damage.iter().any(|x| x.as_str() == d.key()))
                            .unwrap_or(false))
            })
            .collect();
        if matched.is_empty() {
            return None;
        }

        let lattice = RuneLattice::builtin();
        let mut perks = base.selected_perk_ids.clone();
        let mut sub_tree = base.sub_style_id;
        let mut parts: Vec<String> = Vec::new();
        let mut reasons: Vec<String> = Vec::new();
        let mut sub_done = false;
        let mut shard_done = false;

        for rule in &matched {
            if !sub_done {
                if let (Some(tree), [r1, r2]) = (rule.set.sub_tree, rule.set.sub_runes.as_slice())
                {
                    if *r1 != perks[4] || *r2 != perks[5] || tree != sub_tree {
                        sub_tree = tree;
                        perks[4] = *r1;
                        perks[5] = *r2;
                        let names = format!(
                            "{}+{}",
                            lattice.name_of(*r1).unwrap_or("?"),
                            lattice.name_of(*r2).unwrap_or("?")
                        );
                        parts.push(format!("副系{}: {names}", lattice.tree_name(tree)));
                    }
                    sub_done = true;
                }
            }
            if !shard_done {
                if let Some(choice) = &rule.set.shard {
                    let shard = choice.pick(opp_damage);
                    if perks[8] != shard {
                        perks[8] = shard;
                        parts.push(match opp_damage {
                            Some(DamageClass::Ad) => "防御碎片: 护甲".to_string(),
                            Some(DamageClass::Ap) => "防御碎片: 魔抗".to_string(),
                            _ => "防御碎片: 成长生命".to_string(),
                        });
                    }
                    shard_done = true;
                }
            }
            if !rule.explain.is_empty() && reasons.len() < 2 {
                reasons.push(rule.explain.clone());
            }
        }

        if parts.is_empty() {
            return None;
        }

        let diffs: Vec<String> = perks
            .iter()
            .zip(base.selected_perk_ids.iter())
            .enumerate()
            .filter(|(_, (after, before))| after != before)
            .map(|(idx, (after, before))| {
                format!(
                    "{}: {}→{}",
                    slot_label(idx),
                    rune_name(*before),
                    rune_name(*after)
                )
            })
            .collect();

        Some(RunePlan {
            primary_style_id: base.primary_style_id,
            sub_style_id: sub_tree,
            selected_perk_ids: perks,
            line: parts.join(", "),
            reasons,
            diffs,
            page_name: "[ChampR] Counter符文".to_string(),
            pressure,
            opponent_damage: opp_damage,
            base: base.clone(),
        })
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn source(&self) -> &str {
        &self.source_desc
    }
}

// ---------------------------------------------------------------------------
//  Output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RunePlan {
    pub primary_style_id: i64,
    pub sub_style_id: i64,
    /// 9 perk ids in LCU order: keystone, 3x primary rows, 2x sub, 3x shards.
    pub selected_perk_ids: Vec<i64>,
    /// One-line summary of the applied changes, e.g. "副系坚决: 骸骨镀层+过度生长, 防御碎片: 护甲".
    pub line: String,
    /// Human-readable rule explanations behind the change (max 2).
    pub reasons: Vec<String>,
    /// Per-slot trade-offs vs the base page, e.g. "副系第1格: 神奇之鞋→骸骨镀层".
    pub diffs: Vec<String>,
    pub page_name: String,
    pub pressure: Pressure,
    pub opponent_damage: Option<DamageClass>,
    /// The base page the plan adjusted from (alias/position/metadata kept for apply).
    pub base: Rune,
}

impl RunePlan {
    /// The full page ready for `lcu_api::apply_rune`.
    pub fn to_rune_page(&self) -> Rune {
        let mut rune = self.base.clone();
        rune.name = self.page_name.clone();
        rune.primary_style_id = self.primary_style_id;
        rune.sub_style_id = self.sub_style_id;
        rune.selected_perk_ids = self.selected_perk_ids.clone();
        rune
    }
}

/// Chinese label for a Data Dragon champion tag.
pub fn tag_zh(tag: &str) -> &'static str {
    match tag {
        "Assassin" => "刺客",
        "Fighter" => "战士",
        "Mage" => "法师",
        "Marksman" => "射手",
        "Tank" => "坦克",
        "Support" => "辅助",
        _ => "未知",
    }
}

/// Rune display name: lattice first, then the stat-shard table (shards are
/// not part of runesReforged.json), finally a raw id fallback.
pub fn rune_name(rune_id: i64) -> String {
    if let Some(name) = RuneLattice::builtin().name_of(rune_id) {
        return name.to_string();
    }
    match rune_id {
        5005 => "攻速".to_string(),
        5007 => "技能急速".to_string(),
        5008 => "自适应之力".to_string(),
        5001 => "成长生命".to_string(),
        5002 => "护甲".to_string(),
        5003 => "魔抗".to_string(),
        5011 => "韧性及减速抗性".to_string(),
        5013 => "生命值成长".to_string(),
        other => format!("符文{other}"),
    }
}

/// Slot label for a diff line, given the selectedPerkIds index (0..=8).
fn slot_label(idx: usize) -> &'static str {
    match idx {
        0 => "基石",
        1 => "主系第1格",
        2 => "主系第2格",
        3 => "主系第3格",
        4 => "副系第1格",
        5 => "副系第2格",
        6 => "进攻碎片",
        7 => "灵活碎片",
        _ => "防御碎片",
    }
}

/// 只解析这局对位的压力档(给 war 心战等非符文消费方复用)。
/// 数据不足时返回 Pressure::Unknown。
pub fn pressure_of_matchup(
    local_champion_id: i64,
    local_position: &str,
    opponent_champion_id: i64,
    opponent_key: &str,
    sections_map: &std::collections::HashMap<i64, Vec<BuildSection>>,
) -> Pressure {
    let section = match sections_map
        .get(&local_champion_id)
        .and_then(|s| crate::advisor::best_section(s, local_position))
    {
        Some(s) => s,
        None => return Pressure::Unknown,
    };
    let Some(counters) = &section.counters else {
        return Pressure::Unknown;
    };
    counters
        .matchups
        .iter()
        .find(|m| {
            (m.champion_id > 0 && m.champion_id == opponent_champion_id)
                || (!opponent_key.is_empty() && m.champion_key == opponent_key)
        })
        .map(Pressure::from_matchup)
        .unwrap_or(Pressure::Unknown)
}

/// Shortcut: resolve the lane matchup for `opponent_champion_id` inside the
/// local champion's counters table, then produce a plan when actionable.
pub fn plan_for_matchup(
    base: &Rune,
    local_champion_id: i64,
    local_position: &str,
    opponent_champion_id: i64,
    opponent_key: &str,
    opponent_profile: Option<&ChampionProfile>,
    sections_map: &std::collections::HashMap<i64, Vec<BuildSection>>,
) -> Option<RunePlan> {
    let section = sections_map.get(&local_champion_id).and_then(|s| {
        crate::advisor::best_section(s, local_position)
    })?;
    let counters = section.counters.as_ref()?;
    let matchup = counters.matchups.iter().find(|m| {
        (m.champion_id > 0 && m.champion_id == opponent_champion_id)
            || (!opponent_key.is_empty() && m.champion_key == opponent_key)
    })?;
    let pressure = Pressure::from_matchup(matchup);
    if pressure == Pressure::Unknown {
        return None;
    }
    let plan = CounterSystem::load().plan(base, opponent_profile, pressure)?;
    // 与两页主流推荐完全相同就不另行推荐(避免噪音, 按用户要求)。
    let duplicates_mainstream = section.runes.iter().any(|rune| {
        rune.selected_perk_ids == plan.selected_perk_ids
            && rune.primary_style_id == plan.primary_style_id
            && rune.sub_style_id == plan.sub_style_id
    });
    if duplicates_mainstream {
        return None;
    }
    Some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::ChampInfoStats;

    fn profile(tags: &[&str], attack: i32, magic: i32) -> ChampionProfile {
        profile_of(&ChampInfo {
            info: ChampInfoStats {
                attack,
                defense: 0,
                magic,
                difficulty: 0,
            },
            tags: tags.iter().map(|s| s.to_string()).collect(),
            ..ChampInfo::default()
        })
    }

    fn base_rune() -> Rune {
        // Conqueror page with a generic utility settle sub tree.
        Rune {
            name: "base".to_string(),
            primary_style_id: 8000,
            sub_style_id: 8300,
            selected_perk_ids: vec![8010, 9111, 9105, 8299, 8345, 8347, 5008, 5008, 5001],
            ..Default::default()
        }
    }

    #[test]
    fn lattice_covers_default_rules() {
        let system = CounterSystem::load_fresh();
        // The bundled TOML must be parseable, fully valid and non-empty.
        assert!(system.rule_count() >= 4, "warnings: {:?}", system.warnings);
        assert!(system.warnings.is_empty(), "warnings: {:?}", system.warnings);
    }

    #[test]
    fn assassin_pressure_loses_swap_to_resolve() {
        let system = CounterSystem::load_fresh();
        let assassin = profile(&["Assassin"], 9, 2);
        let plan = system.plan(&base_rune(), Some(&assassin), Pressure::Lose).unwrap();
        assert_eq!(plan.sub_style_id, 8400);
        assert_eq!(plan.selected_perk_ids[4], 8473, "骸骨镀层");
        assert_eq!(plan.selected_perk_ids[5], 8451, "过度生长");
        assert_eq!(plan.selected_perk_ids[8], 5002, "护甲 vs AD");
        assert!(plan.line.contains("骸骨镀层"), "{}", plan.line);
        assert!(!plan.reasons.is_empty());
        // Keystone/primary tiles untouched.
        assert_eq!(&plan.selected_perk_ids[0..4], &[8010, 9111, 9105, 8299]);
    }

    #[test]
    fn mage_pressure_prefers_second_wind() {
        let system = CounterSystem::load_fresh();
        let mage = profile(&["Mage"], 2, 9);
        let plan = system.plan(&base_rune(), Some(&mage), Pressure::HardLose).unwrap();
        assert_eq!(plan.selected_perk_ids[4], 8444, "复苏之风");
        assert_eq!(plan.selected_perk_ids[8], 5003, "魔抗 vs AP");
    }

    #[test]
    fn even_matchup_tunes_shard_only() {
        let system = CounterSystem::load_fresh();
        let fighter = profile(&["Fighter"], 8, 1); // AD
        let plan = system.plan(&base_rune(), Some(&fighter), Pressure::Even).unwrap();
        // 均势兜底规则: 副系保持主流, 只防御碎片随伤害类型
        assert_eq!(plan.sub_style_id, 8300);
        assert_eq!(plan.selected_perk_ids[8], 5002);
        assert_eq!(plan.diffs, vec!["防御碎片: 成长生命→护甲".to_string()]);
        // 无样本/未知压力仍不推荐(信息不足不乱动)
        assert!(system.plan(&base_rune(), Some(&fighter), Pressure::Unknown).is_none());
    }

    #[test]
    fn plan_diffs_describe_each_slot_tradeoff() {
        let system = CounterSystem::load_fresh();
        let assassin = profile(&["Assassin"], 9, 2);
        let plan = system.plan(&base_rune(), Some(&assassin), Pressure::Lose).unwrap();
        assert!(plan.diffs.contains(&"副系第1格: 饼干配送→骸骨镀层".to_string()), "{:?}", plan.diffs);
        assert!(plan.diffs.contains(&"副系第2格: 星界洞悉→过度生长".to_string()), "{:?}", plan.diffs);
        assert!(plan.diffs.contains(&"防御碎片: 成长生命→护甲".to_string()), "{:?}", plan.diffs);
        assert_eq!(plan.diffs.len(), 3);
    }

    #[test]
    fn win_keeps_sub_and_tunes_shard() {
        let system = CounterSystem::load_fresh();
        let adc = profile(&["Marksman"], 9, 1);
        let plan = system.plan(&base_rune(), Some(&adc), Pressure::Win).unwrap();
        // Sub tree untouched (8300 stays), only the defense shard changes.
        assert_eq!(plan.sub_style_id, 8300);
        assert_eq!(plan.selected_perk_ids[8], 5002);
        assert!(!plan.line.contains("副系"), "{}", plan.line);
        // ident perks remain for sub slots
        assert_eq!(plan.selected_perk_ids[4], 8345);
    }

    #[test]
    fn invalid_rule_is_dropped_with_warning() {
        let broken = Rule {
            name: "bad".to_string(),
            priority: 1,
            pressure: vec!["lose".to_string()],
            opponent_tags: vec![],
            opponent_damage: vec![],
            explain: String::new(),
            set: RuleSet {
                sub_tree: Some(8400),
                // 骸骨镀层 + 复苏之风 are both resolve row 2: illegal side-by-side.
                sub_runes: vec![8473, 8444],
                shard: None,
            },
        };
        let system = CounterSystem {
            rules: Vec::new(),
            warnings: Vec::new(),
            source_desc: "test".to_string(),
        };
        assert!(system.validate(&broken).is_err());
    }

    #[test]
    fn override_text_replaces_embedded_rules() {
        let custom = r##"
[[rules]]
name = "自定义规则"
priority = 1
pressure = ["lose"]
explain = "测试"
[rules.set]
shard = { ad = 5002, ap = 5003, default = 5001 }
"##;
        let system = CounterSystem::from_rules_text(custom, "test".to_string());
        assert_eq!(system.rule_count(), 1, "warnings: {:?}", system.warnings);
        // Broken TOML: falls back to the bundled defaults instead of an empty set.
        let broken = CounterSystem::from_rules_text("[[rules] not toml", "test".to_string());
        assert!(broken.rule_count() >= 4);
        assert!(!broken.warnings.is_empty());
    }

    #[test]
    fn pressure_thresholds() {
        assert_eq!(Pressure::from_win_rate(44.9, 100), Pressure::HardLose);
        assert_eq!(Pressure::from_win_rate(46.0, 100), Pressure::Lose);
        assert_eq!(Pressure::from_win_rate(50.0, 100), Pressure::Even);
        assert_eq!(Pressure::from_win_rate(52.0, 100), Pressure::Win);
        assert_eq!(Pressure::from_win_rate(39.0, SAMPLE_MIN - 1), Pressure::Unknown);
    }

    #[test]
    fn damage_classification_via_info() {
        assert_eq!(profile(&[], 9, 2).damage, DamageClass::Ad);
        assert_eq!(profile(&[], 1, 9).damage, DamageClass::Ap);
        assert_eq!(profile(&[], 9, 8).damage, DamageClass::Mixed);
    }
}
