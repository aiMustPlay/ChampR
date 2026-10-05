use anyhow::{bail, Context};
use serde_json::Value;
use std::collections::HashMap;

use crate::builds::BuildSection;
use crate::match_context::{
    opgg_position_aliases, position_label, ChampSelectMember, ChampSelectSnapshot, LivePlayer,
    LiveSnapshot, TeamObjectives,
};
use crate::tips::PlaystyleAtlas;
use crate::web::{ChampionsMap, StaticNames};

pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a League of Legends in-game coach.
The context you receive may include:
- Champion select data: bans, both teams' champions, lanes and solo-queue ranks, the
  local player's current rune page, the lane opponent, and OP.GG stats (tier, win
  rate, recommended runes, skill order and item builds).
- Live game data: every player's champion, level, KDA, CS, keystone and rune trees,
  summoner spells, items and death state, the local player's gold, ability levels,
  champion stats and full rune page, team kill scores, objective control (dragons,
  Void Grubs, Rift Herald, Baron, towers, inhibitors) and recent kills.
Always base your reply on this data, and answer in this order:
1. Lane matchup analysis for the local player against the lane opponent.
2. Rune advice: compare the local player's current or expected runes with the
   recommended ones, and point out what should change for this matchup.
3. Itemization and how-to-play advice that fits the current game state, objective
   difference and team compositions.
All replies must be in Simplified Chinese only, regardless of the prompt language.
Keep it under 350 characters, no unrelated fluff, mention concrete numbers from the
data instead of generic statements, and when some data is missing, say so briefly.
Your final reply must contain only plain text, commas, and periods. Do not use any
other punctuation, markdown, bullet points, question marks, exclamation marks,
colons, parentheses, or special symbols."#;

// ---------------------------------------------------------------------------
//  Name resolution helpers
// ---------------------------------------------------------------------------

fn champ_zh_by_id(champion_id: i64, champions: &ChampionsMap, names: &StaticNames) -> Option<String> {
    if champion_id <= 0 {
        return None;
    }
    let key = champion_id.to_string();
    let champ = champions.values().find(|c| c.key == key)?;
    Some(
        names
            .champion(&champ.key)
            .map(str::to_string)
            .unwrap_or_else(|| champ.name.clone()),
    )
}

/// allPlayers only carries the English display name; resolve it through Data Dragon
/// (key -> zh name). Falls back to the raw display name.
fn champ_zh_by_display(display: &str, champions: &ChampionsMap, names: &StaticNames) -> String {
    if display.is_empty() {
        return "未知英雄".to_string();
    }
    if let Some(champ) = champions
        .values()
        .find(|c| c.name == display || c.id == display)
    {
        return names
            .champion(&champ.key)
            .map(str::to_string)
            .unwrap_or_else(|| champ.name.clone());
    }
    display.to_string()
}

/// Live Client Data reports summoner spells by English display name only.
fn spell_zh_by_display(display: &str) -> String {
    match display {
        "Flash" => "闪现",
        "Teleport" => "传送",
        "Smite" => "惩戒",
        "Ignite" => "点燃",
        "Heal" => "治疗术",
        "Ghost" => "幽灵疾步",
        "Barrier" => "屏障",
        "Cleanse" => "净化",
        "Exhaust" => "衰竭",
        "Clarity" => "清晰术",
        "Mark" => "标记",
        "Dash" => "突进",
        "Poro Toss" => "魄罗投掷",
        "Poro Dash" => "魄罗冲撞",
        other if !other.is_empty() => other,
        _ => "未知",
    }
    .to_string()
}

/// Champ select reports summoner spells by numeric id.
fn spell_zh_by_id(id: i64) -> String {
    match id {
        1 => "净化",
        3 => "衰竭",
        4 => "闪现",
        6 => "幽灵疾步",
        7 => "治疗术",
        11 => "惩戒",
        12 => "传送",
        13 => "清晰术",
        14 => "点燃",
        21 => "屏障",
        30 => "魄罗投掷",
        31 => "魄罗冲撞",
        32 => "标记",
        other => return format!("技能{other}"),
    }
    .to_string()
}

fn dragon_zh(dragon_type: &str) -> String {
    match dragon_type {
        "Infernal" => "火龙",
        "Mountain" => "土龙",
        "Cloud" => "风龙",
        "Ocean" => "水龙",
        "Hextech" => "海克斯龙",
        "Chemtech" => "炼金龙",
        "Elder" => "远古巨龙",
        other => other,
    }
    .to_string()
}

fn render_items(items: &[(i64, i64)], names: &StaticNames) -> String {
    if items.is_empty() {
        return "无".to_string();
    }
    items
        .iter()
        .map(|(id, count)| {
            let name = names
                .item(*id)
                .map(str::to_string)
                .unwrap_or_else(|| format!("装备{id}"));
            if *count > 1 {
                format!("{name}x{count}")
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub fn format_game_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

// ---------------------------------------------------------------------------
//  OP.GG backend meta rendering
// ---------------------------------------------------------------------------

/// Pick the section matching the assigned position (falls back to the first one).
pub fn best_section<'a>(
    sections: &'a [BuildSection],
    assigned_position: &str,
) -> Option<&'a BuildSection> {
    let aliases = opgg_position_aliases(assigned_position);
    sections
        .iter()
        .find(|s| aliases.iter().any(|a| s.position.eq_ignore_ascii_case(a)))
        .or_else(|| sections.first())
}

/// The top rune page for the current lane, used by "apply best rune" / auto-apply.
/// OP.GG lists runes by popularity inside each position section, so the first
/// entry of the matching section is the recommended page.
pub fn best_rune_for_position<'a>(
    sections: &'a [BuildSection],
    assigned_position: &str,
) -> Option<&'a crate::builds::Rune> {
    best_section(sections, assigned_position)?.runes.first()
}

/// One-line summary used for non-local players:
/// "暗裔剑魔·上单: 梯度T2 胜率50.3% 常用基石:征服者+坚决系"
pub fn render_opgg_summary(
    champion_id: i64,
    assigned_position: &str,
    champions: &ChampionsMap,
    names: &StaticNames,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
) -> Option<String> {
    let sections = sections_map.get(&champion_id)?;
    let section = best_section(sections, assigned_position)?;
    let champ = champ_zh_by_id(champion_id, champions, names)
        .unwrap_or_else(|| section.alias.clone());
    let tier = section
        .champion_tier
        .as_deref()
        .filter(|t| !t.is_empty())
        .map(|t| format!("梯度{t} "))
        .unwrap_or_default();

    let keystone = section.runes.first().and_then(|rune| {
        let keystone_id = *rune.selected_perk_ids.first()?;
        let keystone = names.rune(keystone_id, "");
        if keystone.is_empty() {
            return None;
        }
        let tree = names.rune(rune.sub_style_id, "");
        if tree.is_empty() {
            Some(format!(" 常用基石:{keystone}"))
        } else {
            Some(format!(" 常用基石:{keystone}+{tree}系"))
        }
    });

    Some(format!(
        "{champ}·{}: {tier}胜率{} 样本{}场{}",
        position_label(&section.position),
        section.win_rate,
        section.pick_count,
        keystone.unwrap_or_default()
    ))
}

/// Rune page detail for one OP.GG rune entry:
/// "符文1(选用2345,胜率52.1%): 电刑,血之滋味,眼球收集器,无情猎手(主宰)+饼干配送,时间扭曲补药(启迪)+自适应之力x2,护甲"
fn render_opgg_rune(idx: usize, rune: &crate::builds::Rune, names: &StaticNames) -> String {
    let perks = &rune.selected_perk_ids;
    let render = |slice: &[i64]| {
        slice
            .iter()
            .map(|id| names.rune(*id, "").to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    };

    let primary = render(perks.get(..4).unwrap_or(&[]));
    let secondary = render(perks.get(4..6).unwrap_or(&[]));
    let stats = render(perks.get(6..9).unwrap_or(&[]));
    let primary_tree = names.rune(rune.primary_style_id, "主系");
    let sub_tree = names.rune(rune.sub_style_id, "副系");

    let mut parts = Vec::new();
    if !primary.is_empty() {
        parts.push(format!("{}({})", primary.join(","), primary_tree));
    }
    if !secondary.is_empty() {
        parts.push(format!("{}({})", secondary.join(","), sub_tree));
    }
    if !stats.is_empty() {
        parts.push(format!("属性:{}", stats.join(",")));
    }

    format!(
        "符文{}(选用{},胜率{}): {}",
        idx + 1,
        rune.pick_count,
        rune.win_rate,
        parts.join(" + ")
    )
}

/// Item build blocks translated to Chinese item names:
/// "标准出装: [starter]多兰之戒,生命药水 -> [core]卢登的激荡,影焰"
pub fn render_build_reference(build: &crate::builds::ItemBuild, names: &StaticNames) -> String {
    let blocks = build
        .blocks
        .iter()
        .map(|block| {
            let items = block
                .items
                .as_ref()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.id
                                .parse::<i64>()
                                .ok()
                                .and_then(|id| names.item(id).map(str::to_string))
                                .unwrap_or_else(|| item.id.clone())
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "...".to_string());
            format!("[{}]{items}", block.type_field)
        })
        .collect::<Vec<_>>()
        .join(" -> ");
    format!("{}: {}", build.title, blocks)
}

/// Full detail block for the local player during champ select.
pub fn render_opgg_detail(
    champion_id: i64,
    assigned_position: &str,
    champions: &ChampionsMap,
    names: &StaticNames,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
) -> Option<String> {
    let sections = sections_map.get(&champion_id)?;
    let section = best_section(sections, assigned_position)?;
    let champ = champ_zh_by_id(champion_id, champions, names)
        .unwrap_or_else(|| section.alias.clone());

    let mut lines = vec![format!(
        "{champ}·{}: 梯度{} 胜率{} 样本{}场",
        position_label(&section.position),
        section.champion_tier.as_deref().filter(|t| !t.is_empty()).unwrap_or("未知"),
        section.win_rate,
        section.pick_count
    )];

    for (idx, rune) in section.runes.iter().take(2).enumerate() {
        lines.push(render_opgg_rune(idx, rune, names));
    }

    if let Some(skills) = section.skills.as_ref().filter(|s| !s.is_empty()) {
        lines.push(format!("技能加点: {}", skills.join(">")));
    }

    if let Some(spells) = section.spells.as_ref().filter(|s| !s.is_empty()) {
        lines.push(format!("推荐召唤师技能: {}", spells.join("/")));
    }

    if let Some(build) = section.item_builds.first() {
        lines.push(render_build_reference(build, names));
    }

    Some(lines.join("\n"))
}

/// Matchup statistics between the local champion and the lane opponent,
/// taken from OP.GG counters data: first from the local champion's matchup
/// list, falling back to the opponent's list (win rate mirrored).
pub fn find_direct_matchup(
    local_champion_id: i64,
    local_position: &str,
    opponent_champion_id: i64,
    opponent_zh: &str,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
    champions: &ChampionsMap,
) -> Option<String> {
    if opponent_champion_id <= 0 {
        return None;
    }
    let key_of = |champion_id: i64| -> String {
        champions
            .values()
            .find(|c| c.key == champion_id.to_string())
            .map(|c| c.id.to_lowercase())
            .unwrap_or_default()
    };
    let opponent_key = key_of(opponent_champion_id);
    let local_key = key_of(local_champion_id);

    let win_rate_pct = |raw: &str| -> f64 {
        raw.trim_end_matches('%').trim().parse::<f64>().unwrap_or(50.0)
    };
    // Look the target champion up in a matchup list, returning
    // (our win-rate text, our win-rate pct, games), mirroring when the list
    // belongs to the opponent (our win rate = 100 - theirs).
    let find_in = |cs: &crate::builds::Counters,
                   target_id: i64,
                   target_key: &str,
                   mirrored: bool|
     -> Option<(String, f64, i64)> {
        cs.matchups
            .iter()
            .find(|m| {
                (m.champion_id > 0 && m.champion_id == target_id)
                    || (!target_key.is_empty() && m.champion_key == target_key)
            })
            .map(|m| {
                let pct = win_rate_pct(&m.win_rate);
                if mirrored {
                    (format!("{:.2}%", 100.0 - pct), 100.0 - pct, m.play)
                } else {
                    (m.win_rate.clone(), pct, m.play)
                }
            })
    };

    let section_lane = |cid: i64, pos: &str| -> Option<&BuildSection> {
        sections_map.get(&cid).and_then(|s| best_section(s, pos))
    };

    // Prefer the local champion's own matchup list (win rate is their own).
    if let Some(section) = section_lane(local_champion_id, local_position) {
        if let Some(counters) = section.counters.as_ref() {
            if let Some((win_rate, pct, play)) =
                find_in(counters, opponent_champion_id, &opponent_key, false)
            {
                let verdict = if pct >= 50.0 { "优势对位" } else { "劣势对位" };
                return Some(format!(
                    "对位大数据(OP.GG {}): 你对{opponent_zh}胜率{win_rate}({play}场), {verdict}",
                    position_label(&counters.position),
                ));
            }
        }
    }

    // Fall back to the opponent's matchup list (their rate mirrors ours).
    if let Some(section) = section_lane(opponent_champion_id, local_position) {
        if let Some(counters) = section.counters.as_ref() {
            if let Some((win_rate, pct, play)) =
                find_in(counters, local_champion_id, &local_key, true)
            {
                let verdict = if pct >= 50.0 { "优势对位" } else { "劣势对位" };
                return Some(format!(
                    "对位大数据(OP.GG {},按对方数据换算): 你对{opponent_zh}胜率约{win_rate}(参考{play}场), {verdict}",
                    position_label(&counters.position),
                ));
            }
        }
    }

    None
}

/// Ban-phase help: the top hardest matchups for the local champion from the
/// OP.GG counters table (lowest win rate first, weak samples filtered out).
pub fn ban_suggestions(
    local_champion_id: i64,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
    champions: &ChampionsMap,
    names: &StaticNames,
    limit: usize,
) -> Option<String> {
    if local_champion_id <= 0 {
        return None;
    }
    let counters = sections_map
        .get(&local_champion_id)?
        .iter()
        .find_map(|s| s.counters.as_ref())?;

    const MIN_GAMES: i64 = 20;
    let mut hard: Vec<&crate::builds::Matchup> = counters
        .matchups
        .iter()
        .filter(|m| {
            m.play >= MIN_GAMES
                && m.win_rate
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .map(|pct| pct < 50.0)
                    .unwrap_or(false)
        })
        .collect();
    hard.sort_by(|a, b| {
        let pct = |m: &crate::builds::Matchup| {
            m.win_rate.trim_end_matches('%').parse::<f64>().unwrap_or(50.0)
        };
        pct(a).total_cmp(&pct(b))
    });
    if hard.is_empty() {
        return None;
    }

    let name_of = |m: &crate::builds::Matchup| -> Option<String> {
        if m.champion_id > 0 {
            if let Some(zh) = champ_zh_by_id(m.champion_id, champions, names) {
                return Some(zh);
            }
        }
        // crawled without ids: resolve through the champions map by slug
        let by_key = champions
            .values()
            .find(|c| c.id.to_lowercase() == m.champion_key)?;
        champ_zh_by_id(by_key.key.parse().ok()?, champions, names)
    };

    let entries = hard
        .iter()
        .take(limit)
        .map(|m| {
            let name = name_of(m).unwrap_or_else(|| m.champion_key.clone());
            format!("{name} {}({}场)", m.win_rate, m.play)
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("Ban位参考(你最难打的对手): {entries}"))
}

/// Counter-system note for the lineup prompt / panel: opponent profile
/// (tags + damage typing) plus the local rule engine's rune adjustment.
/// Deterministic; no LLM involved.
pub fn build_counter_note(
    local_champion_id: i64,
    local_position: &str,
    opponent_champion_id: i64,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
    champions: &ChampionsMap,
) -> Option<String> {
    let opponent_info = champions
        .values()
        .find(|c| c.key == opponent_champion_id.to_string())?;
    let profile = crate::counter::profile_of(opponent_info);
    let tags = opponent_info
        .tags
        .iter()
        .map(|t| crate::counter::tag_zh(t))
        .filter(|t| *t != "未知")
        .take(2)
        .collect::<Vec<_>>()
        .join("/");

    let sections = sections_map.get(&local_champion_id)?;
    let base = best_rune_for_position(sections, local_position)?;
    let plan = crate::counter::plan_for_matchup(
        base,
        local_champion_id,
        local_position,
        opponent_champion_id,
        &opponent_info.id.to_lowercase(),
        Some(&profile),
        sections_map,
    )?;

    let mut line = format!(
        "符文针对(counter规则, 敌方{}·{},对位{}): {}",
        if tags.is_empty() { "未知类型" } else { &tags },
        profile.damage.label_zh(),
        plan.pressure.label_zh(),
        plan.line,
    );
    // 与主流页的差异明细(用户要求: 不同就要说明哪里不一样)
    if !plan.diffs.is_empty() {
        line.push_str(&format!(" [{}]", plan.diffs.join("; ")));
    }
    if let Some(reason) = plan.reasons.first() {
        line.push_str(&format!(" — {reason}"));
    }
    Some(line)
}

// ---------------------------------------------------------------------------
//  Champ select prompt
// ---------------------------------------------------------------------------

fn render_rune_page(page: &Value, names: &StaticNames) -> Option<String> {
    let perk_ids: Vec<i64> = page
        .get("selectedPerkIds")?
        .as_array()?
        .iter()
        .filter_map(Value::as_i64)
        .collect();
    if perk_ids.is_empty() {
        return None;
    }

    let rune_names: Vec<String> = perk_ids
        .iter()
        .map(|id| names.rune(*id, "").to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if rune_names.is_empty() {
        return None;
    }

    let primary_tree = names.rune(
        page.get("primaryStyleId").and_then(Value::as_i64).unwrap_or(0),
        "",
    );
    let sub_tree = names.rune(
        page.get("subStyleId").and_then(Value::as_i64).unwrap_or(0),
        "",
    );
    let trees = match (primary_tree.is_empty(), sub_tree.is_empty()) {
        (true, true) => String::new(),
        (false, true) => primary_tree,
        (true, false) => sub_tree,
        (false, false) => format!("{primary_tree}+{sub_tree}"),
    };

    Some(format!("{trees}: {}", rune_names.join(",")))
}

pub fn build_lineup_prompt(
    session: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
    local_rune_page: Option<&Value>,
    ranks: &HashMap<i64, RankInfo>,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
    atlas: &PlaystyleAtlas,
) -> anyhow::Result<String> {
    let snapshot = ChampSelectSnapshot::from_session(session)?;

    let ban_names = |bans: &[i64]| {
        bans.iter()
            .filter_map(|id| champ_zh_by_id(*id, champions, names))
            .collect::<Vec<_>>()
            .join(",")
    };
    let my_bans = ban_names(&snapshot.my_bans);
    let their_bans = ban_names(&snapshot.their_bans);

    let render_member = |member: &ChampSelectMember| -> String {
        let champ = champ_zh_by_id(member.effective_champion(), champions, names)
            .map(|zh| {
                if member.champion_id > 0 {
                    zh
                } else {
                    format!("{zh}(意向)")
                }
            })
            .unwrap_or_else(|| "未选择".to_string());
        let rank = ranks
            .get(&member.summoner_id)
            .map(|r| format!(", {r}"))
            .unwrap_or_default();
        let spells = if member.spell1_id > 0 || member.spell2_id > 0 {
            format!(
                ", {}/{}",
                spell_zh_by_id(member.spell1_id),
                spell_zh_by_id(member.spell2_id)
            )
        } else {
            String::new()
        };
        format!(
            "- {champ}({}{}{})",
            position_label(&member.assigned_position),
            rank,
            spells
        )
    };

    let my_team: Vec<String> = snapshot.my_team.iter().map(render_member).collect();
    let their_team: Vec<String> = snapshot.their_team.iter().map(render_member).collect();

    let local = snapshot.local_member().context("champ select session missing local player")?;
    let local_champion_id = local.effective_champion();
    let local_champ = champ_zh_by_id(local_champion_id, champions, names)
        .unwrap_or_else(|| format!("英雄{local_champion_id}"));
    let local_position = position_label(&local.assigned_position);

    let mut local_line = format!("本机玩家: {local_champ}({local_position})");
    if let Some(page) = local_rune_page.and_then(|page| render_rune_page(page, names)) {
        local_line.push_str(&format!(", 当前符文页: {page}"));
    }

    let mut matchup_line = String::new();
    if let Some(opponent) = snapshot.lane_opponent() {
        let opponent_id = opponent.effective_champion();
        if let Some(opponent_champ) = champ_zh_by_id(opponent_id, champions, names) {
            matchup_line = format!(
                "对位: 敌方{} {opponent_champ}",
                position_label(&opponent.assigned_position)
            );
            if let Some(matchup) = find_direct_matchup(
                local_champion_id,
                &local.assigned_position,
                opponent_id,
                &opponent_champ,
                sections_map,
                champions,
            ) {
                matchup_line.push('\n');
                matchup_line.push_str(&matchup);
            }
            if let Some(note) = build_counter_note(
                local_champion_id,
                &local.assigned_position,
                opponent_id,
                sections_map,
                champions,
            ) {
                matchup_line.push('\n');
                matchup_line.push_str(&note);
            }
            // 心理图谱: 对面英雄的意图/软肋(问 LLM 该识破什么, 而不是猜)
            if let Some(intel) = atlas.render_for_prompt(&opponent_id.to_string(), &opponent_champ)
            {
                matchup_line.push('\n');
                matchup_line.push_str(&intel);
            }
            // 心战星环: 我方战略 + 敌方战略/心理 + 压力战术(三十六计)
            if let (Some(own_info), Some(opp_info)) = (
                champions.values().find(|c| c.key == local_champion_id.to_string()),
                champions.values().find(|c| c.key == opponent_id.to_string()),
            ) {
                let pressure = crate::counter::pressure_of_matchup(
                    local_champion_id,
                    &local.assigned_position,
                    opponent_id,
                    &opp_info.id.to_lowercase(),
                    sections_map,
                );
                let card = crate::war::WarSystem::load().war_card(own_info, opp_info, &pressure);
                matchup_line.push('\n');
                matchup_line.push_str(&crate::war::render_for_prompt(&card));
            }
        }
    }

    if let Some(bans) = ban_suggestions(local_champion_id, sections_map, champions, names, 3) {
        matchup_line.push('\n');
        matchup_line.push_str(&bans);
    }

    // OP.GG backend stats: detail for the local champion, one-liners for everyone else.
    let mut meta_lines: Vec<String> = Vec::new();
    if let Some(detail) = render_opgg_detail(
        local_champion_id,
        &local.assigned_position,
        champions,
        names,
        sections_map,
    ) {
        meta_lines.push(format!("[本机英雄数据]\n{detail}"));
    }
    for member in snapshot.my_team.iter().chain(snapshot.their_team.iter()) {
        let champion_id = member.effective_champion();
        if champion_id == local_champion_id || champion_id <= 0 {
            continue;
        }
        if let Some(line) =
            render_opgg_summary(champion_id, &member.assigned_position, champions, names, sections_map)
        {
            meta_lines.push(line);
        }
    }
    let meta_block = if meta_lines.is_empty() {
        String::new()
    } else {
        format!("\n\nOP.GG数据参考(当前版本):\n{}", meta_lines.join("\n"))
    };

    Ok(format!(
        "阶段: 英雄选择\nBan: 我方[{my_bans}] 敌方[{their_bans}]\n我方阵容:\n{}\n\n敌方阵容:\n{}\n\n{local_line}\n{matchup_line}{meta_block}\n\n请输出: 1.对位分析(本机英雄对位要点) 2.符文建议(与当前符文页对比指出要换的符文) 3.出装与召唤师技能建议",
        my_team.join("\n"),
        their_team.join("\n"),
    ))
}

// ---------------------------------------------------------------------------
//  Live game prompt (full allgamedata)
// ---------------------------------------------------------------------------

pub fn build_live_game_prompt(
    game_data: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
    local_sections: Option<&HashMap<i64, Vec<BuildSection>>>,
    local_champion_id: i64,
    atlas: &PlaystyleAtlas,
) -> anyhow::Result<String> {
    let snapshot = LiveSnapshot::from_all_game_data(game_data)?;

    let local = snapshot.local_player();
    let local_team = local.map(|p| p.team.as_str()).unwrap_or("ORDER");

    let render_player = |player: &LivePlayer| -> String {
        let champ = champ_zh_by_display(&player.champion_name, champions, names);
        let keystone = names.rune(player.keystone_id, &player.keystone_name);
        let primary_tree = names.rune(player.primary_tree_id, &player.primary_tree_name);
        let rune_text = if primary_tree.is_empty() {
            keystone
        } else {
            format!("{keystone}+{primary_tree}")
        };
        let death = if player.is_dead {
            format!(", 阵亡{:.0}s重生", player.respawn_timer)
        } else {
            String::new()
        };
        format!(
            "- {champ}({}, Lv{}, {}, {}刀, {rune_text}, {}/{}, 装备:{}{death})",
            position_label(&player.position),
            player.level,
            player.kda(),
            player.creep_score,
            spell_zh_by_display(&player.spell_one),
            spell_zh_by_display(&player.spell_two),
            render_items(&player.items, names)
        )
    };

    let allied_team: Vec<String> = snapshot
        .team_players(local_team)
        .into_iter()
        .map(render_player)
        .collect();
    let enemy_team = if local_team == "ORDER" { "CHAOS" } else { "ORDER" };
    let enemy_players: Vec<String> = snapshot
        .team_players(enemy_team)
        .into_iter()
        .map(render_player)
        .collect();

    if allied_team.is_empty() || enemy_players.is_empty() {
        bail!("Live Client Data does not contain both teams");
    }

    let environment = format!(
        "游戏环境: 模式={}, 时间={}, 地图={}, 地形={}, 状态={}",
        snapshot.game_mode,
        format_game_time(snapshot.game_time),
        snapshot.map_name,
        if snapshot.map_terrain.is_empty() { "默认" } else { &snapshot.map_terrain },
        if snapshot.ended { "已结束" } else { "进行中" }
    );

    let my_kills = snapshot.team_kills(local_team);
    let their_kills = snapshot.team_kills(enemy_team);

    let objectives_line = |(label, obj): (&str, &TeamObjectives)| {
        let dragons = obj
            .dragons
            .iter()
            .map(|d| dragon_zh(d))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{label}[小龙:{} 巢虫{} 先锋{} 男爵{} 塔{} 水晶{}]",
            if dragons.is_empty() { "无".to_string() } else { dragons },
            obj.void_grubs,
            obj.heralds,
            obj.barons,
            obj.turrets,
            obj.inhibitors
        )
    };
    let (my_obj, their_obj) = if local_team == "ORDER" {
        (&snapshot.order, &snapshot.chaos)
    } else {
        (&snapshot.chaos, &snapshot.order)
    };

    let mut local_lines: Vec<String> = Vec::new();
    if let (Some(active), Some(local)) = (snapshot.active.as_ref(), snapshot.local_player()) {
        let champ = champ_zh_by_display(&local.champion_name, champions, names);
        let stat = |key: &str| format!("{:.0}", active.stats.get(key).copied().unwrap_or(0.0));
        let attack_speed = format!("{:.2}", active.stats.get("attackSpeed").copied().unwrap_or(0.0));
        let crit = format!("{:.0}", active.stats.get("critChance").copied().unwrap_or(0.0));

        local_lines.push(format!(
            "本机玩家: {champ}({}, Lv{}), 金币{:.0}, 加点Q{}W{}E{}R{}, 面板:攻强{} 法强{} 护甲{} 魔抗{} 攻速{} 暴击{}% 生命{}/{} 移速{}",
            position_label(&local.position),
            active.level.max(local.level),
            active.current_gold,
            active.q_level,
            active.w_level,
            active.e_level,
            active.r_level,
            stat("attackDamage"),
            stat("abilityPower"),
            stat("armor"),
            stat("magicResist"),
            attack_speed,
            crit,
            stat("currentHealth"),
            stat("maxHealth"),
            stat("moveSpeed"),
        ));

        let rune_names: Vec<String> = active
            .full_runes
            .iter()
            .map(|(id, display)| names.rune(*id, display))
            .collect();
        if !rune_names.is_empty() {
            local_lines.push(format!("本机符文: {}", rune_names.join(",")));
        }
    }

    if let Some(opponent) = snapshot.lane_opponent() {
        let champ = champ_zh_by_display(&opponent.champion_name, champions, names);
        let keystone = names.rune(opponent.keystone_id, &opponent.keystone_name);
        local_lines.push(format!(
            "对位: 敌方{} {champ}(Lv{}, {}, {}刀, 基石:{keystone})",
            position_label(&opponent.position),
            opponent.level,
            opponent.kda(),
            opponent.creep_score,
        ));

        // OP.GG counter statistics for this exact matchup, when crawled.
        if let (Some(sections_map), Some(local)) = (local_sections, snapshot.local_player()) {
            if local_champion_id > 0 {
                let opponent_id = champions
                    .values()
                    .find(|c| {
                        c.name == opponent.champion_name || c.id == opponent.champion_name
                    })
                    .and_then(|c| c.key.parse::<i64>().ok())
                    .unwrap_or(0);
                if let Some(matchup) = find_direct_matchup(
                    local_champion_id,
                    &local.position,
                    opponent_id,
                    &champ,
                    sections_map,
                    champions,
                ) {
                    local_lines.push(matchup);
                }
            }
        }

        // 对位心理(意图/软肋): 告诉 LLM 对面"想干什么", 而不是让它猜
        if let Some(key) = champions
            .values()
            .find(|c| c.name == opponent.champion_name || c.id == opponent.champion_name)
            .map(|c| c.key.clone())
        {
            if let Some(intel) = atlas.render_for_prompt(&key, &champ) {
                local_lines.push(intel);
            }
        }

        // 心战: 战略 + 战术两件套进入实时建议
        if let Some(local) = snapshot.local_player() {
            if let (Some(own_info), Some(opp_info), Some(sections_map)) = (
                champions
                    .values()
                    .find(|c| c.id == local.champion_name || c.name == local.champion_name),
                champions
                    .values()
                    .find(|c| c.name == opponent.champion_name || c.id == opponent.champion_name),
                local_sections,
            ) {
                let opponent_id = opp_info.key.parse::<i64>().unwrap_or(0);
                let pressure = crate::counter::pressure_of_matchup(
                    local_champion_id,
                    &local.position,
                    opponent_id,
                    &opp_info.id.to_lowercase(),
                    sections_map,
                );
                let card = crate::war::WarSystem::load().war_card(own_info, opp_info, &pressure);
                local_lines.push(crate::war::render_for_prompt(&card));
            }
        }
    }

    let mut recent_line = String::new();
    if !snapshot.recent_kills.is_empty() {
        let resolve = |name: &str| -> String {
            snapshot
                .players
                .iter()
                .find(|p| p.summoner_name == name)
                .map(|p| {
                    let champ = champ_zh_by_display(&p.champion_name, champions, names);
                    let side = if p.team == local_team { "我方" } else { "敌方" };
                    format!("{side}{champ}")
                })
                .unwrap_or_else(|| name.to_string())
        };
        let kills: Vec<String> = snapshot
            .recent_kills
            .iter()
            .map(|event| {
                format!(
                    "{} {}击杀{}",
                    format_game_time(event.event_time),
                    resolve(&event.killer),
                    resolve(&event.victim)
                )
            })
            .collect();
        recent_line = format!("\n\n近期击杀: {}", kills.join("; "));
    }

    let first_blood_line = snapshot
        .first_blood_by
        .as_ref()
        .map(|name| format!("一血: {name}"))
        .unwrap_or_default();

    let mut builds_reference = String::new();
    if let (Some(sections_map), Some(local)) = (local_sections, snapshot.local_player()) {
        if local_champion_id > 0 {
            if let Some(sections) = sections_map.get(&local_champion_id) {
                if let Some(section) = best_section(sections, &local.position) {
                    if let Some(build) = section.item_builds.first() {
                        builds_reference = format!(
                            "\n出装参考(OP.GG:{}): {}",
                            position_label(&section.position),
                            render_build_reference(build, names)
                        );
                    }
                }
            }
        }
    }

    let my_objectives = objectives_line(("我方", my_obj));
    let their_objectives = objectives_line(("敌方", their_obj));
    let local_block = local_lines.join("\n");
    let allied = allied_team.join("\n");
    let enemy = enemy_players.join("\n");
    let first_blood_sep = if first_blood_line.is_empty() { "" } else { ", " };

    Ok(format!(
        "{environment}\n\n比分: 我方{my_kills}杀 vs 敌方{their_kills}杀{first_blood_sep}{first_blood_line}\n资源: {my_objectives} {their_objectives}\n\n{local_block}\n\n我方实时阵容:\n{allied}\n\n敌方实时阵容:\n{enemy}{recent_line}{builds_reference}\n\n请按当前等级阶段给建议,并结合对位KDA/装备差和资源差给出对位分析与出装/打团策略。"
    ))
}

// ---------------------------------------------------------------------------
//  UI match panel text (for the desktop window, not the LLM)
// ---------------------------------------------------------------------------

fn objectives_zh(obj: &TeamObjectives) -> String {
    let dragons = obj
        .dragons
        .iter()
        .map(|d| dragon_zh(d))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "龙:{} 巢虫{} 先锋{} 男爵{} 塔{} 水晶{}",
        if dragons.is_empty() { "无".to_string() } else { dragons },
        obj.void_grubs,
        obj.heralds,
        obj.barons,
        obj.turrets,
        obj.inhibitors
    )
}

fn panel_player_line(player: &LivePlayer, champions: &ChampionsMap, names: &StaticNames) -> String {
    let champ = champ_zh_by_display(&player.champion_name, champions, names);
    let keystone = names.rune(player.keystone_id, &player.keystone_name);
    let state = if player.is_dead {
        format!(" 阵亡{:.0}s", player.respawn_timer)
    } else {
        String::new()
    };
    format!(
        "{} Lv{} {} | {} | {}刀 | {} | {}/{} | {}{}",
        champ,
        player.level,
        position_label(&player.position),
        player.kda(),
        player.creep_score,
        keystone,
        spell_zh_by_display(&player.spell_one),
        spell_zh_by_display(&player.spell_two),
        render_items(&player.items, names),
        state
    )
}

fn columns_of(spec: &[(&str, i32, bool)]) -> Vec<TableColumn> {
    spec.iter()
        .map(|(title, width, emphasis)| TableColumn {
            title: (*title).to_string(),
            width: *width,
            emphasis: *emphasis,
        })
        .collect()
}

/// 一个玩家的排位信息(LCU `/lol-ranked/v1/ranked-stats/{puuid}` 的单双排)。
///
/// 这是**个人**数据, 与"英雄全服胜率"是两回事: 用户 2026-10-05 要的就是个人战绩,
/// 而个人**分英雄**胜率 LCU 拿不到(只提供本地玩家的比赛记录), 所以能给的最近似项是
/// 「该玩家本赛季单双排总胜率」。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RankInfo {
    /// "黄金 II" / "最强王者 320胜点"
    pub rank: String,
    /// 个人本赛季单双排胜率, 例如 "55%"(没有战绩时为空)
    pub personal_rate: String,
    /// "120胜98负"
    pub personal_record: String,
}

impl std::fmt::Display for RankInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.prompt_text())
    }
}

impl RankInfo {
    /// prompt / 日志用的一行文字。
    pub fn prompt_text(&self) -> String {
        let mut text = format!("单双{}", self.rank);
        if !self.personal_record.is_empty() {
            text.push_str(&format!(" {}", self.personal_record));
        }
        text
    }
}

/// 段位英文 → 中文。
pub fn rank_tier_zh(tier: &str) -> String {
    match tier.to_ascii_uppercase().as_str() {
        "IRON" => "坚韧黑铁",
        "BRONZE" => "英勇黄铜",
        "SILVER" => "不屈白银",
        "GOLD" => "荣耀黄金",
        "PLATINUM" => "华贵铂金",
        "EMERALD" => "流光翡翠",
        "DIAMOND" => "璀璨钻石",
        "MASTER" => "超凡大师",
        "GRANDMASTER" => "傲世宗师",
        "CHALLENGER" => "最强王者",
        other => other,
    }
    .to_string()
}

/// 解析 LCU 排位数据: 段位 + 个人胜率 + 战绩。没有单双排成绩时返回 None。
pub fn parse_ranked_stats(stats: &Value) -> Option<RankInfo> {
    let solo = stats.get("queueMap")?.get("RANKED_SOLO_5x5")?;
    let tier = solo.get("tier").and_then(Value::as_str).unwrap_or("");
    if tier.is_empty() || tier.eq_ignore_ascii_case("NONE") {
        return None;
    }
    let division = solo.get("division").and_then(Value::as_str).unwrap_or("");
    let lp = solo.get("leaguePoints").and_then(Value::as_i64).unwrap_or(0);
    let wins = solo.get("wins").and_then(Value::as_i64).unwrap_or(0);
    let losses = solo.get("losses").and_then(Value::as_i64).unwrap_or(0);

    let rank = format!(
        "{}{}{}",
        rank_tier_zh(tier),
        division,
        if lp > 0 {
            format!(" {lp}胜点")
        } else {
            String::new()
        },
    );
    let (personal_rate, personal_record) = if wins + losses > 0 {
        (
            format!("{:.0}%", wins as f64 * 100.0 / (wins + losses) as f64),
            format!("{wins}胜{losses}负"),
        )
    } else {
        (String::new(), String::new())
    };
    Some(RankInfo {
        rank,
        personal_rate,
        personal_record,
    })
}

/// 选人期的"选手档案": 段位、OP.GG 分路胜率这些**只有选人阶段拿得到**的数据。
///
/// 用户 2026-10-05: "用一个状态表格维护选人、游戏过程中的所有关键数据" ——
/// 于是选人阶段把这些存进 AppState, 开局后 build_live_table 再按分路/英雄合并回来,
/// 让同一张表在整局里列不变、只是逐渐填满。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RosterEntry {
    /// LCU 分路(assignedPosition / live position), 用于跨阶段配对
    pub position: String,
    /// 分路中文(上/野/中/下/辅)
    pub position_label: String,
    pub champion_id: i64,
    /// 英雄中文名(选人期可能是"未选择"/"XX(意向)")
    pub champion: String,
    pub summoner: String,
    /// 段位(选人期查 ranked-stats 得到; 拿不到为 "-")
    pub rank: String,
    /// **个人**本赛季单双排胜率(LCU ranked-stats 的 wins/losses 算出来; 拿不到为 "-")
    pub personal_rate: String,
    /// **个人**战绩 "120胜98负"(拿不到为空)
    pub personal_record: String,
    /// OP.GG 该英雄该分路胜率(全服数据, 拿不到为 "-")
    pub win_rate: String,
    pub games: String,
    pub mine_team: bool,
    pub mine: bool,
    pub opponent: bool,
}

/// 统一状态表格的列(选人与对局共用): 选人期先填 段位/胜率, 开局后填 KDA/补刀/等级/装备。
const MATCH_COLUMNS: [(&str, i32, bool); 10] = [
    ("位", 30, false),
    ("英雄", 80, true),
    ("召唤师", 92, false),
    ("段位", 60, false),
    ("个人胜率", 60, true),
    ("英雄胜率", 60, false),
    ("KDA", 58, true),
    ("补刀", 40, true),
    ("等级", 32, true),
    ("装备", 0, false), // 弹性列; 选人阶段这一列放"场次"(见下)
];

/// 空值统一显示成 "-", 免得表格里出现空白格子。
fn or_dash(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "-".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 分路取值: 顶层 `position` 为空时退用 `counters.position`。
/// 实测本地 173 个英雄的顶层 position **全是空的**(爬虫把分路存在 counters 里),
/// 不做这层回退的话分路匹配永远落空, 只能退到"第一条"——多分路英雄会拿错胜率。
fn section_position(section: &BuildSection) -> &str {
    if !section.position.trim().is_empty() {
        return section.position.as_str();
    }
    section
        .counters
        .as_ref()
        .map(|counters| counters.position.as_str())
        .unwrap_or("")
}
/// 按英雄 id + 分路从 OP.GG 数据里取胜率; 分路对不上时退用第一条; 拿不到返回 "-"。
fn section_win_rate(
    sections: &HashMap<i64, Vec<BuildSection>>,
    champion_id: i64,
    position: &str,
) -> String {
    if champion_id <= 0 {
        return "-".to_string();
    }
    let Some(list) = sections.get(&champion_id) else {
        return "-".to_string();
    };
    let wanted = position_label(position);
    let section = list
        .iter()
        .find(|s| !wanted.is_empty() && position_label(section_position(s)) == wanted)
        .or_else(|| list.first());
    match section {
        Some(s) if !s.win_rate.trim().is_empty() => s.win_rate.clone(),
        _ => "-".to_string(),
    }
}

/// 按分路/英雄/召唤师名把选人档案配到当前玩家身上。
///
/// 依次尝试: **同队**分路 → **同队**英雄 → 召唤师名 → 不分队伍的英雄兜底。
/// 必须区分队伍: 我和我的对位是**同一个分路**(中 vs 中), 只按分路找会让两行都命中
/// 名单里第一条同分路记录 —— 用户看到的就是"对面跟我一模一样的胜率"(2026-10-05 报障)。
fn find_roster<'a>(
    roster: &'a [RosterEntry],
    position: &str,
    champion_id: i64,
    summoner: &str,
    on_my_team: bool,
) -> Option<&'a RosterEntry> {
    let position_label_text = position_label(position);
    if !position_label_text.is_empty() {
        if let Some(entry) = roster.iter().find(|entry| {
            entry.mine_team == on_my_team && entry.position_label == position_label_text
        }) {
            return Some(entry);
        }
    }
    if champion_id > 0 {
        if let Some(entry) = roster.iter().find(|entry| {
            entry.mine_team == on_my_team
                && entry.champion_id == champion_id
                && entry.champion_id > 0
        }) {
            return Some(entry);
        }
    }
    // 召唤师名本身唯一, 不必再看队伍
    let summoner = summoner.trim();
    if !summoner.is_empty() {
        if let Some(entry) = roster.iter().find(|entry| entry.summoner.trim() == summoner) {
            return Some(entry);
        }
    }
    // 最后兜底: 队伍标记可能对不上(例如盲选期抓的档案), 只认英雄
    if champion_id > 0 {
        return roster
            .iter()
            .find(|entry| entry.champion_id == champion_id && entry.champion_id > 0);
    }
    None
}

/// 选人阶段: 生成状态表格 + 需要缓存到对局期的选手档案。
pub fn build_champ_select_table(
    session: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
    ranks: &HashMap<i64, RankInfo>,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
) -> anyhow::Result<(DataTable, Vec<RosterEntry>)> {
    let snapshot = ChampSelectSnapshot::from_session(session)?;

    let ban_names = |bans: &[i64]| {
        bans.iter()
            .filter_map(|id| champ_zh_by_id(*id, champions, names))
            .collect::<Vec<_>>()
            .join(",")
    };

    let local_cell = snapshot.local_member().map(|m| m.cell_id).unwrap_or(-1);
    let opponent_cell = snapshot.lane_opponent().map(|m| m.cell_id).unwrap_or(-1);

    // 名字: 区服不给名字时按队伍顺序兜底成"队友1/对手2", 不留空白
    let name_of = |member: &ChampSelectMember, index: usize, mine: bool| -> String {
        if !member.display_name.is_empty() {
            return member.display_name.clone();
        }
        if member.cell_id == local_cell {
            return "我".to_string();
        }
        format!("{}{}", if mine { "队友" } else { "对手" }, index + 1)
    };

    let entry_of = |member: &ChampSelectMember, index: usize, mine: bool| -> RosterEntry {
        let champion = champ_zh_by_id(member.effective_champion(), champions, names)
            .map(|zh| {
                if member.champion_id > 0 {
                    zh
                } else {
                    format!("{zh}(意向)")
                }
            })
            .unwrap_or_else(|| "未选择".to_string());
        let position = position_label(&member.assigned_position);
        let section = sections_map.get(&member.effective_champion()).and_then(|list| {
            list.iter()
                .find(|s| position_label(section_position(s)) == position)
                .or_else(|| list.first())
        });
        RosterEntry {
            position: member.assigned_position.clone(),
            position_label: position,
            champion_id: member.effective_champion(),
            champion,
            summoner: name_of(member, index, mine),
            // 段位/个人胜率/战绩: 同一份 LCU 排位数据, 与英雄无关
            rank: ranks
                .get(&member.summoner_id)
                .map(|info| info.rank.clone())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "-".to_string()),
            personal_rate: ranks
                .get(&member.summoner_id)
                .map(|info| info.personal_rate.clone())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "-".to_string()),
            personal_record: ranks
                .get(&member.summoner_id)
                .map(|info| info.personal_record.clone())
                .unwrap_or_default(),
            win_rate: section
                .map(|s| s.win_rate.clone())
                .unwrap_or_else(|| "-".to_string()),
            games: section
                .map(|s| s.pick_count.to_string())
                .unwrap_or_else(|| "-".to_string()),
            mine_team: mine,
            mine: member.cell_id == local_cell,
            opponent: member.cell_id == opponent_cell,
        }
    };

    let mut roster: Vec<RosterEntry> = snapshot
        .my_team
        .iter()
        .enumerate()
        .map(|(index, member)| entry_of(member, index, true))
        .collect();
    roster.extend(
        snapshot
            .their_team
            .iter()
            .enumerate()
            .map(|(index, member)| entry_of(member, index, false)),
    );

    let row_of = |entry: &RosterEntry| -> TableRow {
        TableRow {
            section: String::new(),
            cells: vec![
                or_dash(&entry.position_label),
                or_dash(&entry.champion),
                or_dash(&entry.summoner),
                or_dash(&entry.rank),
                or_dash(&entry.personal_rate),   // 个人本赛季胜率(LCU)
                or_dash(&entry.win_rate),        // 英雄全服胜率(OP.GG)
                "-".to_string(), // KDA: 对局开始后才有
                "-".to_string(), // 补刀
                "-".to_string(), // 等级
                or_dash(&entry.games), // 选人期装备列先放"场次"(样本量), 开局换成装备
            ],
            mine_team: entry.mine_team,
            mine: entry.mine,
            opponent: entry.opponent,
        }
    };

    let order_key = |entry: &RosterEntry| -> u8 {
        match entry.position_label.as_str() {
            "上" => 0,
            "野" => 1,
            "中" => 2,
            "下" => 3,
            "辅" => 4,
            _ => 5,
        }
    };
    let mut mine_rows: Vec<&RosterEntry> = roster.iter().filter(|e| e.mine_team).collect();
    let mut their_rows: Vec<&RosterEntry> = roster.iter().filter(|e| !e.mine_team).collect();
    mine_rows.sort_by_key(|entry| order_key(entry));
    their_rows.sort_by_key(|entry| order_key(entry));

    let mut rows = vec![TableRow {
        section: "我方".to_string(),
        mine_team: true,
        ..Default::default()
    }];
    rows.extend(mine_rows.iter().map(|entry| row_of(entry)));
    rows.push(TableRow {
        section: "敌方".to_string(),
        mine_team: false,
        ..Default::default()
    });
    rows.extend(their_rows.iter().map(|entry| row_of(entry)));

    let mut sub_lines = vec![format!(
        "ban 我方[{}]  敌方[{}]",
        ban_names(&snapshot.my_bans),
        ban_names(&snapshot.their_bans)
    )];
    sub_lines.retain(|line| !line.trim().is_empty());

    let mut notes: Vec<String> = Vec::new();
    if let Some(local) = snapshot.local_member() {
        let mut status = format!("本机位置 {}", position_label(&local.assigned_position));
        if let Some(opponent) = snapshot.lane_opponent() {
            if let Some(champ) = champ_zh_by_id(opponent.effective_champion(), champions, names) {
                status.push_str(&format!(
                    " | 对位 敌方{} {champ}",
                    position_label(&opponent.assigned_position)
                ));
            }
        }
        notes.push(status);

        if let Some(bans) =
            ban_suggestions(local.effective_champion(), sections_map, champions, names, 3)
        {
            notes.push(bans);
        }
        if let Some(opponent) = snapshot.lane_opponent() {
            if let Some(note) = build_counter_note(
                local.effective_champion(),
                &local.assigned_position,
                opponent.effective_champion(),
                sections_map,
                champions,
            ) {
                notes.push(note);
            }
        }
    }

    // 选人期没有装备, 最后一列放"场次", 表头也要跟着改(否则列名与内容不符)
    let mut columns = columns_of(&MATCH_COLUMNS);
    if let Some(last) = columns.last_mut() {
        last.title = "场次".to_string();
    }
    if let Some(games_col) = columns.get_mut(9) {
        games_col.emphasis = false;
    }

    Ok((
        DataTable {
            summary: "英雄选择中".to_string(),
            sub_lines,
            columns,
            rows,
            notes,
            footnote: String::new(),
        },
        roster,
    ))
}

/// 对局阶段: 同一张状态表格, 用实时数据填满, 并用缓存的选人档案补 段位/胜率。
pub fn build_live_table(
    game_data: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
    roster: &[RosterEntry],
    sections: &HashMap<i64, Vec<BuildSection>>,
) -> anyhow::Result<DataTable> {
    let snapshot = LiveSnapshot::from_all_game_data(game_data)?;

    let local = snapshot.local_player();
    let local_team = local.map(|p| p.team.as_str()).unwrap_or("ORDER");
    let enemy_team = if local_team == "ORDER" { "CHAOS" } else { "ORDER" };
    let (my_obj, their_obj) = if local_team == "ORDER" {
        (&snapshot.order, &snapshot.chaos)
    } else {
        (&snapshot.chaos, &snapshot.order)
    };

    let local_name = local.map(|p| p.summoner_name.clone()).unwrap_or_default();
    let opponent_name = snapshot
        .lane_opponent()
        .map(|p| p.summoner_name.clone())
        .unwrap_or_default();

    let row_of = |player: &LivePlayer| -> TableRow {
        let dead = if player.is_dead {
            format!("阵亡 {:.0}s", player.respawn_timer)
        } else {
            String::new()
        };
        let items = if dead.is_empty() {
            compact_items(&player.items, names)
        } else {
            format!("{} ({dead})", compact_items(&player.items, names))
        };
        // Live 数据的 championName 是 Data Dragon 别名(如 "MonkeyKing"),
        // 用它反查数字 key, 再和选人档案按英雄配对。
        let champion_id = champions
            .get(&player.champion_name)
            .and_then(|info| info.key.parse::<i64>().ok())
            .unwrap_or(0);
        let cached = find_roster(
            roster,
            &player.position,
            champion_id,
            &player.summoner_name,
            player.team == local_team,
        );
        let rank = cached
            .map(|e| e.rank.clone())
            .filter(|text| !text.trim().is_empty() && text != "-")
            .unwrap_or_else(|| "-".to_string());
        // 个人胜率与英雄无关, 直接来自选人期查到的排位战绩(段位那一列同一来源)
        let personal_rate = cached
            .map(|e| e.personal_rate.clone())
            .filter(|text| !text.trim().is_empty() && text != "-")
            .unwrap_or_else(|| "-".to_string());
        // 胜率**必须按他现在真正在玩的英雄**算: 选人后可以换英雄(交易), 档案里记的是
        // 选人时那个英雄 —— 用档案的胜率就会显示成"别人英雄的胜率"(用户 2026-10-05
        // 说"每个玩家当前使用的英雄胜率是假的")。所以先按 Live 的英雄查 OP.GG,
        // 查不到才退回档案里的值。段位与英雄无关, 仍用档案。
        let from_sections = section_win_rate(sections, champion_id, &player.position);
        let win_rate = if from_sections != "-" {
            from_sections
        } else {
            cached
                .map(|e| e.win_rate.clone())
                .filter(|text| !text.trim().is_empty() && text != "-")
                .unwrap_or_else(|| "-".to_string())
        };
        // 召唤师名: Live 数据里有就用(权威), 没有则沿用选人期缓存的
        let summoner = if player.summoner_name.trim().is_empty() {
            cached.map(|e| e.summoner.clone()).unwrap_or_default()
        } else {
            player.summoner_name.clone()
        };

        TableRow {
            section: String::new(),
            cells: vec![
                or_dash(&position_label(&player.position)),
                or_dash(&champ_zh_by_display(&player.champion_name, champions, names)),
                or_dash(&summoner),
                or_dash(&rank),
                or_dash(&personal_rate), // 个人本赛季胜率(LCU, 与英雄无关)
                or_dash(&win_rate),      // 当前英雄的全服胜率(OP.GG)
                or_dash(&player.kda()),
                player.creep_score.to_string(),
                player.level.to_string(),
                or_dash(&items),
            ],
            mine_team: player.team == local_team,
            mine: !local_name.is_empty() && player.summoner_name == local_name,
            opponent: !opponent_name.is_empty() && player.summoner_name == opponent_name,
        }
    };

    let order_key = |row: &TableRow| -> u8 {
        match row.cells.first().map(String::as_str).unwrap_or("") {
            "上" => 0,
            "野" => 1,
            "中" => 2,
            "下" => 3,
            "辅" => 4,
            _ => 5,
        }
    };
    let mut rows_mine: Vec<TableRow> = snapshot
        .team_players(local_team)
        .into_iter()
        .map(row_of)
        .collect();
    let mut rows_theirs: Vec<TableRow> = snapshot
        .team_players(enemy_team)
        .into_iter()
        .map(row_of)
        .collect();
    rows_mine.sort_by_key(order_key);
    rows_theirs.sort_by_key(order_key);

    let mut rows = vec![TableRow {
        section: "我方".to_string(),
        mine_team: true,
        ..Default::default()
    }];
    rows.extend(rows_mine);
    rows.push(TableRow {
        section: "敌方".to_string(),
        mine_team: false,
        ..Default::default()
    });
    rows.extend(rows_theirs);

    let mut notes: Vec<String> = Vec::new();
    if let Some(active) = snapshot.active.as_ref() {
        let rune_names: Vec<String> = active
            .full_runes
            .iter()
            .map(|(id, display)| names.rune(*id, display))
            .collect();
        notes.push(format!(
            "我的金币 {:.0} | 加点 Q{}W{}E{}R{} | 符文 {}",
            active.current_gold,
            active.q_level,
            active.w_level,
            active.e_level,
            active.r_level,
            rune_names.join(",")
        ));
    }
    if !snapshot.recent_kills.is_empty() {
        let resolve = |name: &str| -> String {
            snapshot
                .players
                .iter()
                .find(|p| p.summoner_name == name)
                .map(|p| champ_zh_by_display(&p.champion_name, champions, names))
                .unwrap_or_else(|| name.to_string())
        };
        let kills: Vec<String> = snapshot
            .recent_kills
            .iter()
            .map(|event| {
                format!(
                    "{} {}→{}",
                    format_game_time(event.event_time),
                    resolve(&event.killer),
                    resolve(&event.victim)
                )
            })
            .collect();
        notes.push(format!("近期击杀 {}", kills.join("; ")));
    }

    Ok(DataTable {
        summary: format!(
            "{} {} · 比分 {}:{}",
            if snapshot.ended { "已结束" } else { "对局中" },
            format_game_time(snapshot.game_time),
            snapshot.team_kills(local_team),
            snapshot.team_kills(enemy_team)
        ),
        sub_lines: vec![
            format!("我方  {}", table_objectives_zh(my_obj)),
            format!("敌方  {}", table_objectives_zh(their_obj)),
        ],
        columns: columns_of(&MATCH_COLUMNS),
        rows,
        notes,
        footnote: build_matchup_line(&snapshot, champions, names),
    })
}

/// 通用表格的一列。width = 逻辑像素; width 0 表示最后一列占满剩余宽度。
#[derive(Debug, Clone, Default)]
pub struct TableColumn {
    pub title: String,
    pub width: i32,
    /// 主列: UI 用正文号 + 主文字色; 其余用次要色
    pub emphasis: bool,
}

/// 通用表格的一行。section 非空 = 分区标题行(如"我方"/"敌方"), 此时 cells 不用。
#[derive(Debug, Clone, Default)]
pub struct TableRow {
    pub section: String,
    pub cells: Vec<String>,
    /// 属于我方(true)/敌方(false) —— 决定行首色条
    pub mine_team: bool,
    /// 这行是我(金条 + 金底)
    pub mine: bool,
    /// 这行是我的对位(红条)
    pub opponent: bool,
}

/// 界面上的**唯一**数据展示形态: 选人、对局、符文页列表都用它。
/// 用户 2026-10-05: "如果展示数据需要表格的话, 请直接使用表格…从头到尾一直使用"。
#[derive(Debug, Clone, Default)]
pub struct DataTable {
    /// 顶行大字, 如 "对局中 18:32 · 比分 12:9" / "英雄选择中"
    pub summary: String,
    /// 概览副行(双方资源 / ban 位)
    pub sub_lines: Vec<String>,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<TableRow>,
    /// 表下小字(我的金币/加点/符文、近期击杀…)
    pub notes: Vec<String>,
    /// 表下强调卡(对位对比)
    pub footnote: String,
}

/// 把任意表格渲染成可复制的纯文本。中文占两格宽, 列宽由 UI 的像素宽换算成字符宽
/// (8px ≈ 1 字符), 保证粘到聊天/记事本里仍然对齐。
pub fn render_table_text(table: &DataTable) -> String {
    let mut out = String::new();
    out.push_str(&table.summary);
    out.push('\n');
    for line in &table.sub_lines {
        out.push_str(line);
        out.push('\n');
    }

    let widths: Vec<usize> = table
        .columns
        .iter()
        .map(|c| {
            if c.width <= 0 {
                0
            } else {
                // 像素 → 字符: 8px/字符, 再留 1 格间距; 中文字符宽度由 pad_display 处理
                ((c.width / 8).max(2) as usize).saturating_sub(1)
            }
        })
        .collect();

    // 标记列(★/▲/空格)与数据行一致, 表头也要留一格
    let header: Vec<String> = table
        .columns
        .iter()
        .zip(&widths)
        .map(|(col, width)| pad_display(&col.title, *width))
        .collect();
    out.push_str(&format!(" {}", header.join(" ").trim_end()));
    out.push('\n');

    for row in &table.rows {
        if !row.section.is_empty() {
            out.push_str(&row.section);
            out.push('\n');
            continue;
        }
        let mark = if row.mine {
            "★"
        } else if row.opponent {
            "▲"
        } else {
            " "
        };
        let cells: Vec<String> = row
            .cells
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let width = widths.get(index).copied().unwrap_or(0);
                pad_display(cell, width)
            })
            .collect();
        out.push_str(&format!("{mark}{}", cells.join(" ").trim_end()));
        out.push('\n');
    }

    for note in &table.notes {
        out.push_str(note);
        out.push('\n');
    }
    if !table.footnote.is_empty() {
        out.push_str(&table.footnote);
        out.push('\n');
    }
    out
}

/// 显示宽度: CJK/全角算 2 格, 其它算 1 格。
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| {
            let cp = c as u32;
            let wide = matches!(cp,
                0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
                | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
                | 0x1F300..=0x1FAFF)
                || (0x20000..=0x3FFFD).contains(&cp);
            if wide { 2 } else { 1 }
        })
        .sum()
}

/// 按显示宽度补齐; width == 0 表示不补。
/// 超宽时截断成 "…"(中文两格宽, 所以截断也要按显示宽度算), 否则后面所有列都会右移。
fn pad_display(text: &str, width: usize) -> String {
    if width == 0 {
        return text.to_string();
    }
    let current = display_width(text);
    if current == width {
        return text.to_string();
    }
    if current < width {
        return format!("{}{}", text, " ".repeat(width - current));
    }

    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = display_width(&ch.to_string());
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    let used_after = display_width(&out);
    if used_after < width {
        out.push_str(&" ".repeat(width - used_after));
    }
    out
}

/// 对位对比: "我 阿卡丽 Lv12 8/2/3 196刀 · 对位 劫 Lv12 6/1/2 188刀 · 补刀 +8 · 击杀 +2"
/// 差值带正负号, 让人一眼看出领先还是落后。
fn build_matchup_line(
    snapshot: &LiveSnapshot,
    champions: &ChampionsMap,
    names: &StaticNames,
) -> String {
    let (Some(local), Some(opponent)) = (snapshot.local_player(), snapshot.lane_opponent()) else {
        return String::new();
    };

    let you = champ_zh_by_display(&local.champion_name, champions, names);
    let foe = champ_zh_by_display(&opponent.champion_name, champions, names);
    // 召唤师技能: 表格列被"段位/胜率"占满, 在这里补回来(对线判断 TP/引燃 很关键)
    let my_spells = format!(
        "{}/{}",
        spell_zh_by_display(&local.spell_one),
        spell_zh_by_display(&local.spell_two)
    );
    let foe_spells = format!(
        "{}/{}",
        spell_zh_by_display(&opponent.spell_one),
        spell_zh_by_display(&opponent.spell_two)
    );
    let cs_diff = local.creep_score - opponent.creep_score;
    let level_diff = local.level - opponent.level;
    let kill_diff = (local.kills - local.deaths) - (opponent.kills - opponent.deaths);

    let signed = |value: i64| -> String {
        if value > 0 {
            format!("+{value}")
        } else {
            value.to_string()
        }
    };

    let mut line = format!(
        "我 {you} {my_spells} Lv{} {} {}刀 · 对位 {foe} {foe_spells} Lv{} {} {}刀 · 补刀 {} · 等级 {} · 净击杀 {}",
        local.level,
        local.kda(),
        local.creep_score,
        opponent.level,
        opponent.kda(),
        opponent.creep_score,
        signed(cs_diff),
        signed(level_diff),
        signed(kill_diff),
    );
    if opponent.is_dead {
        line.push_str(&format!(" · 对位阵亡 {:.0}s", opponent.respawn_timer));
    }
    line
}

/// 表格用的资源摘要: 比 prompt 版短, 只留能在 1 行里读完的信息。
fn table_objectives_zh(obj: &TeamObjectives) -> String {
    let dragons = obj
        .dragons
        .iter()
        .map(|d| dragon_zh(d))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "龙 {} · 巢虫 {} · 先锋 {} · 男爵 {} · 塔 {}",
        if dragons.is_empty() { "0".to_string() } else { dragons },
        obj.void_grubs,
        obj.heralds,
        obj.barons,
        obj.turrets
    )
}

/// 装备列: 最多 4 件, 保留完整中文名(表格里装备列最宽, 截断反而看不懂)。
fn compact_items(items: &[(i64, i64)], names: &StaticNames) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (id, count) in items.iter().take(4) {
        let name = names
            .item(*id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("装备{id}"));
        if *count > 1 {
            parts.push(format!("{}x{}", name, count));
        } else {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Compact live match summary for the main window's match panel.
pub fn build_live_panel_text(
    game_data: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
) -> anyhow::Result<String> {
    let snapshot = LiveSnapshot::from_all_game_data(game_data)?;

    let local = snapshot.local_player();
    let local_team = local.map(|p| p.team.as_str()).unwrap_or("ORDER");
    let enemy_team = if local_team == "ORDER" { "CHAOS" } else { "ORDER" };
    let (my_obj, their_obj) = if local_team == "ORDER" {
        (&snapshot.order, &snapshot.chaos)
    } else {
        (&snapshot.chaos, &snapshot.order)
    };

    let mut lines: Vec<String> = vec![format!(
        "{} {} | 比分 我方{} vs 敌方{}",
        if snapshot.ended { "已结束" } else { "对局中" },
        format_game_time(snapshot.game_time),
        snapshot.team_kills(local_team),
        snapshot.team_kills(enemy_team)
    )];

    lines.push(format!(
        "资源 我方[{}] | 敌方[{}]",
        objectives_zh(my_obj),
        objectives_zh(their_obj)
    ));

    if let (Some(local), Some(opponent)) = (snapshot.local_player(), snapshot.lane_opponent()) {
        let you = champ_zh_by_display(&local.champion_name, champions, names);
        let foe = champ_zh_by_display(&opponent.champion_name, champions, names);
        let foe_state = if opponent.is_dead {
            format!(" 阵亡{:.0}s", opponent.respawn_timer)
        } else {
            String::new()
        };
        lines.push(format!(
            "对位: {you} Lv{} {} {}刀  vs  {foe} Lv{} {} {}刀{foe_state}",
            local.level,
            local.kda(),
            local.creep_score,
            opponent.level,
            opponent.kda(),
            opponent.creep_score
        ));
    }

    if let Some(active) = snapshot.active.as_ref() {
        let rune_names: Vec<String> = active
            .full_runes
            .iter()
            .map(|(id, display)| names.rune(*id, display))
            .collect();
        if !rune_names.is_empty() {
            lines.push(format!(
                "金币 {:.0} | 加点 Q{}W{}E{}R{} | 符文 {}",
                active.current_gold,
                active.q_level,
                active.w_level,
                active.e_level,
                active.r_level,
                rune_names.join(",")
            ));
        }
    }

    lines.push("—— 我方 ——".to_string());
    for player in snapshot.team_players(local_team) {
        lines.push(panel_player_line(player, champions, names));
    }
    lines.push("—— 敌方 ——".to_string());
    for player in snapshot.team_players(enemy_team) {
        lines.push(panel_player_line(player, champions, names));
    }

    if !snapshot.recent_kills.is_empty() {
        let resolve = |name: &str| -> String {
            snapshot
                .players
                .iter()
                .find(|p| p.summoner_name == name)
                .map(|p| champ_zh_by_display(&p.champion_name, champions, names))
                .unwrap_or_else(|| name.to_string())
        };
        let kills: Vec<String> = snapshot
            .recent_kills
            .iter()
            .map(|event| {
                format!(
                    "{} {}→{}",
                    format_game_time(event.event_time),
                    resolve(&event.killer),
                    resolve(&event.victim)
                )
            })
            .collect();
        lines.push(format!("近期击杀: {}", kills.join("; ")));
    }

    Ok(lines.join("\n"))
}

/// Champ-select summary for the match panel while no live data exists yet.
pub fn build_champ_select_panel_text(
    session: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
    ranks: &HashMap<i64, RankInfo>,
    sections_map: &HashMap<i64, Vec<BuildSection>>,
) -> anyhow::Result<String> {
    let snapshot = ChampSelectSnapshot::from_session(session)?;

    let ban_names = |bans: &[i64]| {
        bans.iter()
            .filter_map(|id| champ_zh_by_id(*id, champions, names))
            .collect::<Vec<_>>()
            .join(",")
    };

    let render_member = |member: &ChampSelectMember| -> String {
        let champ = champ_zh_by_id(member.effective_champion(), champions, names)
            .map(|zh| {
                if member.champion_id > 0 {
                    zh
                } else {
                    format!("{zh}(意向)")
                }
            })
            .unwrap_or_else(|| "未选择".to_string());
        let rank = ranks
            .get(&member.summoner_id)
            .map(|r| format!(" {r}"))
            .unwrap_or_default();
        format!("{champ}({}{})", position_label(&member.assigned_position), rank)
    };

    let mut lines: Vec<String> = vec!["英雄选择中".to_string()];
    lines.push(format!(
        "ban 我方[{}] 敌方[{}]",
        ban_names(&snapshot.my_bans),
        ban_names(&snapshot.their_bans)
    ));
    lines.push("—— 我方 ——".to_string());
    for member in &snapshot.my_team {
        lines.push(render_member(member));
    }
    lines.push("—— 敌方 ——".to_string());
    for member in &snapshot.their_team {
        lines.push(render_member(member));
    }

    if let Some(local) = snapshot.local_member() {
        let mut status = format!("本机位置: {}", position_label(&local.assigned_position));
        if let Some(opponent) = snapshot.lane_opponent() {
            if let Some(champ) = champ_zh_by_id(opponent.effective_champion(), champions, names) {
                status.push_str(&format!(
                    " | 对位: 敌方{} {champ}",
                    position_label(&opponent.assigned_position)
                ));
            }
        }
        lines.push(status);

        // During the ban phase the hovered champion drives the suggestion.
        if let Some(bans) =
            ban_suggestions(local.effective_champion(), sections_map, champions, names, 3)
        {
            lines.push(bans);
        }
        if let Some(opponent) = snapshot.lane_opponent() {
            if let Some(note) = build_counter_note(
                local.effective_champion(),
                &local.assigned_position,
                opponent.effective_champion(),
                sections_map,
                champions,
            ) {
                lines.push(note);
            }
        }
    }

    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
//  Objective reminders (event-driven, template-based voice nudges)
// ---------------------------------------------------------------------------

/// Objective spawn schedule (seconds). Timers reflect the current season:
/// elemental dragons every 5:00, void grubs at 6:00/12:00, Rift Herald at 15:00,
/// Baron Nashor at 25:00. Reminders fire ~30s before the spawn.
pub const DRAGON_INTERVAL: f64 = 300.0;
pub const GRUB_SPAWNS: [f64; 2] = [360.0, 720.0];
pub const HERALD_SPAWN: f64 = 900.0;
pub const BARON_SPAWN: f64 = 1500.0;
/// Announce this many seconds before an objective spawns.
pub const REMINDER_LEAD: f64 = 30.0;
/// Stop announcing a missed timer this many seconds past the spawn.
pub const REMINDER_GRACE: f64 = 45.0;

/// How important a reminder is; the UI tier filters on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderKind {
    /// First blood, dragon kills, baron kills.
    EventKey,
    /// Herald, void grubs, towers, inhibitors.
    EventMinor,
    /// Objective spawn countdowns.
    Timer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub kind: ReminderKind,
    pub text: String,
}

impl Reminder {
    fn key(text: impl Into<String>) -> Self {
        Self {
            kind: ReminderKind::EventKey,
            text: text.into(),
        }
    }

    fn minor(text: impl Into<String>) -> Self {
        Self {
            kind: ReminderKind::EventMinor,
            text: text.into(),
        }
    }

    fn timer(text: impl Into<String>) -> Self {
        Self {
            kind: ReminderKind::Timer,
            text: text.into(),
        }
    }
}

#[derive(Default)]
pub struct ReminderEngine {
    announced: std::collections::HashSet<String>,
    last_game_time: f64,
    seeded: bool,
}

impl ReminderEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (called when the client leaves a game).
    pub fn reset(&mut self) {
        self.announced.clear();
        self.last_game_time = 0.0;
        self.seeded = false;
    }

    fn next_dragon_spawn(snapshot: &LiveSnapshot) -> f64 {
        let last_kill = snapshot
            .events_log
            .iter()
            .filter(|e| e.name == "DragonKill")
            .map(|e| e.event_time)
            .fold(0.0_f64, f64::max);
        if last_kill > 0.0 {
            return last_kill + DRAGON_INTERVAL;
        }
        let next = ((snapshot.game_time + 1.0) / DRAGON_INTERVAL).ceil() * DRAGON_INTERVAL;
        next.max(DRAGON_INTERVAL)
    }

    fn announce_timer(
        announced: &mut std::collections::HashSet<String>,
        game_time: f64,
        spawn_time: f64,
        key: &str,
        text: String,
    ) -> Option<String> {
        let late = game_time - spawn_time;
        let due = game_time >= spawn_time - REMINDER_LEAD && late <= REMINDER_GRACE;
        if !due || announced.contains(key) {
            return None;
        }
        announced.insert(key.to_string());
        Some(text)
    }

    /// Inspect the latest snapshot and return newly-due reminder texts.
    /// The first snapshot of a game *seeds* known events without announcing them,
    /// so attaching mid-game does not replay old events.
    pub fn collect(
        &mut self,
        snapshot: &LiveSnapshot,
        champions: &ChampionsMap,
        names: &StaticNames,
    ) -> Vec<Reminder> {
        // Game time going backwards means a new match started.
        if snapshot.ended || snapshot.game_time + 20.0 < self.last_game_time {
            self.reset();
            if snapshot.ended {
                return Vec::new();
            }
        }
        self.last_game_time = snapshot.game_time;

        let local_team = snapshot
            .local_player()
            .map(|p| p.team.as_str())
            .unwrap_or("ORDER");
        let side_of = |team: &str| -> &str {
            if team == local_team { "我方" } else { "敌方" }
        };
        let champ_of = |summoner: &str| -> String {
            snapshot
                .players
                .iter()
                .find(|p| p.summoner_name == summoner)
                .map(|p| champ_zh_by_display(&p.champion_name, champions, names))
                .unwrap_or_else(|| summoner.to_string())
        };

        let mut out = Vec::new();

        for event in &snapshot.events_log {
            // Void grubs arrive in bursts of 3 within seconds; bucket them.
            let key = if event.name.contains("Horde") || event.name.contains("Grub") {
                format!("grub-burst:{:.0}", event.event_time / 30.0)
            } else {
                format!("{}:{:.1}:{}:{}", event.name, event.event_time, event.killer, event.detail)
            };
            if self.announced.contains(&key) {
                continue;
            }
            self.announced.insert(key);
            if !self.seeded {
                continue;
            }

            let side = side_of(&event.killer_team);
            let reminder = match event.name.as_str() {
                "FirstBlood" => {
                    Some(Reminder::key(format!("一血出现:{}{}拿下", side, champ_of(&event.killer))))
                }
                "DragonKill" => {
                    let dragon = dragon_zh(&event.detail);
                    if event.killer_team == local_team {
                        Some(Reminder::key(format!("我方击杀{dragon}")))
                    } else {
                        Some(Reminder::key(format!("敌方击杀{dragon},注意龙魂进度")))
                    }
                }
                "BaronKill" => {
                    if event.killer_team == local_team {
                        Some(Reminder::key("我方击杀男爵,抱团推进".to_string()))
                    } else {
                        Some(Reminder::key("敌方击杀男爵,注意守塔防守".to_string()))
                    }
                }
                "HeraldKill" => Some(Reminder::minor(format!("{side}击杀峡谷先锋"))),
                name if name.contains("Horde") || name.contains("Grub") => {
                    Some(Reminder::minor(format!("{side}拿下巢虫")))
                }
                "TurretKilled" => Some(Reminder::minor(format!("{side}推掉一座防御塔"))),
                "InhibKilled" => Some(Reminder::minor(format!("{side}摧毁一座水晶"))),
                _ => None,
            };
            if let Some(text) = reminder {
                out.push(text);
            }
        }

        // Spawn timers.
        let dragon_spawn = Self::next_dragon_spawn(snapshot);
        if let Some(text) = Self::announce_timer(
            &mut self.announced,
            snapshot.game_time,
            dragon_spawn,
            &format!("dragon:{dragon_spawn:.0}"),
            "小龙将在30秒后刷新,提前集合布眼".to_string(),
        ) {
            out.push(Reminder::timer(text));
        }

        let fixed_timers: [(f64, &str, &str); 4] = [
            (GRUB_SPAWNS[0], "grubs-1", "巢虫将在30秒后刷新,考虑呼叫打野控虫"),
            (GRUB_SPAWNS[1], "grubs-2", "第二组巢虫将在30秒后刷新"),
            (HERALD_SPAWN, "herald", "峡谷先锋将在30秒后刷新,提前站位"),
            (BARON_SPAWN, "baron", "男爵将在30秒后刷新,注意排眼控视野"),
        ];
        for (spawn, key, text) in fixed_timers {
            if let Some(line) = Self::announce_timer(
                &mut self.announced,
                snapshot.game_time,
                spawn,
                key,
                text.to_string(),
            ) {
                out.push(Reminder::timer(line));
            }
        }

        if !self.seeded {
            self.seeded = true;
            out.clear();
        }

        out
    }
}

// ---------------------------------------------------------------------------
//  Gameflow fallback prompt (when neither live data nor champ select is up)
// ---------------------------------------------------------------------------

pub fn build_gameflow_prompt(
    session: &Value,
    champions: &ChampionsMap,
    names: &StaticNames,
) -> anyhow::Result<String> {
    let game_data = session
        .get("gameData")
        .context("gameflow session missing gameData")?;

    let phase = session
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("未知");

    let render_team = |key: &str| -> Vec<String> {
        game_data
            .get(key)
            .and_then(Value::as_array)
            .map(|team| {
                team.iter()
                    .filter_map(|member| member.get("championId").and_then(Value::as_i64))
                    .filter(|id| *id > 0)
                    .map(|id| {
                        champ_zh_by_id(id, champions, names)
                            .unwrap_or_else(|| format!("英雄{id}"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let team_one = render_team("teamOne");
    let team_two = render_team("teamTwo");

    if team_one.is_empty() && team_two.is_empty() {
        bail!("gameflow session contains no team data");
    }

    let queue_name = game_data
        .get("queue")
        .and_then(|q| q.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");

    Ok(format!(
        "对局阶段: {phase}{queue_part}(对局数据暂不可用,仅游戏流程信息,无KDA/装备/资源)\n我方: {}\n敌方: {}",
        team_one.join(","),
        team_two.join(","),
        queue_part = if queue_name.is_empty() {
            String::new()
        } else {
            format!(" 模式:{queue_name}")
        }
    ))
}

// ---------------------------------------------------------------------------
//  Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builds::{Block, Counters, Item, Matchup, Rune};
    use crate::web::ChampInfo;
    use serde_json::json;

    fn test_champions() -> ChampionsMap {
        let mut map = ChampionsMap::new();
        // (alias, key, name, tags, attack, magic) — profiles drive counter rules.
        for (alias, key, name, tags, attack, magic) in [
            ("Aatrox", "266", "Aatrox", vec!["Fighter"], 8, 3),
            ("Zed", "238", "Zed", vec!["Assassin"], 9, 1),
            ("Zoe", "142", "Zoe", vec!["Mage"], 2, 8),
            ("Nami", "267", "Nami", vec!["Support", "Mage"], 3, 8),
        ] {
            map.insert(
                alias.to_string(),
                ChampInfo {
                    id: alias.to_string(),
                    key: key.to_string(),
                    name: name.to_string(),
                    tags: tags.iter().map(|s| s.to_string()).collect(),
                    info: crate::web::ChampInfoStats {
                        attack,
                        defense: 0,
                        magic,
                        difficulty: 0,
                    },
                    ..ChampInfo::default()
                },
            );
        }
        map
    }

    fn test_names() -> StaticNames {
        StaticNames {
            champions_cn: HashMap::from([
                ("266".to_string(), "暗裔剑魔".to_string()),
                ("238".to_string(), "影流之主".to_string()),
                ("142".to_string(), "佐伊".to_string()),
                ("267".to_string(), "唤潮鲛姬".to_string()),
            ]),
            runes_cn: HashMap::from([
                (8000, "精密".to_string()),
                (8100, "主宰".to_string()),
                (8200, "巫术".to_string()),
                (8400, "坚决".to_string()),
                (8010, "征服者".to_string()),
                (8009, "凯旋".to_string()),
                (8128, "电刑".to_string()),
                (5008, "自适应之力".to_string()),
                (5002, "护甲".to_string()),
            ]),
            items_cn: HashMap::from([
                ("1055".to_string(), "多兰之刃".to_string()),
                ("6692".to_string(), "暗行者之爪".to_string()),
            ]),
        }
    }

    fn sample_live_data() -> Value {
        json!({
            "activePlayer": {
                "abilities": {"Q": {"abilityLevel": 3}, "W": {"abilityLevel": 1}, "E": {"abilityLevel": 2}, "R": {"abilityLevel": 1}},
                "championStats": {"attackDamage": 187.0, "abilityPower": 0.0, "armor": 64.0, "magicResist": 30.0, "attackSpeed": 0.85, "critChance": 0.0, "currentHealth": 1426.0, "maxHealth": 1610.0, "moveSpeed": 345.0},
                "currentGold": 2233.4,
                "fullRunes": {
                    "keystone": {"displayName": "Conqueror", "id": 8010},
                    "generalRunes": [{"displayName": "Triumph", "id": 8009}],
                    "primaryRuneTree": {"id": 8000},
                    "secondaryRuneTree": {"id": 8400},
                    "statRunes": [{"id": 5008}, {"id": 5002}]
                },
                "level": 8,
                "summonerName": "Me"
            },
            "allPlayers": [
                {
                    "championName": "Aatrox", "isDead": false, "items": [{"itemID": 1055, "count": 1}],
                    "level": 8, "position": "TOP", "respawnTimer": 0.0,
                    "runes": {"keystone": {"id": 8010}, "primaryRuneTree": {"id": 8000}, "secondaryRuneTree": {"id": 8400}},
                    "scores": {"assists": 0, "creepScore": 64, "deaths": 1, "kills": 3, "wardScore": 4.0},
                    "summonerName": "Me",
                    "summonerSpells": {"summonerSpellOne": {"displayName": "Flash"}, "summonerSpellTwo": {"displayName": "Teleport"}},
                    "team": "ORDER"
                },
                {
                    "championName": "Zed", "isDead": false, "items": [{"itemID": 6692, "count": 1}],
                    "level": 9, "position": "TOP", "respawnTimer": 0.0,
                    "runes": {"keystone": {"id": 8010}, "primaryRuneTree": {"id": 8000}, "secondaryRuneTree": {"id": 8100}},
                    "scores": {"assists": 1, "creepScore": 80, "deaths": 2, "kills": 4, "wardScore": 2.0},
                    "summonerName": "Foe",
                    "summonerSpells": {"summonerSpellOne": {"displayName": "Flash"}, "summonerSpellTwo": {"displayName": "Ignite"}},
                    "team": "CHAOS"
                }
            ],
            "events": {"Events": [
                {"EventName": "FirstBlood", "Recipient": "Foe"},
                {"EventName": "ChampionKill", "EventTime": 300.0, "KillerName": "Me", "VictimName": "Foe"},
                {"EventName": "DragonKill", "EventTime": 320.0, "KillerName": "Me", "DragonType": "Infernal"},
                {"EventName": "HordeKill", "EventTime": 400.0, "KillerName": "Foe"},
                {"EventName": "TurretKilled", "EventTime": 420.0, "KillerName": "Me"},
                {"EventName": "BaronKill", "EventTime": 500.0, "KillerName": "Foe"}
            ]},
            "gameData": {"gameMode": "CLASSIC", "gameTime": 754.4, "mapName": "Map11"}
        })
    }

    /// 回归: 「对局数据」表格里**每个玩家**(队友/对手)的胜率列都要有值。
    /// 用户 2026-10-05 报"队友的英雄胜率拿不到", 根因有两个:
    ///   1) 选人档案在"进游戏"那一刻被清空 → 段位/胜率全变 "-"
    ///   2) 对局中只为本地英雄拉 OP.GG 数据 → 别人根本没有数据来源
    /// 这个测试锁住第 2 条: 即使选人档案是空的, 也要按各自的英雄把胜率填上。
    #[test]
    fn live_table_fills_win_rate_for_every_player_without_roster() {
        let champions = test_champions();
        let names = test_names();
        let data = sample_live_data();

        let section = |alias: &str, champ_id: i64, position: &str, rate: &str, games: i64| {
            (
                champ_id,
                vec![crate::builds::BuildSection {
                    alias: alias.to_string(),
                    name: alias.to_string(),
                    position: position.to_string(),
                    win_rate: rate.to_string(),
                    pick_count: games,
                    ..Default::default()
                }],
            )
        };
        // 本地 Aatrox(266, TOP) 与对手 Zed(238, TOP)
        let sections: HashMap<i64, Vec<crate::builds::BuildSection>> =
            HashMap::from([section("aatrox", 266, "top", "47.83%", 5528), section("zed", 238, "top", "51.21%", 5058)]);

        // 关键: 档案为空(模拟"app 在对局中才启动"/缓存被清)
        let table = build_live_table(&data, &champions, &names, &[], &sections).expect("live table");

        let player_rows: Vec<&TableRow> = table
            .rows
            .iter()
            .filter(|row| row.section.is_empty())
            .collect();
        assert_eq!(player_rows.len(), 2, "两名玩家各一行");
        for row in &player_rows {
            // 列序: 位/英雄/召唤师/段位/胜率/KDA/补刀/等级/基石/装备
            assert_ne!(
                row.cells[5], "-",
                "英雄胜率列不能为空: {:?}",
                row.cells
            );
        }
        let rates: Vec<&str> = player_rows.iter().map(|row| row.cells[5].as_str()).collect();
        assert!(rates.contains(&"47.83%"), "本地英雄胜率来自 OP.GG: {rates:?}");
        assert!(rates.contains(&"51.21%"), "对手英雄胜率同样要有: {rates:?}");
    }

    #[test]
    fn live_prompt_contains_rich_state() {
        let champions = test_champions();
        let names = test_names();
        let prompt = build_live_game_prompt(
            &sample_live_data(),
            &champions,
            &names,
            None,
            0,
            &crate::tips::PlaystyleAtlas::default(),
        )
        .unwrap();

        // 对位 + KDA + 补刀
        assert!(prompt.contains("对位: 敌方上单 影流之主(Lv9, 4/2/1, 80刀"));
        // 本机金币 + 加点 + 面板
        assert!(prompt.contains("金币2233"));
        assert!(prompt.contains("Q3W1E2R1"));
        // 中文符文名(来自静态名表)
        assert!(prompt.contains("本机符文: 征服者,凯旋,自适应之力,护甲"));
        // 资源与事件
        assert!(prompt.contains("火龙"));
        assert!(prompt.contains("男爵1"));
        assert!(prompt.contains("近期击杀"));
        assert!(prompt.contains("一血"));
        // 中文英雄名 + 中文技能名 + 中文装备
        assert!(prompt.contains("暗裔剑魔(上单, Lv8, 3/1/0, 64刀, 征服者+精密, 闪现/传送, 装备:多兰之刃)"));
        assert!(prompt.contains("暗行者之爪"));
    }

    #[test]
    fn lineup_prompt_contains_matchup_and_runes() {
        let champions = test_champions();
        let names = test_names();

        let session = json!({
            "localPlayerCellId": 0,
            "bans": {"myTeamBans": [157], "theirTeamBans": [142]},
            "myTeam": [
                {"cellId": 0, "championId": 266, "assignedPosition": "middle", "summonerId": 11, "puuid": "p-me", "spell1Id": 4, "spell2Id": 14},
                {"cellId": 1, "championId": 267, "assignedPosition": "utility", "summonerId": 22, "puuid": ""}
            ],
            "theirTeam": [
                {"cellId": 5, "championId": 238, "assignedPosition": "middle", "summonerId": 0},
                {"cellId": 6, "championId": 0, "assignedPosition": "top", "summonerId": 33}
            ]
        });

        let rune_page = json!({
            "primaryStyleId": 8100,
            "subStyleId": 8200,
            "selectedPerkIds": [8128, 8139, 8138, 8135, 8226, 8210, 5008, 5008, 5002]
        });

        let mut sections: HashMap<i64, Vec<BuildSection>> = HashMap::new();
        sections.insert(
            266,
            vec![BuildSection {
                position: "middle".to_string(),
                champion_tier: Some("T2".to_string()),
                win_rate: "50.3%".to_string(),
                pick_count: 12345,
                runes: vec![Rune {
                    name: "标准符文".to_string(),
                    position: "middle".to_string(),
                    pick_count: 2345,
                    win_rate: "52.1%".to_string(),
                    primary_style_id: 8000,
                    sub_style_id: 8400,
                    selected_perk_ids: vec![8010, 8009, 9104, 8299, 8451, 8453, 5008, 5002],
                    ..Rune::default()
                }],
                item_builds: vec![crate::builds::ItemBuild {
                    title: "标准出装".to_string(),
                    blocks: vec![Block {
                        type_field: "starter".to_string(),
                        items: Some(vec![Item { id: "1055".to_string(), count: 1 }]),
                    }],
                    ..crate::builds::ItemBuild::default()
                }],
                skills: Some(vec!["Q".to_string(), "W".to_string(), "E".to_string()]),
                ..BuildSection::default()
            }],
        );
        sections.insert(
            238,
            vec![BuildSection {
                position: "mid".to_string(),
                champion_tier: Some("T1".to_string()),
                win_rate: "51.2%".to_string(),
                pick_count: 5678,
                runes: vec![Rune {
                    position: "mid".to_string(),
                    pick_count: 1000,
                    win_rate: "51.8%".to_string(),
                    primary_style_id: 8100,
                    sub_style_id: 8200,
                    selected_perk_ids: vec![8128],
                    ..Rune::default()
                }],
                ..BuildSection::default()
            }],
        );

        let ranks = HashMap::from([(
            11,
            RankInfo {
                rank: "黄金II".to_string(),
                personal_rate: "55%".to_string(),
                personal_record: "120胜98负".to_string(),
            },
        )]);

        let atlas = crate::tips::PlaystyleAtlas {
            champions: std::collections::HashMap::from([(
                "238".to_string(),
                crate::tips::ChampIntel {
                    title: "影流之主".to_string(),
                    blurb: String::new(),
                    ally_tips: vec!["劫依赖三级连招起手, 没有W时近乎无害".to_string()],
                    enemy_tips: vec!["影流之主的影分身有空当, 趁放量期走位压制".to_string()],
                },
            )]),
        };

        let prompt = build_lineup_prompt(
            &session,
            &champions,
            &names,
            Some(&rune_page),
            &ranks,
            &sections,
            &atlas,
        )
        .unwrap();
        // 对位心理已注入(意图/软肋带标签)
        assert!(prompt.contains("(意图)劫依赖三级连招起手"));
        assert!(prompt.contains("(软肋)影流之主的影分身有空当"));

        println!("{prompt}");
        // 对位行
        assert!(prompt.contains("对位: 敌方中单 影流之主"));
        // 段位与召唤师技能
        assert!(prompt.contains("暗裔剑魔(中单, 单双黄金II"), "段位应出现在 prompt");
        assert!(prompt.contains("闪现/点燃"), "召唤师技能应出现在 prompt");
        // 当前符文页中文名
        assert!(prompt.contains("当前符文页: 主宰+巫术: 电刑"));
        // OP.GG 细节: 梯度/胜率/符文/装备
        assert!(prompt.contains("梯度T2 胜率50.3%"));
        assert!(prompt.contains("符文1(选用2345,胜率52.1%): 征服者,凯旋(精密)"));
        assert!(prompt.contains("技能加点: Q>W>E"));
        assert!(prompt.contains("标准出装: [starter]多兰之刃"));
        // 对位英雄摘要
        assert!(prompt.contains("影流之主·中单: 梯度T1 胜率51.2% 样本5678场 常用基石:电刑"));
        // ban 列表(敌方 ban 了 142 佐伊)
        assert!(prompt.contains("敌方[佐伊]"));
    }

    #[test]
    fn live_panel_renders_score_objectives_and_matchup() {
        let champions = test_champions();
        let names = test_names();
        let panel = build_live_panel_text(&sample_live_data(), &champions, &names).unwrap();

        assert!(panel.contains("对局中 12:34 | 比分 我方3 vs 敌方4"));
        assert!(panel.contains("资源 我方[龙:火龙 巢虫0 先锋0 男爵0 塔1 水晶0]"));
        assert!(panel.contains("对位: 暗裔剑魔 Lv8 3/1/0 64刀  vs  影流之主 Lv9 4/2/1 80刀"));
        assert!(panel.contains("金币 2233 | 加点 Q3W1E2R1 | 符文 征服者,凯旋,自适应之力,护甲"));
        assert!(panel.contains("暗裔剑魔 Lv8 上单 | 3/1/0 | 64刀 | 征服者 | 闪现/传送 | 多兰之刃"));
        assert!(panel.contains("近期击杀"));
    }

    #[test]
    fn champ_select_panel_lists_bans_teams_and_matchup() {
        let champions = test_champions();
        let names = test_names();
        let session = json!({
            "localPlayerCellId": 0,
            "bans": {"myTeamBans": [157], "theirTeamBans": [142]},
            "myTeam": [
                {"cellId": 0, "championId": 266, "assignedPosition": "middle", "summonerId": 11},
                {"cellId": 1, "championId": 267, "assignedPosition": "utility", "summonerId": 22}
            ],
            "theirTeam": [
                {"cellId": 5, "championId": 238, "assignedPosition": "middle", "summonerId": 0},
                {"cellId": 6, "championId": 0, "assignedPosition": "top", "summonerId": 33}
            ]
        });
        let ranks = HashMap::from([(
            11,
            RankInfo {
                rank: "荣耀黄金II".to_string(),
                personal_rate: "55%".to_string(),
                personal_record: "120胜98负".to_string(),
            },
        )]);
        // Local champ (Aatrox) fields counters: panel should surface ban advice.
        let sections: HashMap<i64, Vec<BuildSection>> =
            HashMap::from([(266, vec![aatrox_section_with_counters()])]);

        let panel =
            build_champ_select_panel_text(&session, &champions, &names, &ranks, &sections).unwrap();

        assert!(panel.contains("ban 我方[] 敌方[佐伊]"));
        assert!(panel.contains("暗裔剑魔(中单 单双荣耀黄金II"), "段位+个人战绩应出现在选人面板");
        assert!(panel.contains("影流之主(中单)"));
        assert!(!panel.contains("本机位置: 中单 | 对位: 敌方中单 佐伊")); // 敌方中单是影流之主
        assert!(panel.contains("本机位置: 中单 | 对位: 敌方中单 影流之主"));
        // Ban suggestion derived from counters (Zed 46.47%/340场, Nami 60% filtered >50%).
        assert!(panel.contains("Ban位参考(你最难打的对手): 影流之主 46.47%(340场)"));
    }

    fn live_snapshot_with(game_time: f64, events: Value) -> LiveSnapshot {
        let mut data = sample_live_data();
        data["gameData"]["gameTime"] = json!(game_time);
        data["events"]["Events"] = events;
        LiveSnapshot::from_all_game_data(&data).unwrap()
    }

    #[test]
    fn reminder_engine_seeds_then_announces_new_events() {
        let champions = test_champions();
        let names = test_names();
        let mut engine = ReminderEngine::new();

        // Attach mid-game at 754s: existing events are seeded silently.
        let first = engine.collect(
            &live_snapshot_with(
                754.4,
                json!([
                    {"EventName": "FirstBlood", "EventTime": 150.0, "Recipient": "Foe"},
                    {"EventName": "DragonKill", "EventTime": 320.0, "KillerName": "Me", "DragonType": "Infernal"},
                    {"EventName": "BaronKill", "EventTime": 600.0, "KillerName": "Foe"}
                ]),
            ),
            &champions,
            &names,
        );
        assert!(first.is_empty());

        // A new dragon kill by the local team is announced exactly once.
        let second = engine.collect(
            &live_snapshot_with(
                782.0,
                json!([
                    {"EventName": "FirstBlood", "EventTime": 150.0, "Recipient": "Foe"},
                    {"EventName": "DragonKill", "EventTime": 320.0, "KillerName": "Me", "DragonType": "Infernal"},
                    {"EventName": "BaronKill", "EventTime": 600.0, "KillerName": "Foe"},
                    {"EventName": "DragonKill", "EventTime": 780.0, "KillerName": "Me", "DragonType": "Ocean"}
                ]),
            ),
            &champions,
            &names,
        );
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "我方击杀水龙");
        assert_eq!(second[0].kind, ReminderKind::EventKey);

        // Same snapshot again: nothing new.
        let third = engine.collect(
            &live_snapshot_with(
                784.0,
                json!([
                    {"EventName": "FirstBlood", "EventTime": 150.0, "Recipient": "Foe"},
                    {"EventName": "DragonKill", "EventTime": 320.0, "KillerName": "Me", "DragonType": "Infernal"},
                    {"EventName": "BaronKill", "EventTime": 600.0, "KillerName": "Foe"},
                    {"EventName": "DragonKill", "EventTime": 780.0, "KillerName": "Me", "DragonType": "Ocean"}
                ]),
            ),
            &champions,
            &names,
        );
        assert!(third.is_empty());
    }

    #[test]
    fn reminder_engine_announces_grub_burst_once_and_timers() {
        let champions = test_champions();
        let names = test_names();
        let mut engine = ReminderEngine::new();

        // Seed at 200s.
        let _ = engine.collect(&live_snapshot_with(200.0, json!([])), &champions, &names);

        // 275s: next dragon spawns at 300 -> pre-spawn reminder fires.
        let reminders = engine.collect(&live_snapshot_with(275.0, json!([])), &champions, &names);
        assert!(reminders.iter().any(|r| {
            r.kind == ReminderKind::Timer && r.text == "小龙将在30秒后刷新,提前集合布眼"
        }));

        // Same moment again: no duplicate.
        let again = engine.collect(&live_snapshot_with(276.0, json!([])), &champions, &names);
        assert!(!again.iter().any(|r| r.text == "小龙将在30秒后刷新,提前集合布眼"));

        // Grubs killed as a burst of 3 -> one announcement only (enemy side).
        engine.reset();
        let _ = engine.collect(&live_snapshot_with(400.0, json!([])), &champions, &names);
        let burst = engine.collect(
            &live_snapshot_with(
                500.0,
                json!([
                    {"EventName": "HordeKill", "EventTime": 370.0, "KillerName": "Foe"},
                    {"EventName": "HordeKill", "EventTime": 371.0, "KillerName": "Foe"},
                    {"EventName": "HordeKill", "EventTime": 372.0, "KillerName": "Foe"}
                ]),
            ),
            &champions,
            &names,
        );
        assert_eq!(
            burst
                .iter()
                .filter(|r| r.kind == ReminderKind::EventMinor && r.text.contains("敌方拿下巢虫"))
                .count(),
            1
        );
    }

    #[test]
    fn reminder_engine_resets_on_new_game() {
        let champions = test_champions();
        let names = test_names();
        let mut engine = ReminderEngine::new();

        let _ = engine.collect(
            &live_snapshot_with(
                754.4,
                json!([{"EventName": "FirstBlood", "EventTime": 150.0, "Recipient": "Foe"}]),
            ),
            &champions,
            &names,
        );

        // New game: time jumped back -> engine re-seeds, no stale announcements.
        let out = engine.collect(
            &live_snapshot_with(
                120.0,
                json!([{"EventName": "FirstBlood", "EventTime": 110.0, "Recipient": "Me"}]),
            ),
            &champions,
            &names,
        );
        assert!(out.is_empty());
    }

    fn aatrox_section_with_counters() -> crate::builds::BuildSection {
        crate::builds::BuildSection {
            alias: "aatrox".to_string(),
            counters: Some(Counters {
                position: "top".to_string(),
                matchups: vec![
                    Matchup {
                        champion_id: 238,
                        champion_key: "zed".to_string(),
                        win_rate: "46.47%".to_string(),
                        play: 340,
                    },
                    Matchup {
                        // Ids unknown after a single-champion crawl: key acts as fallback.
                        champion_id: 0,
                        champion_key: "nami".to_string(),
                        win_rate: "60.00%".to_string(),
                        play: 320,
                    },
                ],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn direct_matchup_reads_local_counters() {
        let champions = test_champions();
        let map: HashMap<i64, Vec<BuildSection>> =
            HashMap::from([(266, vec![aatrox_section_with_counters()])]);

        // Zed (238): below-50% win rate -> declared a losing matchup.
        let line = find_direct_matchup(266, "top", 238, "影流之主", &map, &champions).unwrap();
        assert!(line.contains("46.47%"), "{line}");
        assert!(line.contains("(340场)"), "{line}");
        assert!(line.contains("劣势对位"), "{line}");

        // Nami entry has no id; matched via the OP.GG key; >50% -> advantage.
        let line = find_direct_matchup(266, "top", 267, "唤潮鲛姬", &map, &champions).unwrap();
        assert!(line.contains("优势对位"), "{line}");

        // Unknown opponent -> no line instead of a wrong guess.
        assert!(find_direct_matchup(266, "top", 142, "佐伊", &map, &champions).is_none());
    }

    #[test]
    fn ban_suggestions_ranks_hardest_matchups() {
        let champions = test_champions();
        let names = test_names();
        let map: HashMap<i64, Vec<BuildSection>> =
            HashMap::from([(266, vec![aatrox_section_with_counters()])]);

        let line = ban_suggestions(266, &map, &champions, &names, 3).unwrap();
        // Only losing matchups with enough games; sorted by win rate ascending.
        assert!(line.contains("影流之主 46.47%(340场)"), "{line}");
        assert!(line.contains("Ban位参考"), "{line}");
        assert!(!line.contains("60.00%"), "{line}"); // Nami: winning, not a ban target
        // No data for unknown champion -> None.
        assert!(ban_suggestions(238, &map, &champions, &names, 3).is_none());
    }

    #[test]
    fn direct_matchup_mirrors_opponent_counters() {
        let champions = test_champions();
        let zed_section = BuildSection {
            alias: "zed".to_string(),
            ..Default::default()
        };
        let map: HashMap<i64, Vec<BuildSection>> = HashMap::from([
            (238, vec![zed_section]),
            (266, vec![aatrox_section_with_counters()]),
        ]);

        // Aatrox's list shows 46.47% vs Zed -> Zed's win rate is 100-46.47 = 53.53%.
        let line = find_direct_matchup(238, "top", 266, "暗裔剑魔", &map, &champions).unwrap();
        assert!(line.contains("53.53%"), "{line}");
        assert!(line.contains("优势对位"), "{line}");
        assert!(line.contains("按对方数据换算"), "{line}");
    }

    fn aatrox_counter_rune_sections() -> HashMap<i64, Vec<BuildSection>> {
        let mut section = aatrox_section_with_counters();
        // Base page: precision + inspiration shards (inspiration sub gets swapped out).
        section.runes = vec![Rune {
            position: "top".to_string(),
            primary_style_id: 8000,
            sub_style_id: 8300,
            selected_perk_ids: vec![8010, 9111, 9105, 8299, 8345, 8347, 5008, 5008, 5001],
            ..Rune::default()
        }];
        HashMap::from([(266, vec![section])])
    }

    #[test]
    fn counter_note_describes_rule_engine_plan() {
        let champions = test_champions();
        let map = aatrox_counter_rune_sections();

        // Aatrox vs Zed (Assassin, AD): 46.47%/340场 -> 劣势 + 抗压刺客规则.
        let note = build_counter_note(266, "top", 238, &map, &champions).unwrap();
        assert!(note.contains("刺客"), "{note}");
        assert!(note.contains("物理"), "{note}");
        assert!(note.contains("劣势"), "{note}");
        assert!(note.contains("副系坚决"), "{note}");
        assert!(note.contains("骸骨镀层+过度生长"), "{note}");
        assert!(note.contains("防御碎片: 护甲"), "{note}");
        // 差异取舍说明: 原副系启迪 神奇之鞋→骸骨镀层 etc.
        assert!(note.contains("副系第1格: 饼干配送→骸骨镀层"), "{note}");
        assert!(note.contains("防御碎片: 成长生命→护甲"), "{note}");

        // Nami winning lane (优势/辅助·AP): 保持副系, 只调防御碎片为魔抗
        let win_note = build_counter_note(266, "top", 267, &map, &champions).unwrap();
        assert!(win_note.contains("优势"), "{win_note}");
        assert!(win_note.contains("防御碎片: 魔抗"), "{win_note}");
        assert!(!win_note.contains("副系坚决"), "{win_note}");

        // Unknown opponent -> None.
        assert!(build_counter_note(266, "top", 142, &map, &champions).is_none());
    }

    #[test]
    fn counter_note_skips_when_mainstream_page_already_matches() {
        let champions = test_champions();
        let mut section = aatrox_section_with_counters();
        // Second mainstream page is exactly the counter page (骸骨镀层, 过度生长, 护甲).
        section.runes = vec![
            Rune {
                position: "top".to_string(),
                primary_style_id: 8000,
                sub_style_id: 8300,
                selected_perk_ids: vec![8010, 9111, 9105, 8299, 8345, 8347, 5008, 5008, 5001],
                ..Rune::default()
            },
            Rune {
                position: "top".to_string(),
                primary_style_id: 8000,
                sub_style_id: 8400,
                selected_perk_ids: vec![8010, 9111, 9105, 8299, 8473, 8451, 5008, 5008, 5002],
                ..Rune::default()
            },
        ];
        let map = HashMap::from([(266, vec![section])]);

        // CP.Zed 刺客/AD 劣势 -> 方案与主流页第2页完全一致 -> 不再另行推荐。
        assert!(build_counter_note(266, "top", 238, &map, &champions).is_none());
    }

    #[test]
    fn gameflow_fallback_lists_teams() {
        let champions = test_champions();
        let names = test_names();
        let session = json!({
            "phase": "InProgress",
            "gameData": {
                "queue": {"name": "召唤师峡谷"},
                "teamOne": [{"championId": 266}, {"championId": 267}],
                "teamTwo": [{"championId": 238}, {"championId": 142}]
            }
        });

        let prompt = build_gameflow_prompt(&session, &champions, &names).unwrap();
        assert!(prompt.contains("我方: 暗裔剑魔,唤潮鲛姬"));
        assert!(prompt.contains("敌方: 影流之主,佐伊"));
    }

    /// 复制出来的表格文本必须列对齐: 中文按 2 格宽, 所以每行的列起始位置一致。
    #[test]
    fn table_text_columns_line_up() {
        let row = |champ: &str, name: &str, kda: &str, mine: bool| TableRow {
            section: String::new(),
            cells: vec![
                "中".to_string(),
                champ.to_string(),
                name.to_string(),
                "铂金 II".to_string(),
                "55%".to_string(),      // 个人胜率
                "51.2%".to_string(),    // 英雄胜率
                kda.to_string(),
                "180".to_string(),
                "11".to_string(),
                "电刑".to_string(),
                "暗影阔剑".to_string(),
            ],
            mine_team: true,
            mine,
            opponent: !mine,
        };

        let table = DataTable {
            summary: "对局中 18:32 · 比分 12:9".to_string(),
            sub_lines: vec!["我方  龙 火".to_string(), "敌方  龙 土".to_string()],
            columns: columns_of(&MATCH_COLUMNS),
            rows: vec![
                TableRow {
                    section: "我方".to_string(),
                    ..Default::default()
                },
                row("阿卡丽", "短名", "8/2/3", true),
                TableRow {
                    section: "敌方".to_string(),
                    ..Default::default()
                },
                row("劫", "一个比较长的召唤师名字", "6/1/2", false),
            ],
            notes: vec!["我的金币 8420".to_string()],
            footnote: "我 阿卡丽 vs 劫 · 补刀 +8".to_string(),
        };

        let text = render_table_text(&table);
        eprintln!("TABLE-DEBUG
{text}");
        let header = text
            .lines()
            .find(|l| l.trim_start().starts_with('位'))
            .expect("表头存在");
        let my_row = text
            .lines()
            .find(|l| l.contains("阿卡丽"))
            .expect("我方行存在");
        let foe_row = text
            .lines()
            .find(|l| l.contains("劫"))
            .expect("敌方行存在");

        // 表头/两行数据的"补刀"列起始位置必须相同(前面 4 列宽度固定)
        let column_start = |line: &str, needle: &str| -> usize {
            let byte_index = line.find(needle).expect("列存在");
            display_width(&line[..byte_index])
        };
        assert_eq!(column_start(header, "补刀"), column_start(my_row, "180"));
        assert_eq!(column_start(header, "补刀"), column_start(foe_row, "180"));
        // 我 = ★, 对位 = ▲, 一眼能认出
        assert!(my_row.trim_start().starts_with('★'));
        assert!(foe_row.trim_start().starts_with('▲'));
    }

    /// 对局里把选人档案配回玩家: 分路优先, 其次是英雄, 最后是召唤师名。
    /// 任何一层缺失都会让队友的"段位/胜率"变成 "-"(用户 2026-10-05 报障的根因)。
    #[test]
    fn roster_matching_falls_back_position_then_champion_then_name() {
        let entry = |pos: &str, champ: i64, name: &str, my_team: bool| RosterEntry {
            position: pos.to_string(),
            position_label: position_label(pos),
            champion_id: champ,
            champion: format!("英雄{champ}"),
            summoner: name.to_string(),
            rank: "铂金 II".to_string(),
            personal_rate: "55%".to_string(),
            personal_record: "120胜98负".to_string(),
            win_rate: "51.2%".to_string(),
            games: "1240".to_string(),
            mine_team: my_team,
            mine: false,
            opponent: false,
        };
        let roster = vec![
            entry("middle", 103, "我方中单", true),
            entry("top", 266, "我方上单", true),
            entry("middle", 238, "敌方中单", false),
            entry("top", 64, "敌方上单", false),
        ];

        // 1) 同队 + 分路命中(即使英雄不同 —— 换英雄/被抢线也要能配上)
        let hit = find_roster(&roster, "middle", 0, "", true).expect("my team middle");
        assert_eq!(hit.rank, "铂金 II");
        assert_eq!(hit.summoner, "我方中单");
        // 关键: 对位同分路必须命中**另一份**档案
        let foe = find_roster(&roster, "middle", 0, "", false).expect("enemy middle");
        assert_eq!(foe.summoner, "敌方中单");
        assert_ne!(hit.summoner, foe.summoner, "我和对位不能是同一份档案");

        // 2) 分路不可用(盲选) → 按英雄 id(同样分队伍)
        let hit = find_roster(&roster, "", 266, "", true).expect("champion match");
        assert_eq!(hit.summoner, "我方上单");
        let foe = find_roster(&roster, "", 64, "", false).expect("enemy champion match");
        assert_eq!(foe.summoner, "敌方上单");

        // 3) 分路与英雄都对不上 → 按召唤师名
        let hit = find_roster(&roster, "jungle", 999, "敌方中单", false).expect("name match");
        assert_eq!(hit.champion_id, 238);

        // 4) 三个都不匹配 → None(调用方显示 "-")
        assert!(find_roster(&roster, "jungle", 999, "路人", true).is_none());
    }

    /// 每一行的胜率必须来自**他自己的英雄**, 不能串到别人身上。
    /// 用真实库里的数值(Aatrox 47.83% / Zed 49.95% / Ahri 51.21% / Yasuo 50.32%)
    /// 组成 10 人排位, 逐行核对。用户 2026-10-05 质疑"每个玩家的英雄胜率是假的"。
    #[test]
    fn champ_select_rates_match_each_members_own_champion() {
        let champions = test_champions();
        let names = test_names();

        // 四个英雄的真实 OP.GG 数据(与本地 champion_data 一致)
        let section = |alias: &str, lane: &str, rate: &str, games: i64| crate::builds::BuildSection {
            alias: alias.to_string(),
            name: alias.to_string(),
            position: lane.to_string(),
            win_rate: rate.to_string(),
            pick_count: games,
            ..Default::default()
        };
        let sections: HashMap<i64, Vec<crate::builds::BuildSection>> = HashMap::from([
            (266, vec![section("aatrox", "top", "47.83%", 5528)]),
            (238, vec![section("zed", "middle", "49.95%", 3079)]),
            (142, vec![section("zoe", "middle", "51.21%", 5058)]),
            (267, vec![section("nami", "support", "52.10%", 5187)]),
        ]);

        let member = |cell: i64, champion: i64, pos: &str, name: &str| {
            serde_json::json!({
                "cellId": cell,
                "championId": champion,
                "championPickIntent": 0,
                "assignedPosition": pos,
                "summonerId": cell * 10,
                "puuid": format!("puuid-{cell}"),
                "displayName": name,
            })
        };
        let session = serde_json::json!({
            "localPlayerCellId": 0,
            "myTeam": [
                member(0, 142, "middle", "我"),
                member(1, 266, "top", "队友上单"),
            ],
            "theirTeam": [
                member(5, 238, "middle", "敌方中单"),
                member(6, 267, "support", "敌方辅助"),
            ],
            "bans": {"myTeamBans": [], "theirTeamBans": []},
        });

        let (table, roster) = build_champ_select_table(
            &session,
            &champions,
            &names,
            &HashMap::new(),
            &sections,
        )
        .expect("table");

        // 档案里每个英雄配到的必须是自己的胜率
        let rate_of = |champion_id: i64| -> String {
            roster
                .iter()
                .find(|entry| entry.champion_id == champion_id)
                .map(|entry| entry.win_rate.clone())
                .unwrap_or_default()
        };
        assert_eq!(rate_of(142), "51.21%", "佐伊");
        assert_eq!(rate_of(266), "47.83%", "亚托克斯");
        assert_eq!(rate_of(238), "49.95%", "劫");
        assert_eq!(rate_of(267), "52.10%", "唤潮鲛姬");

        // 表格里每一行的胜率列也必须等于该行英雄自己的值(按英雄名核对)
        let rate_for_row = |champion: &str| -> String {
            table
                .rows
                .iter()
                .find(|row| row.section.is_empty() && row.cells[1] == champion)
                .map(|row| row.cells[5].clone()) // 英雄胜率列
                .unwrap_or_else(|| panic!("缺少 {champion} 这一行"))
        };
        assert_eq!(rate_for_row("佐伊"), "51.21%");
        assert_eq!(rate_for_row("暗裔剑魔"), "47.83%");
        assert_eq!(rate_for_row("影流之主"), "49.95%");
        assert_eq!(rate_for_row("唤潮鲛姬"), "52.10%");
    }

    /// 回归: 选人后换了英雄(交易), 对局里显示的胜率必须是**现在这个英雄**的,
    /// 不能沿用选人档案里那个旧英雄的胜率 —— 用户 2026-10-05:
    /// "你拿的每个玩家的当前使用的英雄的胜率是假的"。
    #[test]
    fn traded_champion_uses_the_live_champions_win_rate() {
        let champions = test_champions();
        let names = test_names();
        let mut data = sample_live_data();
        // 我把亚托克斯换成了劫(交易): Live 数据里我的英雄变成 Zed
        data["allPlayers"][0]["championName"] = serde_json::json!("Zed");

        let section = |alias: &str, lane: &str, rate: &str| crate::builds::BuildSection {
            alias: alias.to_string(),
            name: alias.to_string(),
            position: lane.to_string(),
            win_rate: rate.to_string(),
            pick_count: 4000,
            ..Default::default()
        };
        let sections: HashMap<i64, Vec<crate::builds::BuildSection>> = HashMap::from([
            (266, vec![section("aatrox", "top", "47.83%")]),
            (238, vec![section("zed", "top", "49.95%")]),
        ]);

        // 档案里记的还是选人时的亚托克斯(47.83% 之外的旧值), 段位与英雄无关
        let roster = vec![RosterEntry {
            position: "top".to_string(),
            position_label: position_label("top"),
            champion_id: 266,
            champion: "暗裔剑魔".to_string(),
            summoner: "Me".to_string(),
            rank: "黄金 II".to_string(),
            personal_rate: "55%".to_string(),
            personal_record: "120胜98负".to_string(),
            win_rate: "40.00%".to_string(), // 旧英雄的胜率: 绝不能被采用
            games: "1".to_string(),
            mine_team: true,
            mine: true,
            opponent: false,
        }];

        let table =
            build_live_table(&data, &champions, &names, &roster, &sections).expect("table");
        let my_row = table
            .rows
            .iter()
            .find(|row| row.mine)
            .expect("my row");

        assert_eq!(my_row.cells[1], "影流之主", "英雄列应是交易后的劫");
        assert_eq!(my_row.cells[3], "黄金 II", "段位与英雄无关, 仍来自档案");
        assert_eq!(
            my_row.cells[5], "49.95%",
            "英雄胜率必须按当前英雄(劫)算, 而不是档案里旧英雄的 40.00%"
        );
        assert_eq!(my_row.cells[4], "55%", "个人胜率与英雄无关, 仍是 55%");
    }

    /// 回归: 我与对位同分路时, 两行必须各查各的档案 —— 不能显示成一样的胜率/段位。
    /// 用户 2026-10-05 的原话是"对面跟我一模一样的胜率"。
    #[test]
    fn lane_opponent_never_borrows_my_roster_row() {
        let champions = test_champions();
        let names = test_names();
        // 样本数据里 Me=Aatrox(ORDER) 与 Foe=Zed(CHAOS) 都是 TOP 分路
        let data = sample_live_data();

        let the_row = |mine_team: bool, rank: &str, rate: &str, champ: i64, name: &str| RosterEntry {
            position: "top".to_string(),
            // 必须是真实标签("上单", 而不是"上"): 标签写错会让分路分支整体落空,
            // 测试就测不到队伍区分这条逻辑(写这个测试时踩过一次)。
            position_label: position_label("top"),
            champion_id: champ,
            champion: format!("英雄{champ}"),
            summoner: name.to_string(),
            rank: rank.to_string(),
            personal_rate: "55%".to_string(),
            personal_record: "120胜98负".to_string(),
            win_rate: rate.to_string(),
            games: "1000".to_string(),
            mine_team,
            mine: mine_team,
            opponent: !mine_team,
        };
        let roster = vec![
            the_row(true, "黄金 II", "48.00%", 266, "Me"),
            the_row(false, "铂金 I", "53.00%", 238, "Foe"),
        ];

        let table =
            build_live_table(&data, &champions, &names, &roster, &HashMap::new()).expect("table");
        let rows: Vec<&TableRow> = table
            .rows
            .iter()
            .filter(|row| row.section.is_empty())
            .collect();
        let my_row = rows.iter().find(|row| row.mine).expect("my row");
        let foe_row = rows.iter().find(|row| row.opponent).expect("opponent row");

        // 列序: 位/英雄/召唤师/段位/胜率/…
        assert_eq!(my_row.cells[3], "黄金 II");
        assert_eq!(my_row.cells[4], "55%"); // 个人胜率来自各自档案
        assert_eq!(foe_row.cells[3], "铂金 I");
        assert_eq!(foe_row.cells[4], "55%");
        assert_eq!(my_row.cells[5], "48.00%"); // 英雄胜率
        assert_eq!(foe_row.cells[5], "53.00%");
    }

    #[test]
    fn display_width_counts_cjk_double() {
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("阿卡丽"), 6);
        assert_eq!(display_width("阿a"), 3);
        assert_eq!(pad_display("劫", 6), "劫    ");
        // 超宽要截断成省略号, 且截断后仍然占满列宽(否则后面的列会右移)
        assert_eq!(display_width(&pad_display("一个很长的召唤师名", 6)), 6);
        assert!(pad_display("一个很长的召唤师名", 6).contains('…'));
        assert_eq!(display_width(&pad_display("阿卡丽", 4)), 4);
    }
}
