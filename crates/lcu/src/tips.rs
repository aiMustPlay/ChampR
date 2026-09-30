//! 对位心理图谱(playstyle intel)。
//!
//! 数据源: Data Dragon `championFull.json` (zh_CN, 单文件全英雄)。
//! 每英雄带三类原生字段:
//! - `allytips` —— 这个英雄的本能动作("他会想干什么"), 读它是在读对手心理状态
//! - `enemytips` —— 官方写的对抗提示, 等同"他的弱点、你要识破的点"
//! - `blurb` / `title` —— 故事背景一句话、称号(称号比中文名承担更多叙事感, 展示用)
//!
//! 与 counter.rs 的关系: counter 引擎回答"我该生成什么符文",
//! tips 回答"我该理解对面在想什么" —— 一立一破, 对齐对位心理的四层模型(意图/窗口/欺骗/崩溃)。

use serde::Deserialize;
use std::collections::HashMap;

use crate::web::FetchError;

const DATA_DRAGON_BASE_URL: &str = "https://ddragon.leagueoflegends.com";

/// 单一英雄的心理画像。
#[derive(Debug, Clone, Default)]
pub struct ChampIntel {
    /// 英雄称号(如 "疾风剑豪" —— 比中文名更无歧义, 也引入了叙述感)
    pub title: String,
    /// 背景一句话/玩法核心句
    pub blurb: String,
    /// 他怎么玩(本能动作、心灵语言)
    pub ally_tips: Vec<String>,
    /// 你怎么打他(弱点、识破)
    pub enemy_tips: Vec<String>,
}

/// 全英雄心理图(键 = 数字 key 字符串, 如 "266")。
#[derive(Debug, Clone, Default)]
pub struct PlaystyleAtlas {
    pub champions: HashMap<String, ChampIntel>,
}

impl PlaystyleAtlas {
    /// 按数字 key 查(numeric key 与 champ-select / live snapshot 的车联网唯一锚点)。
    pub fn by_key(&self, key: &str) -> Option<&ChampIntel> {
        self.champions.get(key)
    }

    /// 拼出对位心理的(标题, 正文); 标题承担 UI 标题行, 正文是编号条目。
    /// ally/enemy 各只取前几条, 避免长文本挤压卡片布局。
    pub fn render_opponent(&self, key: &str, zh_name: &str) -> Option<(String, String)> {
        let intel = self.by_key(key)?;
        let header = format!("对位心理 · {} {}", zh_name, intel.title);
        let mut body = Vec::new();
        for (i, tip) in intel.ally_tips.iter().take(2).enumerate() {
            body.push(format!("他想 {} 「{}」", i + 1, tip));
        }
        for (i, tip) in intel.enemy_tips.iter().take(3).enumerate() {
            body.push(format!("打他 {} 「{}」", i + 1, tip));
        }
        if body.is_empty() {
            return None;
        }
        Some((header, body.join("\n")))
    }

    /// 给 LLM prompt 的紧凑注入文本(只保留信息高密度行, 不同行号)。
    pub fn render_for_prompt(&self, key: &str, zh_name: &str) -> Option<String> {
        let intel = self.by_key(key)?;
        if intel.ally_tips.is_empty() && intel.enemy_tips.is_empty() {
            return None;
        }
        let mut out = format!("{}({}) 行为参考:", zh_name, intel.title);
        for tip in intel.ally_tips.iter().take(2) {
            out.push_str(&format!("\n- (意图){tip}"));
        }
        for tip in intel.enemy_tips.iter().take(3) {
            out.push_str(&format!("\n- (软肋){tip}"));
        }
        Some(out)
    }
}

// ---------- DDragon 源解析 ----------

#[derive(Debug, Deserialize)]
struct ChampionFullResponse {
    data: HashMap<String, ChampionFullEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChampionFullEntry {
    key: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    blurb: String,
    #[serde(default)]
    allytips: Vec<String>,
    #[serde(default)]
    enemytips: Vec<String>,
}

/// 从正文解析(与 HTTP 解耦, 测试不用联网)。
pub fn parse_playstyle_atlas(body: &str) -> Result<PlaystyleAtlas, serde_json::Error> {
    let resp: ChampionFullResponse = serde_json::from_str(body)?;
    let champions = resp
        .data
        .into_values()
        .map(|entry| {
            (
                entry.key,
                ChampIntel {
                    title: entry.title,
                    blurb: entry.blurb,
                    ally_tips: entry.allytips,
                    enemy_tips: entry.enemytips,
                },
            )
        })
        .collect();
    Ok(PlaystyleAtlas { champions })
}

/// 拉取全英雄心理图(zh_CN championFull.json 单请求)。
/// 失败返回 Err(FetchError::Failed), 调用方回退空图(视为功能关闭, 与 name map 一致)。
pub async fn fetch_playstyle_atlas() -> Result<PlaystyleAtlas, FetchError> {
    let version = {
        // 复用与 web.rs 同一版本捷径: 每个版本都受检测, 弃后重新拉。
        let resp = reqwest::get(format!("{DATA_DRAGON_BASE_URL}/api/versions.json"))
            .await
            .map_err(|_| FetchError::Failed)?;
        let versions = resp
            .json::<Vec<String>>()
            .await
            .map_err(|_| FetchError::Failed)?;
        versions.into_iter().next().ok_or(FetchError::Failed)?
    };
    let url = format!("{DATA_DRAGON_BASE_URL}/cdn/{version}/data/zh_CN/championFull.json");
    let resp = reqwest::get(url).await.map_err(|_| FetchError::Failed)?;
    let body = resp.text().await.map_err(|_| FetchError::Failed)?;
    parse_playstyle_atlas(&body).map_err(|_| FetchError::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_body() -> &'static str {
        // 按真实 championFull.json 的最小合法结构捏的样例(中文走 UTF-8 验证编码不丢字)
        r#"{
            "data": {
                "Ahri": {
                    "key": "103",
                    "title": "九尾妖狐",
                    "blurb": "九尾妖狐能操控人心。",
                    "allytips": ["阿狸的技能可以对多个敌人造成伤害, 在团战中效果最佳"],
                    "enemytips": ["阿狸的魅惑E需要精准, 走位横向移动更难被命中"]
                },
                "Yasuo": {
                    "key": "157",
                    "title": "疾风剑豪",
                    "blurb": "亚索是一名御风剑客。",
                    "allytips": [],
                    "enemytips": ["斩钢闪的攻击范围非常狭窄, 尽可能靠边"]
                }
            }
        }"#
    }

    #[test]
    fn parses_champion_full_entries() {
        let atlas = parse_playstyle_atlas(sample_body()).unwrap();
        assert_eq!(atlas.champions.len(), 2);
        let ahri = atlas.by_key("103").unwrap();
        assert_eq!(ahri.title, "九尾妖狐");
        assert_eq!(ahri.enemy_tips[0], "阿狸的魅惑E需要精准, 走位横向移动更难被命中");
    }

    #[test]
    fn renders_opponent_text_within_line_budget() {
        let atlas = parse_playstyle_atlas(sample_body()).unwrap();
        let (header, body) = atlas.render_opponent("103", "阿狸").unwrap();
        assert!(header.contains("对位心理 · 阿狸 九尾妖狐"));
        let lines = body.lines().count();
        assert!(lines <= 2 + 3, "正文不应超过预算: {}", body);
        assert!(body.contains("他想 1"), "应有意图条目: {}", body);
        assert!(body.contains("打他 1"), "应有对抗条目: {}", body);
    }

    #[test]
    fn missing_key_returns_none() {
        let atlas = parse_playstyle_atlas(sample_body()).unwrap();
        assert!(atlas.render_opponent("999", "无名").is_none());
    }

    #[test]
    fn prompt_render_omits_when_all_tips_empty() {
        // 壮递的崩溃情形: blurb/have no tips
        let body = r#"{"data":{"Empty":{"key":"1","title":"","blurb":"","allytips":[],"enemytips":[]}}}"#;
        let atlas = parse_playstyle_atlas(body).unwrap();
        assert!(atlas.render_for_prompt("1", "无").is_none());
    }
}
