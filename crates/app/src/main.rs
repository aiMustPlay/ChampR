#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kv_log_macro::{info, warn};
use slint::{ComponentHandle, Image, ModelRc, SharedPixelBuffer, SharedString, VecModel, Weak};

use lcu::{
    advisor,
    builds::Rune,
    cmd::{get_cmd_output, get_lcu_process_id},
    deepseek::{DeepSeekClient, DeepSeekConfig},
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
        }
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
    sources_window.on_llm_assistance_changed(move |enabled| {
        llm_assistance_state.lock().unwrap().llm_assistance_enabled = enabled;
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

async fn advice_loop(sources_weak: Weak<SourcesWindow>, state: SharedState) {
    let item_names = web::fetch_item_names().await.unwrap_or_default();

    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_prompt: Option<String> = None;

    loop {
        interval.tick().await;

        let (auth_url, champions_map, deepseek_config) = {
            let s = state.lock().unwrap();
            (
                s.auth_url.clone(),
                s.champions_map.clone(),
                s.deepseek_config.clone(),
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

        if deepseek_config.api_key.is_empty() {
            let weak = sources_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_advice_text(SharedString::from("DeepSeek API Key 未配置"));
                }
            });
            continue;
        }

        let client = DeepSeekClient::new(deepseek_config);

        let endpoint = format!("https://{auth_url}");
        let live_prompt = match live_client::fetch_all_game_data().await {
            Ok(game_data) => match advisor::build_live_game_prompt(&game_data, &item_names) {
                Ok(prompt) => {
                    last_prompt = Some(prompt.clone());
                    Some(prompt)
                }
                Err(_) => None,
            },
            Err(_) => None,
        };

        let mut prompt = if let Some(prompt) = live_prompt {
            prompt
        } else {
            let session = lcu_api::get_champ_select_session(&endpoint).await;

            match session {
                Ok(session) => match advisor::build_lineup_prompt(&session, &champions_map) {
                    Ok(prompt) => {
                        last_prompt = Some(prompt.clone());
                        prompt
                    }
                    Err(_) => match last_prompt.clone() {
                        Some(prompt) => prompt,
                        None => continue,
                    },
                },
                Err(_) => match last_prompt.clone() {
                    Some(prompt) => prompt,
                    None => continue,
                },
            }
        };

        let local_champion_id = {
            let s = state.lock().unwrap();
            s.current_champion_id
        };
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
                    prompt = format!("{prompt}\n当前英雄推荐装备方案: {build_titles}");
                }
            }
        }

        match client
            .chat(advisor::DEFAULT_SYSTEM_PROMPT, &prompt)
            .await
        {
            Ok(advice) => {
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
