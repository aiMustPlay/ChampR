//! Full structured parsing of League of Legends open client interfaces:
//!
//! - `LiveSnapshot`: everything exposed by
//!   `https://127.0.0.1:2999/liveclientdata/allgamedata`
//!   (active player stats/runes/abilities/gold, per-player KDA/CS/runes/
//!   spells/items/death state, objective events, recent kill feed)
//! - `ChampSelectSnapshot`: everything exposed by the LCU endpoint
//!   `/lol-champ-select/v1/session` (bans, picks, positions, spells,
//!   summoner ids, local player and lane opponent pairing)

use serde_json::Value;
use std::collections::HashMap;

fn v_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn v_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn v_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn v_bool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

// ---------------------------------------------------------------------------
//  Live Client Data (in-game)
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct LivePlayer {
    pub summoner_name: String,
    pub champion_name: String,
    pub position: String,
    pub team: String,
    pub level: i64,
    pub kills: i64,
    pub deaths: i64,
    pub assists: i64,
    pub creep_score: i64,
    pub ward_score: f64,
    pub is_dead: bool,
    pub respawn_timer: f64,
    /// (item id, count) pairs, empty slots filtered out.
    pub items: Vec<(i64, i64)>,
    pub keystone_id: i64,
    pub keystone_name: String,
    pub primary_tree_id: i64,
    pub primary_tree_name: String,
    pub secondary_tree_id: i64,
    pub secondary_tree_name: String,
    pub spell_one: String,
    pub spell_two: String,
    pub is_bot: bool,
}

impl LivePlayer {
    fn parse(player: &Value) -> LivePlayer {
        let scores = player.get("scores").cloned().unwrap_or(Value::Null);
        let keystone = player.get("runes").and_then(|r| r.get("keystone")).cloned().unwrap_or(Value::Null);
        let primary_tree = player.get("runes").and_then(|r| r.get("primaryRuneTree")).cloned().unwrap_or(Value::Null);
        let secondary_tree = player.get("runes").and_then(|r| r.get("secondaryRuneTree")).cloned().unwrap_or(Value::Null);

        let items = player
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = v_i64(item, "itemID");
                        (id > 0).then(|| (id, v_i64(item, "count").max(1)))
                    })
                    .collect()
            })
            .unwrap_or_default();

        LivePlayer {
            summoner_name: v_str(player, "summonerName").to_string(),
            champion_name: v_str(player, "championName").to_string(),
            position: v_str(player, "position").to_string(),
            team: v_str(player, "team").to_string(),
            level: v_i64(player, "level").max(1),
            kills: v_i64(&scores, "kills"),
            deaths: v_i64(&scores, "deaths"),
            assists: v_i64(&scores, "assists"),
            creep_score: v_i64(&scores, "creepScore"),
            ward_score: v_f64(&scores, "wardScore"),
            is_dead: v_bool(player, "isDead"),
            respawn_timer: v_f64(player, "respawnTimer"),
            items,
            keystone_id: v_i64(&keystone, "id"),
            keystone_name: v_str(&keystone, "displayName").to_string(),
            primary_tree_id: v_i64(&primary_tree, "id"),
            primary_tree_name: v_str(&primary_tree, "displayName").to_string(),
            secondary_tree_id: v_i64(&secondary_tree, "id"),
            secondary_tree_name: v_str(&secondary_tree, "displayName").to_string(),
            spell_one: v_str(
                &player.get("summonerSpells").and_then(|s| s.get("summonerSpellOne")).cloned().unwrap_or(Value::Null),
                "displayName",
            )
            .to_string(),
            spell_two: v_str(
                &player.get("summonerSpells").and_then(|s| s.get("summonerSpellTwo")).cloned().unwrap_or(Value::Null),
                "displayName",
            )
            .to_string(),
            is_bot: v_bool(player, "isBot"),
        }
    }

    pub fn kda(&self) -> String {
        format!("{}/{}/{}", self.kills, self.deaths, self.assists)
    }
}

/// `activePlayer` block: only describes the local player, including things that are
/// not visible for other players (gold, ability levels, full rune page, stats panel).
#[derive(Default, Debug, Clone)]
pub struct ActivePlayer {
    pub summoner_name: String,
    pub level: i64,
    pub current_gold: f64,
    pub q_level: i64,
    pub w_level: i64,
    pub e_level: i64,
    pub r_level: i64,
    /// Flattened championStats: attackDamage, abilityPower, armor, magicResist,
    /// attackSpeed, critChance, currentHealth, maxHealth, moveSpeed, resourceValue...
    pub stats: HashMap<String, f64>,
    /// (perk id, english display name) pairs for the full rune page
    /// (keystone + 3 primary + 2 secondary + tree ids are separate below).
    pub full_runes: Vec<(i64, String)>,
    pub primary_tree_id: i64,
    pub primary_tree_name: String,
    pub secondary_tree_id: i64,
    pub secondary_tree_name: String,
}

impl ActivePlayer {
    fn parse(active: &Value) -> Option<ActivePlayer> {
        if active.is_null() {
            return None;
        }

        let ability = |key: &str| -> i64 {
            active
                .get("abilities")
                .and_then(|a| a.get(key))
                .and_then(|a| a.get("abilityLevel"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };

        let stats = active
            .get("championStats")
            .and_then(Value::as_object)
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n)))
                    .collect()
            })
            .unwrap_or_default();

        let full_runes = active.get("fullRunes").cloned().unwrap_or(Value::Null);
        let mut rune_ids: Vec<(i64, String)> = Vec::new();
        let push_rune = |rune_ids: &mut Vec<(i64, String)>, rune: &Value| {
            let id = v_i64(rune, "id");
            if id > 0 {
                rune_ids.push((id, v_str(rune, "displayName").to_string()));
            }
        };
        if let Some(keystone) = full_runes.get("keystone") {
            push_rune(&mut rune_ids, keystone);
        }
        if let Some(general) = full_runes.get("generalRunes").and_then(Value::as_array) {
            for rune in general {
                push_rune(&mut rune_ids, rune);
            }
        }
        if let Some(stats_runes) = full_runes.get("statRunes").and_then(Value::as_array) {
            for rune in stats_runes {
                push_rune(&mut rune_ids, rune);
            }
        }

        let primary_tree = full_runes.get("primaryRuneTree").cloned().unwrap_or(Value::Null);
        let secondary_tree = full_runes.get("secondaryRuneTree").cloned().unwrap_or(Value::Null);

        Some(ActivePlayer {
            summoner_name: v_str(active, "summonerName").to_string(),
            level: v_i64(active, "level"),
            current_gold: v_f64(active, "currentGold"),
            q_level: ability("Q"),
            w_level: ability("W"),
            e_level: ability("E"),
            r_level: ability("R"),
            stats,
            full_runes: rune_ids,
            primary_tree_id: v_i64(&primary_tree, "id"),
            primary_tree_name: v_str(&primary_tree, "displayName").to_string(),
            secondary_tree_id: v_i64(&secondary_tree, "id"),
            secondary_tree_name: v_str(&secondary_tree, "displayName").to_string(),
        })
    }
}

/// Per-team objective control accumulated from the event log.
#[derive(Default, Debug, Clone)]
pub struct TeamObjectives {
    pub dragons: Vec<String>,
    pub barons: i64,
    pub heralds: i64,
    pub void_grubs: i64,
    pub turrets: i64,
    pub inhibitors: i64,
}

#[derive(Default, Debug, Clone)]
pub struct KillEvent {
    pub event_time: f64,
    pub killer: String,
    pub victim: String,
    /// Team of the killer ("ORDER"/"CHAOS"), empty when it could not be resolved
    /// (e.g. executed by minions or turrets).
    pub killer_team: String,
}

/// An objective-relevant event (first blood, epic monsters, structures).
#[derive(Default, Debug, Clone)]
pub struct NamedEvent {
    pub event_time: f64,
    /// E.g. FirstBlood / DragonKill / BaronKill / HeraldKill / HordeKill /
    /// TurretKilled / InhibKilled / Ace.
    pub name: String,
    pub killer: String,
    /// Team credited with the event, resolved through the player list.
    pub killer_team: String,
    /// Dragon type ("Infernal"...) or the destroyed structure identifier.
    pub detail: String,
}

#[derive(Default, Debug)]
pub struct LiveSnapshot {
    pub game_mode: String,
    pub game_time: f64,
    pub map_name: String,
    pub map_terrain: String,
    pub ended: bool,
    pub players: Vec<LivePlayer>,
    pub active: Option<ActivePlayer>,
    pub order: TeamObjectives,
    pub chaos: TeamObjectives,
    pub first_blood_by: Option<String>,
    pub recent_kills: Vec<KillEvent>,
    /// Objective/structure events in chronological order (for reminders).
    pub events_log: Vec<NamedEvent>,
}

impl LiveSnapshot {
    pub fn from_all_game_data(data: &Value) -> anyhow::Result<LiveSnapshot> {
        let players = data
            .get("allPlayers")
            .and_then(Value::as_array)
            .map(|players| players.iter().map(LivePlayer::parse).collect::<Vec<_>>())
            .unwrap_or_default();

        anyhow::ensure!(!players.is_empty(), "Live Client Data allPlayers is empty");

        let by_summoner: HashMap<&str, &LivePlayer> = players
            .iter()
            .map(|p| (p.summoner_name.as_str(), p))
            .collect();
        let team_of = |name: &str| -> String {
            if let Some(player) = by_summoner.get(name) {
                return player.team.clone();
            }
            // Kill events sometimes reference champion names instead of summoner names.
            players
                .iter()
                .find(|p| p.champion_name == name)
                .map(|p| p.team.clone())
                .unwrap_or_default()
        };

        let game_data = data.get("gameData").cloned().unwrap_or(Value::Null);
        let events = data
            .get("events")
            .and_then(|e| e.get("Events"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut order = TeamObjectives::default();
        let mut chaos = TeamObjectives::default();
        let mut kills: Vec<KillEvent> = Vec::new();
        let mut events_log: Vec<NamedEvent> = Vec::new();
        let mut first_blood_by: Option<String> = None;
        let mut ended = false;

        for event in &events {
            let name = v_str(event, "EventName");
            let event_time = v_f64(event, "EventTime");
            let killer = v_str(event, "KillerName").to_string();
            match name {
                "ChampionKill" => {
                    kills.push(KillEvent {
                        event_time,
                        killer: killer.clone(),
                        victim: v_str(event, "VictimName").to_string(),
                        killer_team: team_of(&killer),
                    });
                }
                "FirstBlood" => {
                    let recipient = v_str(event, "Recipient").to_string();
                    first_blood_by = Some(recipient.clone());
                    events_log.push(NamedEvent {
                        event_time,
                        name: name.to_string(),
                        killer: recipient.clone(),
                        killer_team: team_of(&recipient),
                        detail: String::new(),
                    });
                }
                "DragonKill" => {
                    let killer_team = team_of(&killer);
                    let objetives = match killer_team.as_str() {
                        "CHAOS" => &mut chaos,
                        _ => &mut order,
                    };
                    let dragon_type = v_str(event, "DragonType");
                    objetives.dragons.push(if dragon_type.is_empty() {
                        "Dragon".to_string()
                    } else {
                        dragon_type.to_string()
                    });
                    events_log.push(NamedEvent {
                        event_time,
                        name: name.to_string(),
                        killer,
                        killer_team,
                        detail: dragon_type.to_string(),
                    });
                }
                "BaronKill" | "HeraldKill" => {
                    let killer_team = team_of(&killer);
                    match killer_team.as_str() {
                        "CHAOS" => {
                            if name == "BaronKill" {
                                chaos.barons += 1;
                            } else {
                                chaos.heralds += 1;
                            }
                        }
                        "ORDER" => {
                            if name == "BaronKill" {
                                order.barons += 1;
                            } else {
                                order.heralds += 1;
                            }
                        }
                        _ => continue,
                    }
                    events_log.push(NamedEvent {
                        event_time,
                        name: name.to_string(),
                        killer,
                        killer_team,
                        detail: String::new(),
                    });
                }
                // Void grubs are emitted as HordeKill (older patches may use VoidGrubKill).
                name if name.contains("Horde") || name.contains("Grub") => {
                    let killer_team = team_of(&killer);
                    match killer_team.as_str() {
                        "CHAOS" => chaos.void_grubs += 1,
                        "ORDER" => order.void_grubs += 1,
                        _ => {}
                    }
                    if !killer_team.is_empty() {
                        events_log.push(NamedEvent {
                            event_time,
                            name: name.to_string(),
                            killer,
                            killer_team,
                            detail: String::new(),
                        });
                    }
                }
                "TurretKilled" | "InhibKilled" => {
                    let killer_team = team_of(&killer);
                    match killer_team.as_str() {
                        "CHAOS" => {
                            if name == "TurretKilled" {
                                chaos.turrets += 1;
                            } else {
                                chaos.inhibitors += 1;
                            }
                        }
                        "ORDER" => {
                            if name == "TurretKilled" {
                                order.turrets += 1;
                            } else {
                                order.inhibitors += 1;
                            }
                        }
                        _ => continue,
                    }
                    events_log.push(NamedEvent {
                        event_time,
                        name: name.to_string(),
                        killer,
                        killer_team,
                        detail: if name == "TurretKilled" {
                            v_str(event, "TurretKilled").to_string()
                        } else {
                            v_str(event, "InhibKilled").to_string()
                        },
                    });
                }
                "GameEnd" => ended = true,
                _ => {}
            }
        }

        let mut recent_kills = kills;
        if recent_kills.len() > 6 {
            recent_kills = recent_kills.split_off(recent_kills.len() - 6);
        }

        Ok(LiveSnapshot {
            game_mode: v_str(&game_data, "gameMode").to_string(),
            game_time: v_f64(&game_data, "gameTime"),
            map_name: v_str(&game_data, "mapName").to_string(),
            map_terrain: v_str(&game_data, "mapTerrain").to_string(),
            ended,
            players,
            active: ActivePlayer::parse(&data.get("activePlayer").cloned().unwrap_or(Value::Null)),
            order,
            chaos,
            first_blood_by,
            recent_kills,
            events_log,
        })
    }

    pub fn team_players(&self, team: &str) -> Vec<&LivePlayer> {
        self.players.iter().filter(|p| p.team == team).collect()
    }

    /// The local player's full player entry, matched by the activePlayer name.
    pub fn local_player(&self) -> Option<&LivePlayer> {
        let active = self.active.as_ref()?;
        // Newer clients report "name#tag" in activePlayer while allPlayers entries
        // may use either form, so compare both exact and prefix matches.
        self.players
            .iter()
            .find(|p| p.summoner_name == active.summoner_name)
            .or_else(|| {
                let base = active.summoner_name.split('#').next().unwrap_or("");
                self.players.iter().find(|p| {
                    !base.is_empty() && p.summoner_name.split('#').next() == Some(base)
                })
            })
    }

    /// Enemy player on the same lane as the local player (when positions are known).
    pub fn lane_opponent(&self) -> Option<&LivePlayer> {
        let local = self.local_player()?;
        if local.position.is_empty() {
            return None;
        }
        self.players
            .iter()
            .find(|p| p.team != local.team && p.position == local.position)
    }

    pub fn team_kills(&self, team: &str) -> i64 {
        self.players
            .iter()
            .filter(|p| p.team == team)
            .map(|p| p.kills)
            .sum()
    }
}

// ---------------------------------------------------------------------------
//  Champ select (LCU)
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct ChampSelectMember {
    pub cell_id: i64,
    /// Locked-in champion (0 while only hovering).
    pub champion_id: i64,
    /// Hover intent, useful before the pick is locked.
    pub pick_intent: i64,
    pub assigned_position: String,
    pub summoner_id: i64,
    pub puuid: String,
    pub spell1_id: i64,
    pub spell2_id: i64,
    /// 显示名: 有的区服给 displayName/summonerName, 有的什么都不给(隐私限制)。
    /// 空串时 UI 用"队友N/对手N/我"兜底, 不要显示空白。
    pub display_name: String,
}

impl ChampSelectMember {
    fn parse(member: &Value) -> ChampSelectMember {
        let display_name = {
            let display = v_str(member, "displayName");
            if !display.is_empty() {
                display
            } else {
                v_str(member, "summonerName")
            }
        };
        ChampSelectMember {
            cell_id: v_i64(member, "cellId"),
            champion_id: v_i64(member, "championId"),
            pick_intent: v_i64(member, "championPickIntent"),
            assigned_position: v_str(member, "assignedPosition").to_string(),
            summoner_id: v_i64(member, "summonerId"),
            puuid: v_str(member, "puuid").to_string(),
            spell1_id: v_i64(member, "spell1Id"),
            spell2_id: v_i64(member, "spell2Id"),
            display_name: display_name.to_string(),
        }
    }

    /// Champion that should currently be discussed (locked, otherwise hovered).
    pub fn effective_champion(&self) -> i64 {
        if self.champion_id > 0 {
            self.champion_id
        } else {
            self.pick_intent
        }
    }
}

#[derive(Default, Debug)]
pub struct ChampSelectSnapshot {
    pub local_cell_id: i64,
    pub my_team: Vec<ChampSelectMember>,
    pub their_team: Vec<ChampSelectMember>,
    pub my_bans: Vec<i64>,
    pub their_bans: Vec<i64>,
}

impl ChampSelectSnapshot {
    pub fn from_session(session: &Value) -> anyhow::Result<ChampSelectSnapshot> {
        let my_team = session
            .get("myTeam")
            .and_then(Value::as_array)
            .map(|team| team.iter().map(ChampSelectMember::parse).collect::<Vec<_>>())
            .unwrap_or_default();
        let their_team = session
            .get("theirTeam")
            .and_then(Value::as_array)
            .map(|team| team.iter().map(ChampSelectMember::parse).collect::<Vec<_>>())
            .unwrap_or_default();

        anyhow::ensure!(
            !my_team.is_empty() && !their_team.is_empty(),
            "champ select session does not contain both teams"
        );

        let bans = session.get("bans").cloned().unwrap_or(Value::Null);
        let ban_ids = |key: &str| -> Vec<i64> {
            bans.get(key)
                .and_then(Value::as_array)
                .map(|bans| bans.iter().filter_map(Value::as_i64).filter(|id| *id > 0).collect())
                .unwrap_or_default()
        };

        Ok(ChampSelectSnapshot {
            local_cell_id: v_i64(session, "localPlayerCellId"),
            my_team,
            their_team,
            my_bans: ban_ids("myTeamBans"),
            their_bans: ban_ids("theirTeamBans"),
        })
    }

    pub fn local_member(&self) -> Option<&ChampSelectMember> {
        self.my_team.iter().find(|m| m.cell_id == self.local_cell_id)
    }

    /// Enemy occupying the same assigned position as the local player.
    pub fn lane_opponent(&self) -> Option<&ChampSelectMember> {
        let local = self.local_member()?;
        if local.assigned_position.is_empty() {
            return None;
        }
        self.their_team
            .iter()
            .find(|m| m.assigned_position == local.assigned_position)
    }

    /// 一排选秀顺位: "1楼·上单·暗裔剑魔(我) | 2楼·打野·李青 | …"
    ///
    /// Draft 协议下 cell 顺序即选秀顺位(蓝 0..5 / 红 5..10), 这里直接用
    /// 队伍数组序映射"几楼"。大乱斗等无顺位模式顺序仅是展示序。
    /// name_of: 由调用方解析冠军 id → 中文名; 未悬停未锁时显示"选择中"。
    pub fn roster_line(&self, enemy: bool, name_of: &dyn Fn(i64) -> Option<String>) -> String {
        let team = if enemy { &self.their_team } else { &self.my_team };
        team.iter()
            .enumerate()
            .map(|(i, m)| {
                let cid = m.effective_champion();
                let hero = if cid > 0 {
                    name_of(cid).unwrap_or_else(|| format!("英雄#{cid}"))
                } else {
                    "选择中".to_string()
                };
                let me_mark = if !enemy && m.cell_id == self.local_cell_id {
                    "(我)"
                } else {
                    ""
                };
                format!(
                    "{}楼·{}·{}{}",
                    i + 1,
                    position_label(&m.assigned_position),
                    hero,
                    me_mark
                )
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

// ---------------------------------------------------------------------------
//  Position helpers shared by both sources
// ---------------------------------------------------------------------------

/// Localized lane label for LCU/Live position tokens.
pub fn position_label(position: &str) -> String {
    let lower = position.to_ascii_lowercase();
    let label = match lower.as_str() {
        "top" => "上单",
        "jungle" | "jug" => "打野",
        "middle" | "mid" => "中单",
        "bottom" | "adc" | "bot" => "下路",
        "utility" | "support" | "sup" => "辅助",
        "aram" => "大乱斗",
        "" => "待分配",
        other => other,
    };
    label.to_string()
}

/// OP.GG section position tokens that correspond to an LCU assigned position.
pub fn opgg_position_aliases(assigned_position: &str) -> &'static [&'static str] {
    match assigned_position.to_ascii_lowercase().as_str() {
        "top" => &["top"],
        "jungle" | "jug" => &["jungle", "jug"],
        "middle" | "mid" => &["middle", "mid"],
        "bottom" | "adc" | "bot" => &["bottom", "adc", "bot"],
        "utility" | "support" | "sup" => &["utility", "support", "sup"],
        _ => &[],
    }
}

// ---------------------------------------------------------------------------
//  Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_game_data() -> Value {
        json!({
            "activePlayer": {
                "abilities": {
                    "Q": {"abilityLevel": 3},
                    "W": {"abilityLevel": 1},
                    "E": {"abilityLevel": 2},
                    "R": {"abilityLevel": 1}
                },
                "championStats": {"attackDamage": 187.0, "armor": 64.0, "maxHealth": 1610.0},
                "currentGold": 2233.5,
                "fullRunes": {
                    "keystone": {"displayName": "Conqueror", "id": 8010},
                    "generalRunes": [
                        {"displayName": "Triumph", "id": 8009},
                        {"displayName": "Legend: Alacrity", "id": 9104},
                        {"displayName": "Last Stand", "id": 8299}
                    ],
                    "primaryRuneTree": {"displayName": "Precision", "id": 8000},
                    "secondaryRuneTree": {"displayName": "Resolve", "id": 8400},
                    "statRunes": [{"id": 5008}, {"id": 5002}]
                },
                "level": 8,
                "summonerName": "Me#CN1"
            },
            "allPlayers": [
                {
                    "championName": "Aatrox",
                    "isDead": false,
                    "items": [{"itemID": 1055, "count": 1}, {"itemID": 0, "count": 0}],
                    "level": 8,
                    "position": "TOP",
                    "respawnTimer": 0.0,
                    "runes": {
                        "keystone": {"displayName": "Conqueror", "id": 8010},
                        "primaryRuneTree": {"displayName": "Precision", "id": 8000},
                        "secondaryRuneTree": {"displayName": "Resolve", "id": 8400}
                    },
                    "scores": {"assists": 0, "creepScore": 64, "deaths": 1, "kills": 3, "wardScore": 4.0},
                    "summonerName": "Me",
                    "summonerSpells": {
                        "summonerSpellOne": {"displayName": "Flash"},
                        "summonerSpellTwo": {"displayName": "Teleport"}
                    },
                    "team": "ORDER"
                },
                {
                    "championName": "Zed",
                    "isDead": true,
                    "items": [{"itemID": 6692, "count": 1}],
                    "level": 9,
                    "position": "TOP",
                    "respawnTimer": 9.5,
                    "runes": {
                        "keystone": {"displayName": "Conqueror", "id": 8010},
                        "primaryRuneTree": {"displayName": "Precision", "id": 8000},
                        "secondaryRuneTree": {"displayName": "Domination", "id": 8100}
                    },
                    "scores": {"assists": 1, "creepScore": 80, "deaths": 2, "kills": 4, "wardScore": 2.0},
                    "summonerName": "Foe",
                    "summonerSpells": {
                        "summonerSpellOne": {"displayName": "Flash"},
                        "summonerSpellTwo": {"displayName": "Ignite"}
                    },
                    "team": "CHAOS"
                }
            ],
            "events": {
                "Events": [
                    {"EventID": 0, "EventName": "GameStart", "EventTime": 0.1},
                    {"EventName": "FirstBlood", "Recipient": "Foe"},
                    {"EventName": "ChampionKill", "EventTime": 150.0, "KillerName": "Foe", "VictimName": "Me", "Assisters": []},
                    {"EventName": "ChampionKill", "EventTime": 300.0, "KillerName": "Me", "VictimName": "Foe", "Assisters": []},
                    {"EventName": "DragonKill", "EventTime": 320.0, "KillerName": "Me", "DragonType": "Infernal"},
                    {"EventName": "HordeKill", "EventTime": 400.0, "KillerName": "Foe"},
                    {"EventName": "TurretKilled", "EventTime": 420.0, "KillerName": "Me", "TurretKilled": "Turret_T1_L_03_A"},
                    {"EventName": "BaronKill", "EventTime": 500.0, "KillerName": "Foe"}
                ]
            },
            "gameData": {"gameMode": "CLASSIC", "gameTime": 754.4, "mapName": "Map11", "mapTerrain": "Infernal"}
        })
    }

    #[test]
    fn parses_full_live_snapshot() {
        let snap = LiveSnapshot::from_all_game_data(&sample_game_data()).unwrap();

        assert_eq!(snap.players.len(), 2);
        assert!(!snap.ended);
        assert_eq!(snap.map_terrain, "Infernal");

        let me = snap.local_player().unwrap();
        assert_eq!(me.champion_name, "Aatrox");
        assert_eq!(me.kda(), "3/1/0");
        assert_eq!(me.creep_score, 64);
        assert_eq!(me.items, vec![(1055, 1)]);

        let opp = snap.lane_opponent().unwrap();
        assert_eq!(opp.champion_name, "Zed");
        assert!(opp.is_dead);
        assert!((opp.respawn_timer - 9.5).abs() < 0.001);

        let active = snap.active.as_ref().unwrap();
        assert!((active.current_gold - 2233.5).abs() < 0.001);
        assert_eq!((active.q_level, active.w_level, active.e_level, active.r_level), (3, 1, 2, 1));
        assert_eq!(active.full_runes.len(), 6);
        assert_eq!((active.stats["attackDamage"] - 187.0).abs() < 0.001, true);

        assert_eq!(snap.order.dragons, vec!["Infernal".to_string()]);
        assert_eq!(snap.order.turrets, 1);
        assert_eq!(snap.chaos.barons, 1);
        assert_eq!(snap.chaos.void_grubs, 1);
        assert_eq!(snap.first_blood_by.as_deref(), Some("Foe"));
        assert_eq!(snap.recent_kills.len(), 2);
        assert_eq!(snap.team_kills("ORDER"), 3);
    }

    #[test]
    fn parses_champ_select_snapshot_with_opponent() {
        let session = json!({
            "localPlayerCellId": 0,
            "bans": {"myTeamBans": [157, 0], "theirTeamBans": [142, 64]},
            "myTeam": [
                {"cellId": 0, "championId": 142, "championPickIntent": 0, "assignedPosition": "middle", "summonerId": 11, "puuid": "p-me", "spell1Id": 4, "spell2Id": 14},
                {"cellId": 1, "championId": 0, "championPickIntent": 266, "assignedPosition": "top", "summonerId": 22, "puuid": "", "spell1Id": 4, "spell2Id": 12}
            ],
            "theirTeam": [
                {"cellId": 5, "championId": 238, "assignedPosition": "middle", "summonerId": 0, "puuid": "", "spell1Id": 4, "spell2Id": 14},
                {"cellId": 6, "championId": 23, "assignedPosition": "top", "summonerId": 33, "puuid": ""}
            ]
        });

        let snap = ChampSelectSnapshot::from_session(&session).unwrap();
        assert_eq!(snap.my_bans, vec![157]);
        assert_eq!(snap.their_bans, vec![142, 64]);

        let local = snap.local_member().unwrap();
        assert_eq!(local.champion_id, 142);
        assert_eq!(local.spell2_id, 14);

        let opponent = snap.lane_opponent().unwrap();
        assert_eq!(opponent.champion_id, 238);

        let hover = &snap.my_team[1];
        assert_eq!(hover.champion_id, 0);
        assert_eq!(hover.effective_champion(), 266);
    }

    #[test]
    fn roster_line_marks_pick_order_position_and_me() {
        let session = json!({
            "localPlayerCellId": 1,
            "bans": {},
            "myTeam": [
                {"cellId": 0, "championId": 142, "championPickIntent": 0, "assignedPosition": "middle"},
                {"cellId": 1, "championId": 0, "championPickIntent": 266, "assignedPosition": "top"},
                {"cellId": 2, "championId": 0, "championPickIntent": 0, "assignedPosition": "jungle"}
            ],
            "theirTeam": [
                {"cellId": 5, "championId": 238, "assignedPosition": "middle"},
                {"cellId": 6, "championId": 23, "championPickIntent": 0, "assignedPosition": "top"}
            ]
        });
        let snap = ChampSelectSnapshot::from_session(&session).unwrap();
        let names = |id: i64| -> Option<String> {
            match id {
                142 => Some("佐伊".into()),
                266 => Some("暗裔剑魔".into()),
                238 => Some("劫".into()),
                23 => Some("泰达米尔".into()),
                _ => None,
            }
        };
        assert_eq!(
            snap.roster_line(false, &names),
            "1楼·中单·佐伊 | 2楼·上单·暗裔剑魔(我) | 3楼·打野·选择中"
        );
        assert_eq!(
            snap.roster_line(true, &names),
            "1楼·中单·劫 | 2楼·上单·泰达米尔"
        );
    }

    #[test]
    fn rejects_live_snapshot_without_players() {
        let data = json!({"allPlayers": [], "gameData": {}, "events": {"Events": []}});
        assert!(LiveSnapshot::from_all_game_data(&data).is_err());
    }
}
