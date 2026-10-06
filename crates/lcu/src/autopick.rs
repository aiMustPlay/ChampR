//! 选人阶段的自动化决策: 自动禁人 + 自动选人。
//!
//! 用户 2026-10-05: "已经有默认进房间的功能了, 继续实现默认禁人和默认选人的功能"
//! (进房间 = 排队就绪自动接受, 已在 lcu_api::accept_ready_check)。
//!
//! 设计原则(与项目其它部分一致):
//! - **只动自己的动作**: 逐个动作检查 actorCellId == 本机 cellId, 别人的动作绝不碰
//! - **决策是纯函数**: 输入 = champ-select session JSON + 用户偏好 + OP.GG 数据,
//!   输出 = 一个 AutoAction。因此能用真实 session 结构写单测, 不依赖客户端在线
//! - **可解释**: 每个动作带 reason(为什么选这个英雄), 调用方负责写日志/播报
//! - **保守**: 没有配置、没有可依据的数据时返回 None, 绝不瞎选一个英雄

use std::collections::HashMap;

use serde_json::Value;

use crate::builds::BuildSection;
use crate::web::StaticNames;

/// 用户偏好(来自 settings.toml)。
#[derive(Debug, Clone, Default)]
pub struct AutoPrefs {
    /// 自动禁人
    pub auto_ban: bool,
    /// 自动选人(先悬停, 快到时锁定)
    pub auto_pick: bool,
    /// 优先禁用的英雄 id(按顺序取第一个还没被禁掉的)
    pub ban_list: Vec<i64>,
    /// 悬停后锁定: 剩余时间 <= 该秒数时锁定(0 = 悬停后立刻锁定)
    pub lock_before_seconds: f64,
}

impl AutoPrefs {
    /// 是否开启了任何自动动作。
    pub fn any_enabled(&self) -> bool {
        self.auto_ban || self.auto_pick
    }

    /// 人类可读的配置摘要(给日志用, 便于事后复盘"为什么没生效")。
    pub fn describe(&self, names: &StaticNames) -> String {
        let list = |ids: &[i64]| -> String {
            if ids.is_empty() {
                "(空)".to_string()
            } else {
                ids.iter()
                    .map(|id| {
                        names
                            .champion(&id.to_string())
                            .map(str::to_string)
                            .unwrap_or_else(|| id.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            }
        };
        format!(
            "自动禁人={} 名单={} | 自动选人={} | 锁定阈值={:.0}s",
            self.auto_ban,
            list(&self.ban_list),
            self.auto_pick,
            self.lock_before_seconds
        )
    }
}

/// 决策结果。
#[derive(Debug, Clone, PartialEq)]
pub enum AutoAction {
    /// 什么都不做(本机没有待办动作 / 开关没开 / 没有依据)
    None,
    /// 禁用某英雄(禁人必须 completed = true 才生效)
    Ban {
        action_id: i64,
        champion_id: i64,
        reason: String,
    },
    /// 选人: 先悬停(completed=false), 到时锁定(completed=true)
    Pick {
        action_id: i64,
        champion_id: i64,
        completed: bool,
        reason: String,
    },
}

fn action_i64(action: &Value, key: &str) -> i64 {
    action.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn action_str<'a>(action: &'a Value, key: &str) -> &'a str {
    action.get(key).and_then(Value::as_str).unwrap_or("")
}

fn champion_name(names: &StaticNames, champion_id: i64) -> String {
    names
        .champion(&champion_id.to_string())
        .map(str::to_string)
        .unwrap_or_else(|| format!("英雄{champion_id}"))
}

/// 会话里已被禁用的英雄 id(双方都算, 避免重复禁同一个)。
fn banned_champions(session: &Value) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(actions) = session.get("actions").and_then(Value::as_array) {
        for group in actions {
            if let Some(list) = group.as_array() {
                for action in list {
                    if action_str(action, "type") == "ban"
                        && action
                            .get("completed")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    {
                        let champ = action_i64(action, "championId");
                        if champ > 0 {
                            out.push(champ);
                        }
                    }
                }
            }
        }
    }
    out
}

/// 本机当前待办的动作: (action_id, type, 当前英雄, 阶段剩余秒数)。
fn pending_own_actions(session: &Value, local_cell: i64) -> Vec<(i64, String, i64, f64)> {
    let mut out = Vec::new();
    let remaining = session
        .get("timer")
        .and_then(|timer| timer.get("adjustedTimeLeftInPhase"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        / 1000.0;

    if let Some(actions) = session.get("actions").and_then(Value::as_array) {
        for group in actions {
            let Some(list) = group.as_array() else { continue };
            for action in list {
                if action_i64(action, "actorCellId") != local_cell {
                    continue;
                }
                if action
                    .get("completed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    continue;
                }
                // isInProgress=false 表示还没轮到我, 提前提交会被客户端拒绝
                let in_progress = action
                    .get("isInProgress")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                if !in_progress {
                    continue;
                }
                out.push((
                    action_i64(action, "id"),
                    action_str(action, "type").to_string(),
                    action_i64(action, "championId"),
                    remaining,
                ));
            }
        }
    }
    out
}

/// 本机当前悬停/锁定的英雄 id(0 = 还没选)。
fn local_champion(session: &Value, local_cell: i64) -> i64 {
    session
        .get("myTeam")
        .and_then(Value::as_array)
        .and_then(|team| {
            team.iter().find(|member| {
                member.get("cellId").and_then(Value::as_i64).unwrap_or(-1) == local_cell
            })
        })
        .map(|member| {
            let locked = member.get("championId").and_then(Value::as_i64).unwrap_or(0);
            if locked > 0 {
                locked
            } else {
                member
                    .get("championPickIntent")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            }
        })
        .unwrap_or(0)
}

/// 本机被分配的分路(空 = 未知)。
fn local_position(session: &Value, local_cell: i64) -> String {
    session
        .get("myTeam")
        .and_then(Value::as_array)
        .and_then(|team| {
            team.iter().find(|member| {
                member.get("cellId").and_then(Value::as_i64).unwrap_or(-1) == local_cell
            })
        })
        .map(|member| {
            member
                .get("assignedPosition")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        })
        .unwrap_or_default()
}

/// 决策: 本机现在该做什么。
pub fn decide(
    session: &Value,
    prefs: &AutoPrefs,
    names: &StaticNames,
    sections: &HashMap<i64, Vec<BuildSection>>,
) -> AutoAction {
    if !prefs.any_enabled() {
        return AutoAction::None;
    }
    let local_cell = session
        .get("localPlayerCellId")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if local_cell < 0 {
        return AutoAction::None;
    }

    let pending = pending_own_actions(session, local_cell);
    if pending.is_empty() {
        return AutoAction::None;
    }

    // 禁人阶段在前, 选人在后; 一次只做一个动作
    for (action_id, kind, current_champion, remaining) in &pending {
        match kind.as_str() {
            "ban" if prefs.auto_ban => {
                if *current_champion > 0 {
                    // 客户端已带上了要禁的英雄, 只差确认
                    return AutoAction::Ban {
                        action_id: *action_id,
                        champion_id: *current_champion,
                        reason: format!("确认禁用 {}", champion_name(names, *current_champion)),
                    };
                }
                let already_banned = banned_champions(session);
                match prefs
                    .ban_list
                    .iter()
                    .copied()
                    .find(|id| !already_banned.contains(id))
                {
                    Some(champion_id) => {
                        return AutoAction::Ban {
                            action_id: *action_id,
                            champion_id,
                            reason: format!("优先禁用名单: {}", champion_name(names, champion_id)),
                        }
                    }
                    None => {
                        // 名单空 / 都被禁了: 不做动作(不瞎禁), 由调用方记一条日志
                        return AutoAction::None;
                    }
                }
            }
            "pick" if prefs.auto_pick => {
                // 目标英雄: 用户已悬停的 > OP.GG 该分路最高胜率
                let hovered = local_champion(session, local_cell);
                let (target, reason) = if hovered > 0 {
                    (hovered, format!("沿用你已悬停的 {}", champion_name(names, hovered)))
                } else {
                    match best_winrate_champion(sections, session, local_cell) {
                        Some(id) => (
                            id,
                            format!("OP.GG 分路胜率最高: {}", champion_name(names, id)),
                        ),
                        None => return AutoAction::None,
                    }
                };
                if target <= 0 {
                    return AutoAction::None;
                }

                // lock_before_seconds = 0 表示悬停后立刻锁定; > 0 则等到剩余时间进入阈值
                let should_lock = if prefs.lock_before_seconds <= 0.0 {
                    true
                } else {
                    *remaining <= prefs.lock_before_seconds
                };
                if *current_champion == target {
                    if should_lock {
                        return AutoAction::Pick {
                            action_id: *action_id,
                            champion_id: target,
                            completed: true,
                            reason: format!("锁定 {}", champion_name(names, target)),
                        };
                    }
                    // 已是目标英雄, 等锁定时机
                    return AutoAction::None;
                }
                return AutoAction::Pick {
                    action_id: *action_id,
                    champion_id: target,
                    completed: false,
                    reason,
                };
            }
            _ => {}
        }
    }

    AutoAction::None
}

/// OP.GG 数据里, 本机分路胜率最高且样本足够的英雄。
fn best_winrate_champion(
    sections: &HashMap<i64, Vec<BuildSection>>,
    session: &Value,
    local_cell: i64,
) -> Option<i64> {
    let position = local_position(session, local_cell);
    let parse_rate = |text: &str| -> f64 {
        text.trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .unwrap_or(0.0)
    };

    let mut best: Option<(i64, f64)> = None;
    for (champion_id, list) in sections {
        for section in list {
            if !position.is_empty() && section.position != position {
                continue;
            }
            // 场次太少的样本不采信
            if section.pick_count < 300 {
                continue;
            }
            let rate = parse_rate(&section.win_rate);
            if best.map(|(_, top)| rate > top).unwrap_or(true) {
                best = Some((*champion_id, rate));
            }
        }
    }
    best.map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> StaticNames {
        StaticNames {
            champions_cn: HashMap::from([
                ("266".to_string(), "暗裔剑魔".to_string()),
                ("267".to_string(), "唤潮鲛姬".to_string()),
                ("103".to_string(), "九尾妖狐".to_string()),
                ("238".to_string(), "影流之主".to_string()),
            ]),
            runes_cn: HashMap::new(),
            items_cn: HashMap::new(),
        }
    }

    fn sections_with(champion_id: i64, position: &str, rate: &str, games: i64) -> HashMap<i64, Vec<BuildSection>> {
        let section = BuildSection {
            index: 0,
            id: "s".to_string(),
            version: "1".to_string(),
            official_version: "1".to_string(),
            pick_count: games,
            win_rate: rate.to_string(),
            timestamp: 0,
            alias: "x".to_string(),
            name: "x".to_string(),
            position: position.to_string(),
            ..Default::default()
        };
        HashMap::from([(champion_id, vec![section])])
    }

    fn ban_session(action_champion: i64) -> Value {
        serde_json::json!({
            "localPlayerCellId": 2,
            "timer": { "adjustedTimeLeftInPhase": 20000 },
            "myTeam": [
                { "cellId": 2, "championId": 0, "championPickIntent": 0, "assignedPosition": "middle" }
            ],
            "actions": [[
                { "id": 11, "actorCellId": 2, "type": "ban", "completed": false,
                  "isInProgress": true, "championId": action_champion }
            ]]
        })
    }

    fn pick_session(action_champion: i64, team_champion: i64, remaining_ms: i64) -> Value {
        serde_json::json!({
            "localPlayerCellId": 2,
            "timer": { "adjustedTimeLeftInPhase": remaining_ms },
            "myTeam": [
                { "cellId": 2, "championId": team_champion, "championPickIntent": 0, "assignedPosition": "middle" }
            ],
            "actions": [[
                { "id": 21, "actorCellId": 2, "type": "pick", "completed": false,
                  "isInProgress": true, "championId": action_champion }
            ]]
        })
    }

    #[test]
    fn disabled_does_nothing() {
        let prefs = AutoPrefs::default();
        assert_eq!(
            decide(&ban_session(0), &prefs, &names(), &HashMap::new()),
            AutoAction::None
        );
    }

    #[test]
    fn confirm_existing_ban_choice() {
        let prefs = AutoPrefs {
            auto_ban: true,
            ..Default::default()
        };
        match decide(&ban_session(238), &prefs, &names(), &HashMap::new()) {
            AutoAction::Ban {
                action_id,
                champion_id,
                reason,
            } => {
                assert_eq!(action_id, 11);
                assert_eq!(champion_id, 238);
                assert!(reason.contains("影流之主"), "{reason}");
            }
            other => panic!("expected ban, got {other:?}"),
        }
    }

    #[test]
    fn bans_first_entry_of_the_list() {
        let prefs = AutoPrefs {
            auto_ban: true,
            ban_list: vec![266, 267],
            ..Default::default()
        };
        match decide(&ban_session(0), &prefs, &names(), &HashMap::new()) {
            AutoAction::Ban {
                action_id,
                champion_id,
                reason,
            } => {
                assert_eq!(action_id, 11);
                assert_eq!(champion_id, 266);
                assert!(reason.contains("暗裔剑魔"), "{reason}");
            }
            other => panic!("expected ban, got {other:?}"),
        }
    }

    #[test]
    fn skips_already_banned_champions() {
        let mut session = ban_session(0);
        session["actions"] = serde_json::json!([
            [{ "id": 10, "actorCellId": 5, "type": "ban", "completed": true,
               "isInProgress": false, "championId": 266 }],
            [{ "id": 11, "actorCellId": 2, "type": "ban", "completed": false,
               "isInProgress": true, "championId": 0 }]
        ]);
        let prefs = AutoPrefs {
            auto_ban: true,
            ban_list: vec![266, 267],
            ..Default::default()
        };
        match decide(&session, &prefs, &names(), &HashMap::new()) {
            AutoAction::Ban { champion_id, .. } => assert_eq!(champion_id, 267),
            other => panic!("expected ban, got {other:?}"),
        }
    }

    #[test]
    fn empty_ban_list_does_nothing() {
        let prefs = AutoPrefs {
            auto_ban: true,
            ban_list: vec![],
            ..Default::default()
        };
        assert_eq!(
            decide(&ban_session(0), &prefs, &names(), &HashMap::new()),
            AutoAction::None
        );
    }

    #[test]
    fn never_touches_other_players_actions() {
        let mut session = ban_session(0);
        session["actions"] = serde_json::json!([[{
            "id": 7, "actorCellId": 4, "type": "ban", "completed": false,
            "isInProgress": true, "championId": 0
        }]]);
        let prefs = AutoPrefs {
            auto_ban: true,
            ban_list: vec![266],
            ..Default::default()
        };
        assert_eq!(
            decide(&session, &prefs, &names(), &HashMap::new()),
            AutoAction::None
        );
    }

    #[test]
    fn ignores_actions_that_are_not_in_progress() {
        let mut session = pick_session(0, 0, 20000);
        session["actions"] = serde_json::json!([[{
            "id": 31, "actorCellId": 2, "type": "pick", "completed": false,
            "isInProgress": false, "championId": 0
        }]]);
        let prefs = AutoPrefs {
            auto_pick: true,
            ..Default::default()
        };
        assert_eq!(
            decide(&session, &prefs, &names(), &HashMap::new()),
            AutoAction::None
        );
    }

    #[test]
    fn hovers_first_then_locks_when_time_runs_out() {
        let prefs = AutoPrefs {
            auto_pick: true,
            lock_before_seconds: 5.0,
            ..Default::default()
        };
        let sections = sections_with(103, "middle", "54.3%", 1200);

        // 还没选 → 悬停(completed = false), 目标来自 OP.GG 该分路最高胜率
        match decide(&pick_session(0, 0, 20000), &prefs, &names(), &sections) {
            AutoAction::Pick {
                action_id,
                champion_id,
                completed,
                reason,
            } => {
                assert_eq!(action_id, 21);
                assert_eq!(champion_id, 103);
                assert!(!completed);
                assert!(reason.contains("胜率最高"), "{reason}");
            }
            other => panic!("expected hover, got {other:?}"),
        }

        // 已是目标英雄但时间还早 → 什么都不做
        assert_eq!(
            decide(&pick_session(103, 103, 20000), &prefs, &names(), &sections),
            AutoAction::None
        );

        // 剩余 3 秒(<= 5) → 锁定
        match decide(&pick_session(103, 103, 3000), &prefs, &names(), &sections) {
            AutoAction::Pick { completed, .. } => assert!(completed),
            other => panic!("expected lock, got {other:?}"),
        }
    }

    #[test]
    fn respects_user_hover() {
        let prefs = AutoPrefs {
            auto_pick: true,
            lock_before_seconds: 0.0,
            ..Default::default()
        };
        // 用户自己悬停了 238 → 立刻锁定它, 不换成别的
        match decide(&pick_session(238, 238, 20000), &prefs, &names(), &HashMap::new()) {
            AutoAction::Pick {
                champion_id,
                completed,
                ..
            } => {
                assert_eq!(champion_id, 238);
                assert!(completed);
            }
            other => panic!("expected lock of the hovered champion, got {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_best_winrate_for_the_position() {
        let prefs = AutoPrefs {
            auto_pick: true,
            lock_before_seconds: 5.0,
            ..Default::default()
        };
        let sections = sections_with(103, "middle", "54.3%", 1200);
        match decide(&pick_session(0, 0, 20000), &prefs, &names(), &sections) {
            AutoAction::Pick {
                champion_id, reason, ..
            } => {
                assert_eq!(champion_id, 103);
                assert!(reason.contains("胜率最高"), "{reason}");
            }
            other => panic!("expected winrate pick, got {other:?}"),
        }

        // 样本太少(300 场以下)不采信 → 不做动作
        let thin = sections_with(103, "middle", "60.0%", 120);
        assert_eq!(
            decide(&pick_session(0, 0, 20000), &prefs, &names(), &thin),
            AutoAction::None
        );

        // 分路不匹配也不采信
        let wrong_lane = sections_with(103, "top", "60.0%", 1200);
        assert_eq!(
            decide(&pick_session(0, 0, 20000), &prefs, &names(), &wrong_lane),
            AutoAction::None
        );
    }
}
