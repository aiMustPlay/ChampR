#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kv_log_macro::{info, warn};
use slint::{ComponentHandle, Image, ModelRc, SharedPixelBuffer, SharedString, VecModel, Weak};

use lcu::{
    advisor,
    builds::Rune,
    cmd::{get_cmd_output, get_lcu_process_id},
    deepseek::{ChatMessage, DeepSeekClient, DeepSeekConfig},
    lcu_api::{self, make_sub_msg},
    live_client,
    reqwest_websocket::Message,
    serde_json::{from_str, Value},
    tts,
    web::{self, ChampionsMap},
};

slint::include_modules!();
mod settings;

#[allow(dead_code)]
const DEFAULT_SOURCE_LABEL: &str = "OP.GG";
const DEFAULT_SOURCE_VALUE: &str = "op.gg";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchPhase {
    Idle,
    ChampSelect,
    GameStart,
    InProgress,
    Ended,
}

// ---------------------------------------------------------------------------
//  Shared state accessible from both the UI thread and tokio tasks
// ---------------------------------------------------------------------------

/// Auth URL for the running League Client (e.g. "riot:token@127.0.0.1:port").
/// Empty string means no client detected.
struct AppState {
    auth_url: String,
    is_tencent: bool,
    lol_dir: String,
    champions_map: ChampionsMap,
    /// Runes for the currently displayed champion, kept so we can index into them.
    current_runes: Vec<Rune>,
    /// Champion currently selected in the League client, if any.
    current_champion_id: i64,
    /// Data Dragon champion id used as the backend alias (e.g. "Aatrox").
    current_champion_alias: String,
    /// TTS voice configuration used by the advice loop.
    tts_config: tts::TtsConfig,
    /// User-configurable LoL launcher path.
    lol_launcher_path: String,
    /// DeepSeek configuration used by the advice loop.
    deepseek_config: DeepSeekConfig,
    /// Whether LLM-based match assistance is enabled.
    llm_assistance_enabled: bool,
    /// AI provider: "deepseek" or "lmstudio".
    ai_provider: String,
    /// LM Studio local OpenAI-compatible config.
    lmstudio_config: DeepSeekConfig,
    /// Conversation history shared by automatic advice and the coach chat panel.
    coach_messages: Vec<ChatMessage>,
    /// Last successfully built match context, used as a fallback for follow-up questions.
    coach_last_prompt: String,
    /// Prevents overlapping coach chat requests.
    coach_busy: bool,
    /// Cached Data Dragon item names used to enrich the live-game prompt.
    item_names: HashMap<String, String>,
    /// Stable identifier for the current match session.
    match_id: String,
    /// Current observed gameflow/live-client phase.
    match_phase: MatchPhase,
    /// Last compact progress snapshot key, used to avoid sending duplicate snapshots.
    last_progress_key: String,
    /// Human-readable log shown in the single Coach Chat output box.
    ui_log: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            auth_url: String::new(),
            is_tencent: false,
            lol_dir: String::new(),
            champions_map: ChampionsMap::new(),
            current_runes: Vec::new(),
            current_champion_id: 0,
            current_champion_alias: String::new(),
            tts_config: tts::TtsConfig::default(),
            lol_launcher_path: r"C:\WeGameApps\英雄联盟（含经典模式）\WeGameLauncher\launcher.exe".to_string(),
            deepseek_config: DeepSeekConfig {
                api_key: String::new(),
                base_url: "https://api.deepseek.com".to_string(),
                model: "deepseek-v4-flash".to_string(),
                thinking_enabled: false,
                reasoning_effort: "high".to_string(),
                stream_enabled: false,
            },
            llm_assistance_enabled: false,
            ai_provider: "deepseek".to_string(),
            lmstudio_config: DeepSeekConfig {
                api_key: String::new(),
                base_url: "http://localhost:1234/v1".to_string(),
                model: "local-model".to_string(),
                thinking_enabled: false,
                reasoning_effort: String::new(),
                stream_enabled: false,
            },
            coach_messages: Vec::new(),
            coach_last_prompt: String::new(),
            coach_busy: false,
            item_names: HashMap::new(),
            match_id: String::new(),
            match_phase: MatchPhase::Idle,
            last_progress_key: String::new(),
            ui_log: String::new(),
        }
    }
}

impl AppState {
    fn reset_match_session(&mut self) {
        self.coach_messages.clear();
        self.coach_last_prompt.clear();
        self.match_id.clear();
        self.match_phase = MatchPhase::Idle;
        self.last_progress_key.clear();
        self.ui_log.clear();
    }

    fn start_match_session(&mut self, phase: MatchPhase) {
        self.coach_messages.clear();
        self.coach_last_prompt.clear();
        self.match_id = format!(
            "match-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        self.match_phase = phase;
        self.last_progress_key.clear();
        self.ui_log.clear();
    }
}

type SharedState = Arc<Mutex<AppState>>;

// ---------------------------------------------------------------------------
//  main
// ---------------------------------------------------------------------------

fn main() {
    femme::with_level(femme::LevelFilter::Info);

    // -- Create windows --
    let sources_window = SourcesWindow::new().unwrap();
    let runes_window = RunesWindow::new().unwrap();
    let tts_settings_window = TtsSettingsWindow::new().unwrap();

    let saved_settings = settings::Settings::load();
    let mut initial_state = AppState::default();
    initial_state.tts_config = tts::TtsConfig {
        rate: saved_settings.tts_rate,
        volume: saved_settings.tts_volume,
        voice: if saved_settings.tts_voice.is_empty() {
            None
        } else {
            Some(saved_settings.tts_voice.clone())
        },
    };
    initial_state.lol_launcher_path = saved_settings.lol_launcher_path.clone();
    initial_state.deepseek_config = DeepSeekConfig {
        api_key: saved_settings.deepseek_api_key.clone(),
        base_url: saved_settings.deepseek_base_url.clone(),
        model: saved_settings.deepseek_model.clone(),
        thinking_enabled: saved_settings.deepseek_thinking,
        reasoning_effort: saved_settings.deepseek_reasoning_effort.clone(),
        stream_enabled: saved_settings.deepseek_stream,
    };
    initial_state.ai_provider = saved_settings.ai_provider.clone();
    initial_state.lmstudio_config = DeepSeekConfig {
        api_key: saved_settings.lmstudio_api_key.clone(),
        base_url: saved_settings.lmstudio_base_url.clone(),
        model: saved_settings.lmstudio_model.clone(),
        thinking_enabled: false,
        reasoning_effort: String::new(),
        stream_enabled: false,
    };
    let state: SharedState = Arc::new(Mutex::new(initial_state));

    tts_settings_window.set_tts_rate(saved_settings.tts_rate);
    tts_settings_window.set_tts_volume(saved_settings.tts_volume);
    tts_settings_window.set_tts_voice(SharedString::from(&saved_settings.tts_voice));
    tts_settings_window.set_lol_launcher_path(SharedString::from(&saved_settings.lol_launcher_path));
    tts_settings_window.set_deepseek_api_key(SharedString::from(&saved_settings.deepseek_api_key));
    tts_settings_window.set_deepseek_model(SharedString::from(&saved_settings.deepseek_model));
    tts_settings_window.set_deepseek_thinking(saved_settings.deepseek_thinking);
    tts_settings_window.set_deepseek_stream(saved_settings.deepseek_stream);
    tts_settings_window.set_deepseek_reasoning_effort(SharedString::from(
        &saved_settings.deepseek_reasoning_effort,
    ));
    tts_settings_window.set_ai_provider(SharedString::from(&saved_settings.ai_provider));
    tts_settings_window.set_lmstudio_base_url(SharedString::from(&saved_settings.lmstudio_base_url));
    tts_settings_window.set_lmstudio_model(SharedString::from(&saved_settings.lmstudio_model));
    tts_settings_window.set_lmstudio_api_key(SharedString::from(&saved_settings.lmstudio_api_key));

    // -- Apply Builds button --
    let state_c = state.clone();
    let sources_weak = sources_window.as_weak();
    let rt_handle = tokio::runtime::Runtime::new().unwrap();
    // We need the runtime handle to spawn from callbacks
    let rt_handle_ref = rt_handle.handle().clone();

    sources_window.on_apply_builds_clicked({
        let state_c = state_c.clone();
        let weak = sources_weak.clone();
        let handle = rt_handle_ref.clone();
        move || {
            let (champion_alias, current_champion_id, dir, is_tencent, auth_url) = {
                let s = state_c.lock().unwrap();
                (
                    s.current_champion_alias.clone(),
                    s.current_champion_id,
                    s.lol_dir.clone(),
                    s.is_tencent,
                    s.auth_url.clone(),
                )
            };
            info!(
                "apply builds clicked: alias={:?}, id={}, dir={:?}, tencent={}, auth={}",
                champion_alias,
                current_champion_id,
                dir,
                is_tencent,
                !auth_url.is_empty()
            );

            if dir.is_empty() {
                let w = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = w.upgrade() {
                        win.set_apply_status(SharedString::from(
                            "League Client directory not found",
                        ));
                    }
                });
                return;
            }

            let source = DEFAULT_SOURCE_VALUE.to_string();

            // Set applying state
            let w = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    win.set_applying_builds(true);
                    win.set_apply_status(SharedString::from("Applying builds…"));
                }
            });

            let weak2 = weak.clone();
            handle.spawn(async move {
                let mut champion_id = current_champion_id;
                if champion_id == 0 && !auth_url.is_empty() {
                    let endpoint = format!("https://{auth_url}");
                    let session = lcu_api::get_session(&endpoint).await;
                    info!("apply builds LCU session: {:?}", session);
                    if let Ok(Some(cid)) = session {
                        champion_id = cid;
                    }
                }

                if champion_id == 0 && champion_alias.is_empty() {
                    let w = weak2.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = w.upgrade() {
                            win.set_applying_builds(false);
                            win.set_apply_status(SharedString::from("Select a champion first"));
                        }
                    });
                    return;
                }

                let result = if champion_id > 0 {
                    lcu::builds::apply_builds_from_id(
                        &dir,
                        &source,
                        champion_id,
                        is_tencent,
                    )
                    .await
                } else {
                    lcu::builds::apply_builds_from_source(
                        &dir,
                        &source,
                        &champion_alias,
                        is_tencent,
                    )
                    .await
                };

                let champion_label = if champion_id > 0 {
                    champion_id.to_string()
                } else {
                    champion_alias.clone()
                };
                info!(
                    "apply builds result for {}: {:?}",
                    champion_label,
                    result.as_ref().err()
                );

                let apply_ok = result.is_ok();
                let msg = match result {
                    Ok(()) => format!("Done! Applied builds for {}", champion_label),
                    Err(_) => format!("Error applying builds for {}", champion_label),
                };

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak2.upgrade() {
                        win.set_applying_builds(false);
                        win.set_apply_ok(apply_ok);
                        win.set_apply_status(SharedString::from(&msg));
                    }
                });
            });
        }
    });

    // -- Launch LoL client --
    let launch_weak = sources_weak.clone();
    let launch_state = state.clone();
    sources_window.on_launch_lol_clicked(move || {
        if lcu::cmd::check_if_lol_running() {
            let w = launch_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    win.set_launch_ok(true);
                    win.set_launch_status(SharedString::from("LoL 客户端已在运行"));
                }
            });
            return;
        }

        let w = launch_weak.clone();
        let preferred_path = {
            let s = launch_state.lock().unwrap();
            s.lol_launcher_path.clone()
        };
        let result = if preferred_path.trim().is_empty() {
            lcu::cmd::launch_lol_client()
        } else {
            lcu::cmd::launch_lol_client_with_path(Some(preferred_path.as_str()))
        };
        let launch_ok = result.is_ok();
        let msg = match result {
            Ok(path) => format!("LoL 客户端已启动: {}", path.display()),
            Err(err) => format!("无法启动 LoL 客户端: {err}"),
        };

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = w.upgrade() {
                win.set_launch_ok(launch_ok);
                win.set_launch_status(SharedString::from(&msg));
            }
        });
    });

    let tts_window_for_open = tts_settings_window.as_weak();
    sources_window.on_open_tts_settings_clicked(move || {
        if let Some(win) = tts_window_for_open.upgrade() {
            win.show().unwrap();
        }
    });

    let llm_assistance_state = state.clone();
    let llm_assistance_weak = sources_window.as_weak();
    let llm_assistance_handle = rt_handle_ref.clone();
    sources_window.on_llm_assistance_changed(move |enabled| {
        llm_assistance_state.lock().unwrap().llm_assistance_enabled = enabled;
        if enabled {
            llm_assistance_handle.spawn(greet_coach(
                llm_assistance_weak.clone(),
                llm_assistance_state.clone(),
            ));
        }
    });

    let coach_weak = sources_window.as_weak();
    let coach_state = state.clone();
    let coach_handle = rt_handle_ref.clone();
    sources_window.on_coach_send_clicked(move || {
        let Some(win) = coach_weak.upgrade() else {
            return;
        };
        let input = win.get_coach_input().to_string();
        if input.trim().is_empty() {
            return;
        }

        {
            let mut s = coach_state.lock().unwrap();
            if s.coach_busy {
                return;
            }
            s.coach_busy = true;
        }

        let weak = coach_weak.clone();
        let state = coach_state.clone();
        let weak_for_ui = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak_for_ui.upgrade() {
                win.set_coach_busy(true);
            }
        });

        coach_handle.spawn(async move {
            let result = send_coach_message(&weak, &state, input).await;

            {
                let mut s = state.lock().unwrap();
                s.coach_busy = false;
            }

            let weak2 = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak2.upgrade() {
                    win.set_coach_busy(false);
                }
            });

            if let Err(err) = result {
                let message = format!("System: Coach error: {err}");
                append_system_log(&weak, &state, &message);
            }
        });
    });

    let info_weak = sources_window.as_weak();
    let info_state = state.clone();

    sources_window.on_lcu_info_clicked({
        let weak = info_weak.clone();
        let state = info_state.clone();
        move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let status = win.get_lcu_status().to_string();
            let summoner = win.get_lcu_summoner().to_string();
            let text = match status.as_str() {
                    "connected" => format!("League Client: {summoner}"),
                    "authorizing" => "League Client detected. Requesting admin access...".to_string(),
                    "needs-admin" => {
                        "League Client detected. Admin access is required once to read LCU credentials.".to_string()
                    }
                    _ => "League Client not detected".to_string(),
            };
            append_info_log(&weak, &state, &text);
        }
    });

    sources_window.on_launch_info_clicked({
        let weak = info_weak.clone();
        let state = info_state.clone();
        move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let text = win.get_launch_status().to_string();
            if !text.is_empty() {
                append_info_log(&weak, &state, &text);
            }
        }
    });

    sources_window.on_apply_info_clicked({
        let weak = info_weak.clone();
        let state = info_state.clone();
        move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let text = win.get_apply_status().to_string();
            if !text.is_empty() {
                append_info_log(&weak, &state, &text);
            }
        }
    });

    sources_window.on_advice_info_clicked({
        let weak = info_weak.clone();
        let state = info_state.clone();
        move || {
            let Some(win) = weak.upgrade() else {
                return;
            };
            let text = win.get_advice_text().to_string();
            if !text.is_empty() {
                append_info_log(&weak, &state, &text);
            }
        }
    });

    let tts_window_for_apply = tts_settings_window.as_weak();
    let tts_state_for_apply = state.clone();
    tts_settings_window.on_apply_clicked(move || {
        let Some(win) = tts_window_for_apply.upgrade() else {
            return;
        };

        let rate = win.get_tts_rate();
        let volume = win.get_tts_volume();
        let voice = win.get_tts_voice().to_string();
        let launcher_path = win.get_lol_launcher_path().to_string();
        let deepseek_api_key = win.get_deepseek_api_key().to_string();
        let deepseek_model = win.get_deepseek_model().to_string();
        let deepseek_thinking = win.get_deepseek_thinking();
        let deepseek_stream = win.get_deepseek_stream();
        let deepseek_reasoning_effort = win.get_deepseek_reasoning_effort().to_string();
        let ai_provider = win.get_ai_provider().to_string();
        let lmstudio_base_url = win.get_lmstudio_base_url().to_string();
        let lmstudio_model = win.get_lmstudio_model().to_string();
        let lmstudio_api_key = win.get_lmstudio_api_key().to_string();

        {
            let mut state = tts_state_for_apply.lock().unwrap();
            state.tts_config = tts::TtsConfig {
                rate,
                volume,
                voice: if voice.is_empty() { None } else { Some(voice.clone()) },
            };
            state.lol_launcher_path = launcher_path.clone();
            state.deepseek_config = DeepSeekConfig {
                api_key: deepseek_api_key.clone(),
                base_url: "https://api.deepseek.com".to_string(),
                model: deepseek_model.clone(),
                thinking_enabled: deepseek_thinking,
                reasoning_effort: deepseek_reasoning_effort.clone(),
                stream_enabled: deepseek_stream,
            };
            state.ai_provider = ai_provider.clone();
            state.lmstudio_config = DeepSeekConfig {
                api_key: lmstudio_api_key.clone(),
                base_url: lmstudio_base_url.clone(),
                model: lmstudio_model.clone(),
                thinking_enabled: false,
                reasoning_effort: String::new(),
                stream_enabled: false,
            };
        }

        let mut settings = settings::Settings::load();
        settings.tts_rate = rate;
        settings.tts_volume = volume;
        settings.tts_voice = voice;
        settings.lol_launcher_path = launcher_path;
        settings.deepseek_api_key = deepseek_api_key;
        settings.deepseek_model = deepseek_model;
        settings.deepseek_thinking = deepseek_thinking;
        settings.deepseek_stream = deepseek_stream;
        settings.deepseek_reasoning_effort = deepseek_reasoning_effort;
        settings.ai_provider = ai_provider;
        settings.lmstudio_base_url = lmstudio_base_url;
        settings.lmstudio_model = lmstudio_model;
        settings.lmstudio_api_key = lmstudio_api_key;
        settings.save();

        win.hide().unwrap();
    });

    let tts_window_for_cancel = tts_settings_window.as_weak();
    tts_settings_window.on_cancel_clicked(move || {
        if let Some(win) = tts_window_for_cancel.upgrade() {
            win.hide().unwrap();
        }
    });

    let tts_window_for_test = tts_settings_window.as_weak();
    tts_settings_window.on_test_tts_clicked(move || {
        let Some(win) = tts_window_for_test.upgrade() else {
            return;
        };
        let text = win.get_tts_test_text().to_string();
        if text.is_empty() {
            return;
        }
        let config = tts::TtsConfig {
            rate: win.get_tts_rate(),
            volume: win.get_tts_volume(),
            voice: {
                let voice = win.get_tts_voice().to_string();
                if voice.is_empty() {
                    None
                } else {
                    Some(voice)
                }
            },
        };

        let weak = tts_window_for_test.clone();
        std::thread::spawn(move || {
            let result = tts::speak_windows_tts_with_config(&text, &config);
            let msg = match result {
                Ok(()) => "TTS 测试完成".to_string(),
                Err(err) => format!("TTS 测试失败: {err}"),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_tts_test_status(SharedString::from(&msg));
                }
            });
        });
    });

    let speech_settings_weak = tts_settings_window.as_weak();
    tts_settings_window.on_open_speech_settings_clicked(move || {
        let _ = std::process::Command::new("cmd.exe")
            .args(["/C", "start", "ms-settings:speech"])
            .spawn();
        if let Some(win) = speech_settings_weak.upgrade() {
            win.set_tts_test_status(SharedString::from("已打开 Windows 讲述人语音设置"));
        }
    });

    // -- Runes window: close --
    let runes_weak = runes_window.as_weak();
    runes_window.on_close_requested(move || {
        if let Some(win) = runes_weak.upgrade() {
            win.hide().unwrap();
        }
    });

    // -- Runes window: apply rune --
    let runes_weak = runes_window.as_weak();
    let state_c = state.clone();
    let handle_c = rt_handle_ref.clone();
    runes_window.on_apply_rune_clicked({
        move |rune_idx| {
            let s = state_c.lock().unwrap();
            let auth = s.auth_url.clone();
            let rune = s.current_runes.get(rune_idx as usize).cloned();
            drop(s);

            if auth.is_empty() {
                return;
            }
            let Some(rune) = rune else { return };

            let weak = runes_weak.clone();
            let endpoint = format!("https://{auth}");
            handle_c.spawn(async move {
                let _ = slint::invoke_from_event_loop({
                    let weak = weak.clone();
                    move || {
                        if let Some(win) = weak.upgrade() {
                            win.set_apply_rune_status(SharedString::from("Applying rune…"));
                        }
                    }
                });

                let msg = match lcu_api::apply_rune(endpoint, rune).await {
                    Ok(()) => "Rune applied!".to_string(),
                    Err(e) => format!("Failed: {:?}", e),
                };

                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak.upgrade() {
                        win.set_apply_rune_status(SharedString::from(&msg));
                    }
                });
            });
        }
    });

    // -- Spawn background tasks --
    let sources_weak2 = sources_window.as_weak();
    let state_c2 = state.clone();
    rt_handle.spawn(fetch_sources_task(sources_weak2, state_c2));

    let runes_weak2 = runes_window.as_weak();
    let sources_weak3 = sources_window.as_weak();
    let state_c3 = state.clone();
    rt_handle.spawn(lcu_monitor_task(sources_weak3, runes_weak2, state_c3));

    let lmstudio_weak = sources_window.as_weak();
    let lmstudio_state = state.clone();
    rt_handle.spawn(lmstudio_health_loop(lmstudio_weak, lmstudio_state));

    let match_lifecycle_weak = sources_window.as_weak();
    let match_lifecycle_state = state.clone();
    rt_handle.spawn(match_lifecycle_task(
        match_lifecycle_weak,
        match_lifecycle_state,
    ));

    let item_state = state.clone();
    rt_handle.spawn(async move {
        if let Ok(names) = web::fetch_item_names().await {
            item_state.lock().unwrap().item_names = names;
        }
    });

    // DeepSeek-based lineup advice loop.
    let advice_weak = sources_window.as_weak();
    let advice_state = state.clone();
    rt_handle.spawn(advice_loop(advice_weak, advice_state));

    // Auto-launch LoL if it is not already running.
    let auto_launch_weak = sources_window.as_weak();
    let auto_launch_state = state.clone();
    rt_handle.spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut launch_ok = false;
        let msg = if lcu::cmd::check_if_lol_running() {
            launch_ok = true;
            "LoL client is already running".to_string()
        } else {
            let preferred_path = {
                let s = auto_launch_state.lock().unwrap();
                s.lol_launcher_path.clone()
            };
            let result = if preferred_path.trim().is_empty() {
                lcu::cmd::launch_lol_client()
            } else {
                lcu::cmd::launch_lol_client_with_path(Some(preferred_path.as_str()))
            };
            match result {
                Ok(path) => {
                    launch_ok = true;
                    format!("LoL client launched: {}", path.display())
                }
                Err(err) => format!("Unable to launch LoL client: {err}"),
            }
        };
        let launch_ok = launch_ok;
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = auto_launch_weak.upgrade() {
                win.set_launch_ok(launch_ok);
                win.set_launch_status(SharedString::from(&msg));
            }
        });
    });

    // -- Show sources window and run event loop --
    sources_window.show().unwrap();
    slint::run_event_loop().unwrap();
}

// ---------------------------------------------------------------------------
//  Task: fetch sources + champions + runes metadata at startup
// ---------------------------------------------------------------------------

async fn fetch_sources_task(sources_weak: Weak<SourcesWindow>, state: SharedState) {
    match web::init_for_ui().await {
        Ok((champions_map, _runes_meta)) => {
            // Store champions map in shared state
            {
                let mut s = state.lock().unwrap();
                s.champions_map = champions_map;
            }

            slint::invoke_from_event_loop(move || {
                if let Some(win) = sources_weak.upgrade() {
                    win.set_status(SharedString::from("success"));
                }
            })
            .unwrap();
        }
        Err(_) => {
            slint::invoke_from_event_loop(move || {
                if let Some(win) = sources_weak.upgrade() {
                    win.set_status(SharedString::from("error"));
                }
            })
            .unwrap();
        }
    }
}

// ---------------------------------------------------------------------------
//  Task: LCU process polling + WebSocket champion-select monitoring
// ---------------------------------------------------------------------------

async fn lcu_monitor_task(
    sources_weak: Weak<SourcesWindow>,
    runes_weak: Weak<RunesWindow>,
    state: SharedState,
) {
    let mut current_auth_url = String::new();
    let mut current_champion_id: i64 = 0;
    let mut current_lcu_pid: Option<u32> = None;
    let mut auth_prompted_for_pid: Option<u32> = None;

    loop {
        let Some(lcu_pid) = get_lcu_process_id() else {
            if current_lcu_pid.is_some() || !current_auth_url.is_empty() {
                current_auth_url.clear();
                current_champion_id = 0;
                current_lcu_pid = None;
                auth_prompted_for_pid = None;

                {
                    let mut s = state.lock().unwrap();
                    s.auth_url.clear();
                    s.lol_dir.clear();
                    s.is_tencent = false;
                    s.current_champion_id = 0;
                    s.current_champion_alias.clear();
                }

                let sw = sources_weak.clone();
                let rw = runes_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = sw.upgrade() {
                        win.set_lcu_status(SharedString::from("disconnected"));
                        win.set_lcu_summoner(SharedString::from(""));
                        win.set_lol_running(false);
                    }
                    if let Some(win) = rw.upgrade() {
                        win.set_has_champion(false);
                        win.set_champion_id(0);
                        win.hide().unwrap();
                    }
                });
            }
            tokio::time::sleep(Duration::from_millis(2500)).await;
            continue;
        };

        if current_lcu_pid != Some(lcu_pid) {
            current_lcu_pid = Some(lcu_pid);
            auth_prompted_for_pid = None;
            current_auth_url.clear();
            current_champion_id = 0;

            {
                let mut s = state.lock().unwrap();
                s.auth_url.clear();
                s.lol_dir.clear();
                s.is_tencent = false;
                s.current_champion_id = 0;
                s.current_champion_alias.clear();
            }

            let sw = sources_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = sw.upgrade() {
                    win.set_lol_running(true);
                }
            });
        }

        if current_auth_url.is_empty() {
            if auth_prompted_for_pid != Some(lcu_pid) {
                auth_prompted_for_pid = Some(lcu_pid);

                let sw = sources_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = sw.upgrade() {
                        win.set_lcu_status(SharedString::from("authorizing"));
                        win.set_lcu_summoner(SharedString::from(""));
                    }
                });

                let cmd_output = match tokio::task::spawn_blocking(get_cmd_output).await {
                    Ok(Ok(ret)) if !ret.auth_url.is_empty() => ret,
                    _ => {
                        let sw = sources_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(win) = sw.upgrade() {
                                win.set_lcu_status(SharedString::from("needs-admin"));
                                win.set_lcu_summoner(SharedString::from(""));
                            }
                        });
                        tokio::time::sleep(Duration::from_millis(2500)).await;
                        continue;
                    }
                };

                let auth_url = cmd_output.auth_url.clone();
                current_auth_url = auth_url.clone();
                current_champion_id = 0;
                info!("LCU auth URL changed: {}", &current_auth_url);

                {
                    let mut s = state.lock().unwrap();
                    s.auth_url = auth_url.clone();
                    s.lol_dir = cmd_output.dir.clone();
                    s.is_tencent = cmd_output.is_tencent;
                }

                let endpoint = format!("https://{auth_url}");
                let summoner_name = match lcu_api::get_current_summoner(&endpoint).await {
                    Ok(summoner) => {
                        if !summoner.game_name.is_empty() {
                            format!("{}#{}", summoner.game_name, summoner.tag_line)
                        } else {
                            summoner.display_name
                        }
                    }
                    Err(_) => "Connected".to_string(),
                };

                let sw = sources_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = sw.upgrade() {
                        win.set_lcu_status(SharedString::from("connected"));
                        win.set_lcu_summoner(SharedString::from(&summoner_name));
                    }
                });
            } else {
                tokio::time::sleep(Duration::from_millis(2500)).await;
                continue;
            }
        }

        // Connect via WebSocket and listen for champion select events
        match make_ws_client_tls(&current_auth_url).await {
            Ok(ws) => {
                let (mut tx, mut rx) = ws.split();

                if let Err(e) = tx.send(make_sub_msg()).await {
                    warn!("error sending WS subscribe message: {}", e);
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                    continue;
                }

                while let Some(msg) = rx.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if text.is_empty() {
                                continue;
                            }
                            let parsed: Value = match from_str(&text) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };

                            let data = parsed.get(2).and_then(|v| v.as_object());
                            let uri = data.and_then(|v| v.get("uri")).and_then(|v| v.as_str());

                            // Champion select session changes
                            if uri == Some("/lol-champ-select/v1/session") {
                                let event_type = data
                                    .and_then(|v| v.get("eventType"))
                                    .and_then(|v| v.as_str());

                                if event_type == Some("Delete") {
                                    // Session ended
                                    if current_champion_id != 0 {
                                        current_champion_id = 0;
                                        {
                                            let mut s = state.lock().unwrap();
                                            s.current_champion_id = 0;
                                            s.current_champion_alias.clear();
                                        }
                                        let rw = runes_weak.clone();
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(win) = rw.upgrade() {
                                                win.set_has_champion(false);
                                                win.set_champion_id(0);
                                                win.hide().unwrap();
                                            }
                                        });
                                    }
                                    continue;
                                }

                                // Extract champion ID from session data
                                let session_data = data.and_then(|v| v.get("data"));
                                let cid = extract_champion_id_from_session(session_data);

                                if cid != current_champion_id && cid > 0 {
                                    current_champion_id = cid;
                                    info!("champion id changed: {}", cid);

                                    {
                                        let mut s = state.lock().unwrap();
                                        s.current_champion_id = cid;
                                        s.current_champion_alias = s
                                            .champions_map
                                            .values()
                                            .find(|c| c.key == cid.to_string())
                                            .map(|c| c.id.clone())
                                            .unwrap_or_default();
                                    }

                                    // Update runes window
                                    let rw = runes_weak.clone();
                                    let auth = current_auth_url.clone();
                                    let st = state.clone();

                                    show_champion_runes(rw, st, auth, cid).await;
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("WS receive error: {}", e);
                            break;
                        }
                    }
                }

                info!("WebSocket disconnected, will retry");
            }
            Err(e) => {
                warn!("error creating WebSocket client: {:?}", e);
            }
        }

        tokio::time::sleep(Duration::from_millis(2500)).await;
    }
}

fn trim_coach_messages(messages: &mut Vec<ChatMessage>, max_len: usize) {
    if messages.len() > max_len {
        let split_at = messages.len() - max_len;
        *messages = messages.split_off(split_at);
    }
}

fn coach_message_display(role: &str, content: &str) -> String {
    if role == "assistant" {
        format!("Coach: {content}\n\n")
    } else if content.starts_with("LIVE MATCH STATE UPDATE\n") {
        let content = content
            .strip_prefix("LIVE MATCH STATE UPDATE\n")
            .unwrap_or(content);
        format!("Match state: {content}\n\n")
    } else {
        format!("You: {content}\n\n")
    }
}

fn refresh_coach_chat_log(weak: &Weak<SourcesWindow>, state: &SharedState) {
    let output_log = {
        let s = state.lock().unwrap();
        s.ui_log.clone()
    };
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(win) = weak.upgrade() {
            win.set_coach_chat_log(SharedString::from(&output_log));
        }
    });
}

fn append_coach_message(
    weak: &Weak<SourcesWindow>,
    state: &SharedState,
    role: &str,
    content: &str,
) {
    {
        let mut s = state.lock().unwrap();
        s.coach_messages.push(ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        });
        trim_coach_messages(&mut s.coach_messages, 24);
        let display = coach_message_display(role, content);
        if role == "assistant" {
            s.ui_log.push_str(&display);
        }
    }
    refresh_coach_chat_log(weak, state);
}

fn append_system_log(weak: &Weak<SourcesWindow>, state: &SharedState, text: &str) {
    {
        let mut s = state.lock().unwrap();
        s.ui_log.push_str(text);
        s.ui_log.push_str("\n\n");
    }
    refresh_coach_chat_log(weak, state);
}

fn append_info_log(weak: &Weak<SourcesWindow>, state: &SharedState, text: &str) {
    let output_log = {
        let mut s = state.lock().unwrap();
        s.ui_log.push_str(text);
        s.ui_log.push_str("\n\n");
        s.ui_log.clone()
    };
    if let Some(win) = weak.upgrade() {
        win.set_coach_chat_log(SharedString::from(&output_log));
    }
}

async fn build_current_coach_prompt(state: &SharedState) -> Option<String> {
    let (auth_url, champions_map, item_names, local_champion_id, last_prompt) = {
        let s = state.lock().unwrap();
        (
            s.auth_url.clone(),
            s.champions_map.clone(),
            s.item_names.clone(),
            s.current_champion_id,
            s.coach_last_prompt.clone(),
        )
    };

    if auth_url.is_empty() {
        return None;
    }

    let endpoint = format!("https://{auth_url}");
    let mut live_data_available = false;
    let live_prompt = match live_client::fetch_all_game_data().await {
        Ok(game_data) => {
            live_data_available = true;
            match advisor::build_live_game_prompt(&game_data, &item_names) {
                Ok(prompt) => Some(prompt),
                Err(_) => None,
            }
        }
        Err(_) => None,
    };

    let mut prompt = if let Some(prompt) = live_prompt {
        prompt
    } else {
        match lcu_api::get_champ_select_session(&endpoint).await {
            Ok(session) => match advisor::build_lineup_prompt(&session, &champions_map) {
                Ok(prompt) => prompt,
                Err(_) => last_prompt.clone(),
            },
            Err(_) => last_prompt.clone(),
        }
    };

    if prompt.is_empty() {
        return None;
    }

    if !live_data_available {
        prompt = format!("当前无 Live Client Data，可能对局已结束或不在对局中\n\n{prompt}");
    }

    if local_champion_id > 0 {
        if let Ok(sections) =
            web::list_builds_by_id(&DEFAULT_SOURCE_VALUE.to_string(), local_champion_id).await
        {
            let build_titles = sections
                .iter()
                .flat_map(|section| section.item_builds.iter().map(|build| build.title.clone()))
                .collect::<Vec<_>>()
                .join("; ");
            if !build_titles.is_empty() {
                prompt = format!("{prompt}\nCurrent champion recommended builds: {build_titles}");
            }
        }
    }

    {
        let mut s = state.lock().unwrap();
        s.coach_last_prompt = prompt.clone();
        s.last_progress_key = prompt.clone();
    }

    Some(prompt)
}

async fn send_coach_message(
    weak: &Weak<SourcesWindow>,
    state: &SharedState,
    input: String,
) -> anyhow::Result<String> {
    {
        let input_for_ui = input.clone();
        let weak_for_ui = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak_for_ui.upgrade() {
                win.set_coach_input(SharedString::from(&input_for_ui));
            }
        });
    }

    let (auth_url, deepseek_config, lmstudio_config, ai_provider, history) = {
        let s = state.lock().unwrap();
        (
            s.auth_url.clone(),
            s.deepseek_config.clone(),
            s.lmstudio_config.clone(),
            s.ai_provider.clone(),
            s.coach_messages.clone(),
        )
    };

    if auth_url.is_empty() {
        anyhow::bail!("League Client is not connected");
    }

    let llm_config = if ai_provider == "lmstudio" {
        lmstudio_config
    } else {
        deepseek_config
    };
    if ai_provider != "lmstudio" && llm_config.api_key.is_empty() {
        anyhow::bail!("DeepSeek API Key is not configured");
    }

    let client = DeepSeekClient::new(llm_config);
    let context = build_current_coach_prompt(state).await;

    let mut messages = vec![ChatMessage::system(advisor::DEFAULT_SYSTEM_PROMPT)];
    messages.extend(history);
    if let Some(context) = &context {
        messages.push(ChatMessage::user(context.clone()));
    }
    messages.push(ChatMessage::user(input.clone()));

    if let Some(context) = &context {
        append_coach_message(
            weak,
            state,
            "user",
            &format!("LIVE MATCH STATE UPDATE\n{context}"),
        );
    }
    append_coach_message(weak, state, "user", &input);

    let advice = client.chat_messages(messages).await?;

    append_coach_message(weak, state, "assistant", &advice);

    Ok(advice)
}

async fn greet_coach(weak: Weak<SourcesWindow>, state: SharedState) {
    match send_coach_message(&weak, &state, "hi".to_string()).await {
        Ok(reply) => {
            let tts_config = {
                let s = state.lock().unwrap();
                s.tts_config.clone()
            };
            tokio::task::spawn_blocking(move || {
                let _ = tts::speak_windows_tts_with_config(&reply, &tts_config);
            });
        }
        Err(err) => {
            let message = format!("System: Coach error: {err}");
            append_system_log(&weak, &state, &message);
        }
    }
}

fn lmstudio_models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/models")
    } else {
        format!("{trimmed}/v1/models")
    }
}

async fn lmstudio_health_status(base_url: &str) -> &'static str {
    if base_url.trim().is_empty() {
        return "unknown";
    }

    let url = lmstudio_models_url(base_url);
    let client = match lcu::reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => return "unreachable",
    };

    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => "connected",
        _ => "unreachable",
    }
}

async fn lmstudio_health_loop(weak: Weak<SourcesWindow>, state: SharedState) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let base_url = {
            let s = state.lock().unwrap();
            s.lmstudio_config.base_url.clone()
        };
        let status = lmstudio_health_status(&base_url).await;

        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak.upgrade() {
                win.set_lmstudio_status(SharedString::from(status));
            }
        });
    }
}

fn parse_match_phase(raw: &str) -> MatchPhase {
    match raw {
        "ChampSelect" => MatchPhase::ChampSelect,
        "GameStart" => MatchPhase::GameStart,
        "InProgress" => MatchPhase::InProgress,
        "WaitingForStats" | "PreEndOfGame" | "EndOfGame" => MatchPhase::Ended,
        _ => MatchPhase::Idle,
    }
}

fn match_phase_label(phase: &MatchPhase) -> &'static str {
    match phase {
        MatchPhase::Idle => "等待对局",
        MatchPhase::ChampSelect => "对局创建",
        MatchPhase::GameStart => "对局创建",
        MatchPhase::InProgress => "对局中",
        MatchPhase::Ended => "对局结束",
    }
}

async fn match_lifecycle_task(weak: Weak<SourcesWindow>, state: SharedState) {
    let mut last_session_label = String::new();

    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;

        let auth_url = {
            let s = state.lock().unwrap();
            s.auth_url.clone()
        };
        if auth_url.is_empty() {
            continue;
        }

        let raw_phase = match lcu_api::get_gameflow_phase(&auth_url).await {
            Ok(phase) => phase,
            Err(_) => continue,
        };
        let phase = parse_match_phase(&raw_phase);

        let old_phase = {
            let s = state.lock().unwrap();
            s.match_phase.clone()
        };

        if phase == MatchPhase::ChampSelect && old_phase != MatchPhase::ChampSelect {
            state.lock().unwrap().start_match_session(MatchPhase::ChampSelect);
        } else if matches!(phase, MatchPhase::GameStart | MatchPhase::InProgress)
            && matches!(old_phase, MatchPhase::Idle | MatchPhase::Ended)
        {
            state.lock().unwrap().start_match_session(phase.clone());
        } else if phase == MatchPhase::Ended && old_phase != MatchPhase::Ended {
            state.lock().unwrap().reset_match_session();
            let mut s = state.lock().unwrap();
            s.match_phase = MatchPhase::Ended;
        } else if phase == MatchPhase::Idle
            && matches!(
                old_phase,
                MatchPhase::ChampSelect | MatchPhase::GameStart | MatchPhase::InProgress
            )
        {
            // A transient "None" from gameflow should not tear down an active session.
        } else {
            let mut s = state.lock().unwrap();
            s.match_phase = phase.clone();
        }

        let label = {
            let s = state.lock().unwrap();
            match_phase_label(&s.match_phase)
        };
        if label != last_session_label {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_match_session_status(SharedString::from(label));
                }
            });
            last_session_label = label.to_string();
        }
    }
}

async fn advice_loop(sources_weak: Weak<SourcesWindow>, state: SharedState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;

        let (auth_url, deepseek_config, lmstudio_config, ai_provider) = {
            let s = state.lock().unwrap();
            (
                s.auth_url.clone(),
                s.deepseek_config.clone(),
                s.lmstudio_config.clone(),
                s.ai_provider.clone(),
            )
        };

        if auth_url.is_empty() {
            continue;
        }

        let llm_enabled = {
            let s = state.lock().unwrap();
            s.llm_assistance_enabled
        };
        if !llm_enabled {
            continue;
        }

        if ai_provider != "lmstudio" && deepseek_config.api_key.is_empty() {
            let weak = sources_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_advice_text(SharedString::from("DeepSeek API Key 未配置"));
                }
            });
            continue;
        }

        let llm_config = if ai_provider == "lmstudio" {
            lmstudio_config
        } else {
            deepseek_config
        };
        let client = DeepSeekClient::new(llm_config);

        let prompt = match build_current_coach_prompt(&state).await {
            Some(prompt) => prompt,
            None => "当前无法读取实时对局数据，请给出通用对局建议，并提醒玩家等待数据恢复。".to_string(),
        };

        let history = {
            let s = state.lock().unwrap();
            s.coach_messages.clone()
        };
        let context_message = format!("LIVE MATCH STATE UPDATE\n{prompt}");
        let mut messages = vec![ChatMessage::system(advisor::DEFAULT_SYSTEM_PROMPT)];
        messages.extend(history);
        messages.push(ChatMessage::user(context_message.clone()));

        append_coach_message(&sources_weak, &state, "user", &context_message);

        match client.chat_messages(messages).await {
            Ok(advice) => {
                append_coach_message(&sources_weak, &state, "assistant", &advice);
                let tts_text = advice.clone();
                let weak = sources_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak.upgrade() {
                        win.set_advice_text(SharedString::from(&advice));
                    }
                });
                let tts_config_for_speech = {
                    let s = state.lock().unwrap();
                    s.tts_config.clone()
                };
                tokio::task::spawn_blocking(move || {
                    let _ = tts::speak_windows_tts_with_config(&tts_text, &tts_config_for_speech);
                });
            }
            Err(err) => {
                let msg = format!("DeepSeek 请求失败: {err}");
                let weak = sources_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak.upgrade() {
                        win.set_advice_text(SharedString::from(&msg));
                    }
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  Extract champion ID from a champ-select session JSON
// ---------------------------------------------------------------------------

fn extract_champion_id_from_session(session: Option<&Value>) -> i64 {
    let session = match session {
        Some(v) => v,
        None => return 0,
    };

    let cell_id = match session.get("localPlayerCellId").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return 0,
    };

    // Check myTeam first
    if let Some(team) = session.get("myTeam").and_then(|v| v.as_array()) {
        for member in team {
            if member.get("cellId").and_then(|v| v.as_i64()) == Some(cell_id) {
                if let Some(cid) = member.get("championId").and_then(|v| v.as_i64()) {
                    if cid > 0 {
                        return cid;
                    }
                }
            }
        }
    }

    // Check actions
    if let Some(actions) = session.get("actions").and_then(|v| v.as_array()) {
        for row in actions {
            if let Some(arr) = row.as_array() {
                for action in arr {
                    let actor = action.get("actorCellId").and_then(|v| v.as_i64());
                    let action_type = action.get("type").and_then(|v| v.as_str());
                    if actor == Some(cell_id) && action_type != Some("ban") {
                        if let Some(cid) = action.get("championId").and_then(|v| v.as_i64()) {
                            if cid > 0 {
                                return cid;
                            }
                        }
                    }
                }
            }
        }
    }

    0
}

// ---------------------------------------------------------------------------
//  Show champion runes: fetch avatar, populate source list, fetch runes
// ---------------------------------------------------------------------------

async fn show_champion_runes(
    runes_weak: Weak<RunesWindow>,
    state: SharedState,
    auth_url: String,
    champion_id: i64,
) {
    // Fetch champion avatar pixels (off UI thread)
    let avatar_pixels = fetch_champion_avatar_pixels(&auth_url, champion_id as u64).await;

    // Determine champion name from champions_map
    let champion_name = {
        let s = state.lock().unwrap();
        s.champions_map
            .values()
            .find(|c| c.key == champion_id.to_string())
            .map(|c| c.name.clone())
            .unwrap_or_default()
    };

    // Update the runes window with champion info
    let weak = runes_weak.clone();
    let champ_name = SharedString::from(&champion_name);

    let _ = slint::invoke_from_event_loop(move || {
        if let Some(win) = weak.upgrade() {
            win.set_champion_id(champion_id as i32);
            win.set_champion_name(champ_name);
            win.set_has_champion(true);

            if let Some(px) = avatar_pixels {
                let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &px.rgba_data,
                    px.width,
                    px.height,
                );
                win.set_champion_avatar(Image::from_rgba8(buffer));
            }

            win.show().unwrap();
        }
    });

    fetch_and_show_runes(
        runes_weak,
        state,
        DEFAULT_SOURCE_VALUE.to_string(),
        champion_id,
    )
    .await;
}

// ---------------------------------------------------------------------------
//  Fetch runes for a champion from a source and display them
// ---------------------------------------------------------------------------

async fn fetch_and_show_runes(
    runes_weak: Weak<RunesWindow>,
    state: SharedState,
    source: String,
    champion_id: i64,
) {
    // Set loading state
    let weak = runes_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(win) = weak.upgrade() {
            win.set_rune_status(SharedString::from("loading"));
            win.set_apply_rune_status(SharedString::from(""));
        }
    });

    match web::list_builds_by_id(&source, champion_id).await {
        Ok(sections) => {
            let runes: Vec<Rune> = sections.iter().flat_map(|s| s.runes.clone()).collect();

            let rune_models: Vec<RuneModel> = runes
                .iter()
                .enumerate()
                .map(|(i, r)| RuneModel {
                    index: i as i32,
                    name: SharedString::from(&r.name),
                    position: SharedString::from(&r.position),
                    pick_count: r.pick_count as i32,
                    win_rate: SharedString::from(&r.win_rate),
                    primary_style_id: r.primary_style_id as i32,
                    sub_style_id: r.sub_style_id as i32,
                })
                .collect();

            // Store runes in shared state so we can apply them
            {
                let mut s = state.lock().unwrap();
                s.current_runes = runes;
            }

            let weak = runes_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    let model = ModelRc::new(VecModel::from(rune_models));
                    win.set_runes(model);
                    win.set_rune_status(SharedString::from("success"));
                }
            });
        }
        Err(_) => {
            let weak = runes_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_rune_status(SharedString::from("error"));
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
//  WebSocket client that accepts the LCU's self-signed certificate
// ---------------------------------------------------------------------------

async fn make_ws_client_tls(
    endpoint: &str,
) -> Result<lcu::reqwest_websocket::WebSocket, lcu::reqwest_websocket::Error> {
    use lcu::reqwest_websocket::RequestBuilderExt;

    let url = format!("wss://{endpoint}/");
    let client = lcu::reqwest::Client::builder()
        .http1_only()
        .use_rustls_tls()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .build()
        .unwrap();
    let response = client
        .get(url)
        .version(lcu::reqwest::Version::HTTP_11)
        .upgrade()
        .send()
        .await?;
    let ws = response.into_websocket().await?;
    Ok(ws)
}

// ---------------------------------------------------------------------------
//  Champion avatar pixel fetching (decode PNG → RGBA on tokio thread)
// ---------------------------------------------------------------------------

struct AvatarPixels {
    width: u32,
    height: u32,
    rgba_data: Vec<u8>,
}

async fn fetch_champion_avatar_pixels(auth_url: &str, champion_id: u64) -> Option<AvatarPixels> {
    let url = format!(
        "https://{}/lol-game-data/assets/v1/champion-icons/{}.png",
        auth_url, champion_id
    );

    let client = lcu_api::make_client();
    let resp = client.get(&url).send().await.ok()?;
    let bytes = resp.bytes().await.ok()?;

    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    Some(AvatarPixels {
        width,
        height,
        rgba_data: rgba.into_raw(),
    })
}
