use bytes::Bytes;
use lazy_static::lazy_static;
use reqwest::{Client, Url};
use reqwest_websocket::{Message, RequestBuilderExt, WebSocket};
use serde::de::DeserializeOwned;
use serde_derive::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

use crate::{
    builds::{ItemBuild, Rune},
    lcu_error::LcuError,
    web::FetchError,
};

lazy_static! {
    static ref CLIENT: reqwest::Client = {
        reqwest::Client::builder()
            .use_rustls_tls()
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap()
    };
}

pub fn make_client() -> &'static reqwest::Client {
    &CLIENT
}

/// LCU 凭据 URL 的两种形态在这个仓库里长期混用:
/// 裸的 `riot:token@127.0.0.1:port`(LCU 命令行原样读出来的)和
/// 带 scheme 的 `https://riot:token@127.0.0.1:port`。
/// 裸形态直接喂 reqwest 必然 "builder error for url" —— 而且失败得很安静,
/// 谁调用谁中招(2026-10-02 手动对位 / accept 两处连环踩)。
/// 所有出网请求统一从这里过, 缺 scheme 就补 https://。
pub fn lcu_endpoint(raw: &str) -> String {
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    }
}

pub async fn make_get_request<T: DeserializeOwned>(endpoint: &String) -> Result<T, LcuError> {
    let client = make_client();
    client
        .get(lcu_endpoint(endpoint))
        .version(reqwest::Version::HTTP_2)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(LcuError::from)?
        .json()
        .await
        .map_err(LcuError::from)
}

pub async fn get_session(auth_url: &String) -> Result<Option<i64>, LcuError> {
    let endpoint = format!("{auth_url}/lol-champ-select/v1/session");
    let resp: Value = make_get_request(&endpoint).await?;

    if let Some(cell_id) = resp["localPlayerCellId"].as_i64() {
        let my_team = resp["myTeam"].as_array().unwrap();
        for i in my_team {
            let id = i["cellId"].as_i64().unwrap();
            if id == cell_id {
                let champ_id = i.get("championId").unwrap().as_i64().unwrap();
                return Ok(Some(champ_id));
            }
        }

        let actions = resp["actions"].as_array().unwrap();
        for row in actions {
            for i in row.as_array().unwrap() {
                let id = i["actorCellId"].as_i64().unwrap();
                if id == cell_id && i["type"].as_str().unwrap() != "ban" {
                    let champ_id = i.get("championId").unwrap().as_i64().unwrap();
                    return Ok(Some(champ_id));
                }
            }
        }
    };

    Ok(None)
}

/// 排队就绪自动接受对局。幂等: LCU 对重复 accept 返回 204/错误均不可见影响,
/// 调用方自己保证只在状态翻转瞬间调用一次。
pub async fn accept_ready_check(auth_url: &str) -> Result<(), LcuError> {
    let base = lcu_endpoint(auth_url);
    let client = make_client();
    client
        .post(format!("{base}/lol-matchmaking/v1/ready-check/accept"))
        .version(reqwest::Version::HTTP_2)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?;
    Ok(())
}

pub async fn get_champ_select_session(auth_url: &str) -> Result<Value, LcuError> {
    let endpoint = format!("{auth_url}/lol-champ-select/v1/session");
    make_get_request(&endpoint).await
}

pub async fn get_gameflow_phase(auth_url: &str) -> Result<String, LcuError> {
    let endpoint = format!("{auth_url}/lol-gameflow/v1/gameflow-phase");
    let phase: Value = make_get_request(&endpoint).await?;
    Ok(phase.as_str().unwrap_or("None").to_string())
}

pub async fn get_gameflow_session(auth_url: &str) -> Result<Value, LcuError> {
    let endpoint = format!("{auth_url}/lol-gameflow/v1/session");
    make_get_request(&endpoint).await
}

/// The rune page that is currently equipped in the client
/// (`/lol-perks/v1/currentpage`).
pub async fn get_current_rune_page(auth_url: &str) -> Result<Value, LcuError> {
    let endpoint = format!("{auth_url}/lol-perks/v1/currentpage");
    make_get_request(&endpoint).await
}

/// Summoner info by summoner id, used to resolve champion-select teammates.
pub async fn get_summoner_by_id(auth_url: &str, summoner_id: i64) -> Result<Value, LcuError> {
    let endpoint = format!("{auth_url}/lol-summoner/v2/summoners/{summoner_id}");
    make_get_request(&endpoint).await
}

/// Ranked stats for a player (`/lol-ranked/v1/ranked-stats/{puuid}`).
pub async fn get_ranked_stats(auth_url: &str, puuid: &str) -> Result<Value, LcuError> {
    let endpoint = format!("{auth_url}/lol-ranked/v1/ranked-stats/{puuid}");
    make_get_request(&endpoint).await
}

pub async fn apply_rune(endpoint: String, rune: Rune) -> Result<(), LcuError> {
    let endpoint = lcu_endpoint(&endpoint);
    let runes: Value = make_get_request(&format!("{endpoint}/lol-perks/v1/pages")).await?;

    let mut id = 0;
    for r in runes.as_array().unwrap() {
        if r["current"].as_bool().unwrap() {
            id = r["id"].as_i64().unwrap();
            break;
        }
        if r["isDeletable"].as_bool().unwrap() {
            id = r["id"].as_i64().unwrap();
        }
    }

    let client = make_client();
    if id > 0 {
        let _ = client
            .delete(format!("{endpoint}/lol-perks/v1/pages/{id}"))
            .version(reqwest::Version::HTTP_2)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
    }

    let _ = client
        .post(format!("{endpoint}/lol-perks/v1/pages"))
        .version(reqwest::Version::HTTP_2)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&rune)
        .send()
        .await?;
    Ok(())
}

pub async fn appy_rune_and_builds(
    _endpoint: String,
    _rune: Rune,
    _builds: Vec<ItemBuild>,
) -> Result<(), LcuError> {
    Ok(())
}

pub async fn get_rune_image(endpoint: String, icon_path: String) -> Result<Bytes, FetchError> {
    let client = make_client();
    let url = format!("{}/lol-game-data/assets/v1/{icon_path}", lcu_endpoint(&endpoint));
    if let Ok(resp) = client.get(&url).send().await {
        if let Ok(bytes) = resp.bytes().await {
            return Ok(bytes);
        }
    }

    Err(FetchError::Failed)
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summoner {
    pub account_id: i64,
    pub display_name: String,
    pub game_name: String,
    pub internal_name: String,
    pub name_change_flag: bool,
    pub percent_complete_for_next_level: i64,
    pub privacy: String,
    pub profile_icon_id: i64,
    pub puuid: String,
    pub reroll_points: RerollPoints,
    pub summoner_id: i64,
    pub summoner_level: i64,
    pub tag_line: String,
    pub unnamed: bool,
    pub xp_since_last_level: i64,
    pub xp_until_next_level: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RerollPoints {
    pub current_points: i64,
    pub max_rolls: i64,
    pub number_of_rolls: i64,
    pub points_cost_to_roll: i64,
    pub points_to_reroll: i64,
}

pub async fn get_current_summoner(endpoint: &String) -> Result<Summoner, LcuError> {
    let url = format!("{endpoint}/lol-summoner/v1/current-summoner");
    make_get_request(&url).await
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummonerChampion {
    pub active: bool,
    pub alias: String,
    pub ban_vo_path: String,
    pub base_load_screen_path: String,
    pub base_splash_path: String,
    pub bot_enabled: bool,
    pub choose_vo_path: String,
    pub disabled_queues: Vec<Value>,
    pub free_to_play: bool,
    pub id: i64,
    pub name: String,
    pub ownership: Ownership,
    pub passive: Passive,
    // pub purchased: i64,
    pub ranked_play_enabled: bool,
    pub roles: Vec<Value>,
    pub skins: Vec<Value>,
    pub spells: Vec<Value>,
    pub square_portrait_path: String,
    pub stinger_sfx_path: String,
    pub tactical_info: TacticalInfo,
    pub title: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ownership {
    pub loyalty_reward: bool,
    pub owned: bool,
    pub rental: Rental,
    #[serde(rename = "xboxGPReward")]
    pub xbox_gpreward: bool,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rental {
    pub end_date: i64,
    // pub purchase_date: i64,
    pub rented: bool,
    pub win_count_remaining: i64,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Passive {
    pub description: String,
    pub name: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TacticalInfo {
    pub damage_type: String,
    pub difficulty: i64,
    pub style: i64,
}

pub async fn list_available_champions(
    endpoint: &String,
    summoner_id: i64,
) -> Result<Vec<SummonerChampion>, LcuError> {
    let url = format!("{endpoint}/lol-champions/v1/inventories/{summoner_id}/champions");
    make_get_request(&url).await
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Perk {
    pub icon_path: String,
    pub id: i64,
    pub long_desc: String,
    pub name: String,
    pub recommendation_descriptor: String,
    pub short_desc: String,
    pub slot_type: String,
    pub style_id: i64,
    pub style_id_name: String,
    pub tooltip: String,
}

pub async fn list_all_perks(endpoint: &String) -> Result<Vec<Perk>, LcuError> {
    let url = format!("{endpoint}/lol-perks/v1/perks");
    make_get_request(&url).await
}

pub async fn fetch_image_data(url: &String) -> Result<Bytes, FetchError> {
    let client = make_client();
    let url = lcu_endpoint(url);
    match client.get(&url).send().await.map_err(|_| FetchError::Failed) {
        Ok(res) => {
            if res.status().is_success() {
                return res.bytes().await.map_err(|_| FetchError::Failed);
            }
            Err(FetchError::Failed)
        }
        Err(err) => {
            println!("Error fetching rune image: {:?}", err);
            Err(FetchError::Failed)
        }
    }
}

pub async fn fetch_rune_image(endpoint: &String, icon_path: &String) -> Result<Bytes, FetchError> {
    let url = format!("{endpoint}{icon_path}");
    fetch_image_data(&url).await
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuneStyle {
    pub allowed_sub_styles: Vec<i64>,
    pub asset_map: HashMap<String, String>,
    pub default_page_name: String,
    pub default_perks: Vec<i64>,
    pub default_sub_style: i64,
    pub icon_path: String,
    pub id: i64,
    pub id_name: String,
    pub name: String,
    pub slots: Vec<Slot>,
    pub sub_style_bonus: Vec<SubStyleBonu>,
    pub tooltip: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    pub perks: Vec<i64>,
    pub slot_label: String,
    #[serde(rename = "type")]
    pub type_field: String,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubStyleBonu {
    pub perk_id: i64,
    pub style_id: i64,
}

pub async fn list_all_styles(endpoint: &String) -> Result<Vec<RuneStyle>, LcuError> {
    let url = format!("{endpoint}/lol-perks/v1/styles");
    make_get_request(&url).await
}

pub async fn get_champion_icon_by_id(endpoint: &String, id: i64) -> Result<Bytes, FetchError> {
    let url = format!("{endpoint}/lol-game-data/assets/v1/champion-icons/{id}.png");
    fetch_image_data(&url).await
}

pub async fn make_ws_client(endpoint: &String) -> Result<WebSocket, reqwest_websocket::Error> {
    let url = format!("wss://{endpoint}");
    let client = Client::default();
    let response = client.get(url).upgrade().send().await?;
    let ws = response.into_websocket().await?;
    Ok(ws)
}

pub fn make_sub_msg() -> Message {
    Message::Text("[5, \"OnJsonApiEvent\"]".into())
}

pub fn make_champion_avatar_url(endpoint: &String, id: u64) -> Url {
    format!("{}/lol-game-data/assets/v1/champion-icons/{id}.png", lcu_endpoint(endpoint))
        .parse::<Url>()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::lcu_endpoint;

    /// 2026-10-02 事故回归: LCU 命令行读出来的凭据是裸的
    /// `riot:token@127.0.0.1:port`。它其实能被 Url 解析成 scheme="riot",
    /// 所以错误不会发生在解析阶段, 而是 reqwest 发请求时报
    /// "builder error for url" —— 极其难查。补 https:// 后才是合法请求目标。
    #[test]
    fn bare_credentials_url_is_not_directly_requestable() {
        let bare = "riot:abc@127.0.0.1:60092/lol-champ-select/v1/session";
        let parsed = bare.parse::<reqwest::Url>().unwrap();
        assert_ne!(parsed.scheme(), "https", "裸形态的 scheme 不是 https, reqwest 会拒绝");
        assert_eq!(parsed.scheme(), "riot");
    }

    #[test]
    fn lcu_endpoint_normalizes_both_forms() {
        let bare = "riot:abc@127.0.0.1:60092";
        let full = "https://riot:abc@127.0.0.1:60092";
        assert_eq!(lcu_endpoint(bare), full);
        // 已经带 scheme 的原样返回, 不重复叠加
        assert_eq!(lcu_endpoint(full), full);
        assert_eq!(lcu_endpoint(bare), lcu_endpoint(full));
    }

    #[test]
    fn normalized_endpoint_is_requestable_with_auth_in_userinfo() {
        let url = format!(
            "{}/lol-champ-select/v1/session",
            lcu_endpoint("riot:abc@127.0.0.1:60092")
        );
        let parsed = url.parse::<reqwest::Url>().unwrap();
        assert_eq!(parsed.scheme(), "https");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(60092));
        assert_eq!(parsed.username(), "riot");
        assert_eq!(parsed.password(), Some("abc"));
    }
}
