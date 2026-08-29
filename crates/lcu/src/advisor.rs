use anyhow::{Context, bail};
use serde_json::Value;
use std::collections::HashMap;

use crate::web::ChampionsMap;

pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a League of Legends in-game coach.
Read the current team compositions and local player matchup, then provide concise, actionable advice.
Focus on:
1. Lane matchup and trading tips for the local player.
2. Mid-game macro priorities around dragons and Void Grubs.
3. Itemization/positioning advice based on both teams.
Respond in Chinese, keep it under 250 characters, and do not include unrelated fluff."#;

fn champion_name(champion_id: i64, champions: &ChampionsMap) -> String {
    champions
        .values()
        .find(|champion| champion.key == champion_id.to_string())
        .map(|champion| champion.name.clone())
        .unwrap_or_else(|| champion_id.to_string())
}

fn parse_team(team: Option<&Value>, champions: &ChampionsMap) -> Vec<String> {
    let Some(members) = team.and_then(Value::as_array) else {
        return Vec::new();
    };

    members
        .iter()
        .filter_map(|member| {
            let champion_id = member.get("championId")?.as_i64()?;
            let position = member
                .get("assignedPosition")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN");
            let name = champion_name(champion_id, champions);
            Some(format!("{name} ({position})"))
        })
        .collect()
}

pub fn build_lineup_prompt(
    session: &Value,
    champions: &ChampionsMap,
) -> anyhow::Result<String> {
    let my_team = parse_team(session.get("myTeam"), champions);
    let enemy_team = parse_team(session.get("theirTeam"), champions);

    if my_team.is_empty() || enemy_team.is_empty() {
        bail!("champ select session does not contain both teams");
    }

    let local_cell_id = session
        .get("localPlayerCellId")
        .and_then(Value::as_i64)
        .context("champ select session missing localPlayerCellId")?;

    let local_player = session
        .get("myTeam")
        .and_then(Value::as_array)
        .and_then(|members| {
            members
                .iter()
                .find(|member| member.get("cellId").and_then(Value::as_i64) == Some(local_cell_id))
        })
        .map(|member| {
            let champion_id = member.get("championId").and_then(Value::as_i64).unwrap_or(0);
            let position = member
                .get("assignedPosition")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN");
            format!("{} ({})", champion_name(champion_id, champions), position)
        })
        .unwrap_or_else(|| "Unknown local player".to_string());

    Ok(format!(
        "我方阵容:\n{}\n\n敌方阵容:\n{}\n\n本局玩家: {}\n\n请给出对线战斗技巧、装备思路，以及控龙/控虫策略。",
        my_team.join("\n"),
        enemy_team.join("\n"),
        local_player
    ))
}

pub fn build_live_game_prompt(
    game_data: &Value,
    item_names: &HashMap<String, String>,
) -> anyhow::Result<String> {
    let players = game_data
        .get("allPlayers")
        .and_then(Value::as_array)
        .context("Live Client Data missing allPlayers")?;

    if players.is_empty() {
        bail!("Live Client Data allPlayers is empty");
    }

    let mut order_team = Vec::new();
    let mut chaos_team = Vec::new();

    for player in players {
        let champion = player
            .get("championName")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");
        let position = player
            .get("position")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        let level = player
            .get("level")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let items = player
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("itemID").and_then(Value::as_i64))
                    .filter(|item_id| *item_id > 0)
                    .map(|item_id| {
                        let id = item_id.to_string();
                        item_names
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| format!("item {id}"))
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();

        let line = if items.is_empty() {
            format!("{champion} ({position}, LVL {level})")
        } else {
            format!("{champion} ({position}, LVL {level}, items: {items})")
        };

        match player.get("team").and_then(Value::as_str) {
            Some("ORDER") => order_team.push(line),
            Some("CHAOS") => chaos_team.push(line),
            _ => {}
        }
    }

    if order_team.is_empty() || chaos_team.is_empty() {
        bail!("Live Client Data does not contain both teams");
    }

    let active_summoner = game_data
        .get("activePlayer")
        .and_then(|active| active.get("summonerName"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown");

    let local_player = players
        .iter()
        .find(|player| {
            player
                .get("summonerName")
                .and_then(Value::as_str)
                == Some(active_summoner)
        })
        .and_then(|player| {
            let champion = player.get("championName")?.as_str()?;
            let position = player.get("position")?.as_str().unwrap_or("UNKNOWN");
            Some(format!("{champion} ({position})"))
        })
        .unwrap_or_else(|| active_summoner.to_string());

    Ok(format!(
        "我方实时阵容:\n{}\n\n敌方实时阵容:\n{}\n\n本局玩家: {}\n\n请结合实时等级和装备，给出对线战斗技巧、装备调整建议，以及控龙/控虫策略。",
        order_team.join("\n"),
        chaos_team.join("\n"),
        local_player
    ))
}
