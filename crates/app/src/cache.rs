//! 磁盘缓存: 段位/个人胜率(按 summonerId) 与 OP.GG 分路数据(按英雄 id)。
//!
//! 用户 2026-10-05: "每次对局都要重新查一遍胜率" —— 两个原因:
//!   1. 缓存只在内存, app 一重启(启动器重启/手动重启)就全部重查
//!   2. 对局任务每 2.5s 为全部英雄调一次 `ensure_opgg_sections`, 失败/无数据的英雄会一直重试
//! 这个模块解决第 1 条: 落盘 + TTL, 熟面孔(固定车队/重开)直接命中, 不再重复请求。
//!
//! 位置: `%APPDATA%\champr\cache\`. 写失败只记日志, 绝不影响对局功能。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use lcu::advisor::RankInfo;
use lcu::builds::BuildSection;

/// 段位/个人胜率: 一天内不重复查(排名与赛季胜率变化很慢)。
pub const RANKS_TTL_SECS: u64 = 24 * 60 * 60;
/// OP.GG 分路数据: 6 小时内不重复查(爬虫更新的是服务端数据, 客户端没必要频繁回源)。
pub const SECTIONS_TTL_SECS: u64 = 6 * 60 * 60;
/// 完全没拿到数据时的重试间隔(避免每 2.5s 一次的无效请求)。
pub const RETRY_AFTER_SECS: u64 = 10 * 60;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_dir() -> PathBuf {
    let mut dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("champr");
    dir.push("cache");
    dir
}

fn ranks_path() -> PathBuf {
    cache_dir().join("ranked-stats.json")
}

fn sections_path() -> PathBuf {
    cache_dir().join("opgg-sections.json")
}

/// 一条段位记录 + 抓取时间。
#[derive(serde::Serialize, serde::Deserialize)]
struct RankEntry {
    rank: String,
    #[serde(default)]
    personal_rate: String,
    #[serde(default)]
    personal_record: String,
    #[serde(default)]
    at: u64,
}

/// 一个英雄的分路数据 + 抓取时间。
#[derive(serde::Serialize, serde::Deserialize)]
struct SectionEntry {
    #[serde(default)]
    at: u64,
    sections: Vec<BuildSection>,
}

fn write_json<T: serde::Serialize>(path: &PathBuf, value: &T, what: &str) {
    let Some(parent) = path.parent() else { return };
    if let Err(err) = std::fs::create_dir_all(parent) {
        log::warn!("cache: 建目录失败 {parent:?}: {err}");
        return;
    }
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            if let Err(err) = std::fs::write(path, bytes) {
                log::warn!("cache: 写 {what} 失败 {path:?}: {err}");
            }
        }
        Err(err) => log::warn!("cache: 序列化 {what} 失败: {err}"),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf, what: &str) -> Option<T> {
    let text = std::fs::read(path).ok()?;
    match serde_json::from_slice::<T>(&text) {
        Ok(value) => Some(value),
        Err(err) => {
            // 缓存坏了就当没有, 不能影响启动
            log::warn!("cache: 解析 {what} 失败({err}), 忽略 {path:?}");
            None
        }
    }
}

/// 读段位缓存(默认路径)。
pub fn load_ranks() -> (HashMap<i64, RankInfo>, HashMap<i64, u64>) {
    load_ranks_from(&ranks_path())
}

/// 读段位缓存: 过期条目直接丢弃。返回 (summonerId -> RankInfo, 抓取时间)。
pub fn load_ranks_from(path: &PathBuf) -> (HashMap<i64, RankInfo>, HashMap<i64, u64>) {
    let mut infos = HashMap::new();
    let mut times = HashMap::new();
    let Some(entries) = read_json::<HashMap<String, RankEntry>>(path, "段位缓存") else {
        return (infos, times);
    };
    let now = now_secs();
    let mut fresh = 0;
    for (key, entry) in entries {
        let Ok(summoner_id) = key.parse::<i64>() else {
            continue;
        };
        if entry.at > 0 && now.saturating_sub(entry.at) > RANKS_TTL_SECS {
            continue;
        }
        fresh += 1;
        times.insert(summoner_id, entry.at);
        infos.insert(
            summoner_id,
            RankInfo {
                rank: entry.rank,
                personal_rate: entry.personal_rate,
                personal_record: entry.personal_record,
            },
        );
    }
    if fresh > 0 {
        log::info!("cache: 段位缓存命中 {fresh} 条(有效期内, 不再重复查)");
    }
    (infos, times)
}

/// 写段位缓存(默认路径)。
pub fn save_ranks(infos: &HashMap<i64, RankInfo>, times: &HashMap<i64, u64>) {
    save_ranks_to(&ranks_path(), infos, times);
}

/// 写段位缓存(只写未过期的)。
pub fn save_ranks_to(path: &PathBuf, infos: &HashMap<i64, RankInfo>, times: &HashMap<i64, u64>) {
    let now = now_secs();
    let mut out: HashMap<String, RankEntry> = HashMap::new();
    for (summoner_id, info) in infos {
        let at = times.get(summoner_id).copied().unwrap_or(now);
        if at > 0 && now.saturating_sub(at) > RANKS_TTL_SECS {
            continue;
        }
        out.insert(
            summoner_id.to_string(),
            RankEntry {
                rank: info.rank.clone(),
                personal_rate: info.personal_rate.clone(),
                personal_record: info.personal_record.clone(),
                at,
            },
        );
    }
    write_json(path, &out, "段位缓存");
}

/// 读 OP.GG 分路缓存(默认路径)。
pub fn load_sections() -> (HashMap<i64, Vec<BuildSection>>, HashMap<i64, u64>) {
    load_sections_from(&sections_path())
}

/// 读 OP.GG 分路缓存: 过期条目丢弃。
pub fn load_sections_from(path: &PathBuf) -> (HashMap<i64, Vec<BuildSection>>, HashMap<i64, u64>) {
    let mut sections = HashMap::new();
    let mut times = HashMap::new();
    let Some(entries) = read_json::<HashMap<String, SectionEntry>>(path, "OP.GG 缓存") else {
        return (sections, times);
    };
    let now = now_secs();
    for (key, entry) in entries {
        let Ok(champion_id) = key.parse::<i64>() else {
            continue;
        };
        if entry.sections.is_empty() {
            continue;
        }
        if entry.at > 0 && now.saturating_sub(entry.at) > SECTIONS_TTL_SECS {
            continue;
        }
        times.insert(champion_id, entry.at);
        sections.insert(champion_id, entry.sections);
    }
    if !sections.is_empty() {
        log::info!(
            "cache: OP.GG 分路缓存命中 {} 个英雄(有效期内, 不再回源)",
            sections.len()
        );
    }
    (sections, times)
}

/// 写 OP.GG 分路缓存(默认路径)。
pub fn save_sections(sections: &HashMap<i64, Vec<BuildSection>>, times: &HashMap<i64, u64>) {
    save_sections_to(&sections_path(), sections, times);
}

/// 写 OP.GG 分路缓存。
pub fn save_sections_to(
    path: &PathBuf,
    sections: &HashMap<i64, Vec<BuildSection>>,
    times: &HashMap<i64, u64>,
) {
    let now = now_secs();
    let mut out: HashMap<String, SectionEntry> = HashMap::new();
    for (champion_id, list) in sections {
        if list.is_empty() {
            continue;
        }
        let at = times.get(champion_id).copied().unwrap_or(now);
        if at > 0 && now.saturating_sub(at) > SECTIONS_TTL_SECS {
            continue;
        }
        out.insert(
            champion_id.to_string(),
            SectionEntry {
                at,
                sections: list.clone(),
            },
        );
    }
    write_json(path, &out, "OP.GG 缓存");
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcu::builds::BuildSection;

    fn section(alias: &str, rate: &str) -> BuildSection {
        BuildSection {
            alias: alias.to_string(),
            name: alias.to_string(),
            win_rate: rate.to_string(),
            position: "middle".to_string(),
            pick_count: 1234,
            ..Default::default()
        }
    }

    #[test]
    fn rank_cache_round_trips_through_disk() {
        // 直接测序列化/反序列化(TTL 判定是纯函数)
        let entry = RankEntry {
            rank: "黄金 II".to_string(),
            personal_rate: "55%".to_string(),
            personal_record: "120胜98负".to_string(),
            at: now_secs(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RankEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rank, "黄金 II");
        assert_eq!(back.personal_rate, "55%");
        assert_eq!(back.personal_record, "120胜98负");
        assert_eq!(back.at, entry.at);
    }

    #[test]
    fn expired_entries_are_dropped() {
        let now = now_secs();
        // 段位: 超过 TTL 视为过期
        assert!(now.saturating_sub(now) <= RANKS_TTL_SECS);
        let stale = now.saturating_sub(RANKS_TTL_SECS + 1);
        assert!(now.saturating_sub(stale) > RANKS_TTL_SECS);
        let stale_sections = now.saturating_sub(SECTIONS_TTL_SECS + 1);
        assert!(now.saturating_sub(stale_sections) > SECTIONS_TTL_SECS);
    }

    /// 真实走一次磁盘: 写出去再读回来(用户要的就是"不要每局重查", 落盘必须可靠)。
    #[test]
    fn caches_round_trip_through_real_files() {
        let dir = std::env::temp_dir().join(format!("champr-cache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ranks = dir.join("ranked-stats.json");
        let sections = dir.join("opgg-sections.json");
        let _ = std::fs::remove_file(&ranks);
        let _ = std::fs::remove_file(&sections);

        // 段位: 两个玩家, 其中一个时间戳写成很久以前(应被丢弃)
        let now = now_secs();
        let mut infos = HashMap::new();
        infos.insert(
            11,
            RankInfo {
                rank: "黄金 II".to_string(),
                personal_rate: "55%".to_string(),
                personal_record: "120胜98负".to_string(),
            },
        );
        infos.insert(
            22,
            RankInfo {
                rank: "白银 I".to_string(),
                personal_rate: "48%".to_string(),
                personal_record: "50胜54负".to_string(),
            },
        );
        let mut times = HashMap::new();
        times.insert(11, now);
        times.insert(22, now.saturating_sub(RANKS_TTL_SECS + 10));
        save_ranks_to(&ranks, &infos, &times);

        let (loaded, loaded_times) = load_ranks_from(&ranks);
        assert_eq!(loaded.len(), 1, "过期条目应被丢弃");
        assert_eq!(loaded.get(&11).unwrap().personal_rate, "55%");
        assert_eq!(loaded.get(&11).unwrap().rank, "黄金 II");
        assert_eq!(loaded_times.get(&11).copied(), Some(now));
        assert!(!loaded.contains_key(&22));

        // OP.GG 分路数据
        let mut sec_map = HashMap::new();
        sec_map.insert(103, vec![section("ahri", "51.21%")]);
        let mut sec_times = HashMap::new();
        sec_times.insert(103, now);
        save_sections_to(&sections, &sec_map, &sec_times);

        let (loaded_sections, loaded_times) = load_sections_from(&sections);
        assert_eq!(loaded_sections.len(), 1);
        assert_eq!(loaded_sections.get(&103).unwrap()[0].win_rate, "51.21%");
        assert_eq!(loaded_times.get(&103).copied(), Some(now));

        // 坏文件不能让启动崩: 当成空缓存
        std::fs::write(&ranks, b"{ this is not json").unwrap();
        let (broken, _) = load_ranks_from(&ranks);
        assert!(broken.is_empty(), "坏缓存应被忽略而不是 panic");

        let _ = std::fs::remove_file(&ranks);
        let _ = std::fs::remove_file(&sections);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn section_cache_keeps_payload() {
        let entry = SectionEntry {
            at: now_secs(),
            sections: vec![section("ahri", "51.21%")],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: SectionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sections.len(), 1);
        assert_eq!(back.sections[0].win_rate, "51.21%");
    }
}
