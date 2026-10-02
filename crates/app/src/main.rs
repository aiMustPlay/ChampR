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
    browser_sidecar::BrowserSidecar,
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
mod game_screen;
mod monitors;
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
    /// Local player's assigned lane in the current champ select (e.g. "middle").
    current_assigned_position: String,
    /// Auto-apply the position's best OP.GG rune page when the pick locks in.
    auto_apply_rune: bool,
    /// Auto-write recommended item builds when the pick locks in.
    auto_apply_builds: bool,
    /// 排队就绪自动接受对局(设置可开关, 默认关)。
    auto_accept_match: bool,
    /// Objective reminder tier: 0 = all, 1 = key events only, 2 = quiet (log only).
    reminder_tier: i32,
    /// Show the always-on-top mini match window while the game is in progress.
    mini_live_enabled: bool,
    /// 固定窗口出现的显示器索引(-1 = 不固定)。
    pinned_monitor: i32,
    /// 启动时枚举到的显示器列表(供固定与设置页展示)。
    monitors: Vec<monitors::Monitor>,
    /// 全英雄心理图谱(DDragon allytips/enemytips), 启动失败则为空图。
    playbook: lcu::tips::PlaystyleAtlas,
    /// Champion id that was already auto-applied this champ select.
    last_auto_applied_champion: i64,
    /// 本局选人已应用的 counter 方案签名(样式id+perk id 排列)。
    /// 选人窗口内对手换锁/换英雄 → 方案变化 → 重新应用; 签名相同不重复写。
    last_applied_plan_sig: String,
    /// Latest counter-rule rune plan for the current matchup (None when not actionable).
    counter_plan: Option<lcu::counter::RunePlan>,
    /// User closed the runes window during this champ-select; do not re-open
    /// it automatically until a new session starts.
    runes_window_dismissed: bool,
    /// TTS voice configuration used by the advice loop.
    tts_config: tts::TtsConfig,
    /// User-configurable LoL launcher path.
    lol_launcher_path: String,
    /// DeepSeek configuration used by the advice loop.
    deepseek_config: DeepSeekConfig,
    /// Whether LLM-based match assistance is enabled.
    llm_assistance_enabled: bool,
    /// AI provider: deepseek / deepseek_web / lmstudio / openai(任意兼容端点)。
    ai_provider: String,
    /// LLM 通道: "maohou" 经 houmao 引擎子进程(默认) / "direct" 直连 reqwest。
    ai_backend: String,
    /// 显式 maohou 路径(空=自动定位: MAOHOU_BIN → 兄弟仓 → PATH)。
    maohou_bin: String,
    /// 命令行/CMDline bin 打开一次性的 fallback 提醒标记(避免每次调用都 warn)。
    engine_fallback_warned: bool,
    /// User acknowledgment required before browser automation starts.
    deepseek_web_risk_accepted: bool,
    /// Lazily started persistent browser sidecar.
    deepseek_web: Option<Arc<BrowserSidecar>>,
    /// LM Studio local OpenAI-compatible config.
    lmstudio_config: DeepSeekConfig,
    /// 任意 OpenAI 兼容端点(自定义网关/推理站; Claude 不走这里)。
    openai_config: DeepSeekConfig,
    /// Conversation history shared by automatic advice and the coach chat panel.
    coach_messages: Vec<ChatMessage>,
    /// Last successfully built match context, used as a fallback for follow-up questions.
    coach_last_prompt: String,
    /// Prevents overlapping coach chat requests.
    coach_busy: bool,
    /// Serializes all AI calls, including automatic and manual requests.
    coach_request_lock: Arc<tokio::sync::Mutex<()>>,
    /// Prevents overlapping speech playback.
    speech_lock: Arc<Mutex<()>>,
    /// Cached Data Dragon zh_CN static names (champions/runes/items) used to enrich prompts.
    static_names: web::StaticNames,
    /// Per-champion OP.GG sections cache (keyed by champion id) reused across prompts.
    opgg_sections_cache: HashMap<i64, Vec<lcu::builds::BuildSection>>,
    /// Ranked summary cache keyed by summoner id, fetched at most once per player.
    ranked_stats_cache: HashMap<i64, String>,
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
            current_assigned_position: String::new(),
            auto_apply_rune: false,
            auto_apply_builds: false,
            auto_accept_match: true,
            reminder_tier: 0,
            mini_live_enabled: true,
            pinned_monitor: -1,
            monitors: Vec::new(),
            playbook: lcu::tips::PlaystyleAtlas::default(),
            last_auto_applied_champion: 0,
            last_applied_plan_sig: String::new(),
            counter_plan: None,
            runes_window_dismissed: false,
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
            ai_backend: "maohou".to_string(),
            maohou_bin: String::new(),
            engine_fallback_warned: false,
            deepseek_web_risk_accepted: false,
            deepseek_web: None,
            lmstudio_config: DeepSeekConfig {
                api_key: String::new(),
                base_url: "http://localhost:1234/v1".to_string(),
                model: "local-model".to_string(),
                thinking_enabled: false,
                reasoning_effort: String::new(),
                stream_enabled: false,
            },
            openai_config: DeepSeekConfig {
                api_key: String::new(),
                base_url: String::new(),
                model: String::new(),
                thinking_enabled: false,
                reasoning_effort: String::new(),
                stream_enabled: false,
            },
            coach_messages: Vec::new(),
            coach_last_prompt: String::new(),
            coach_busy: false,
            coach_request_lock: Arc::new(tokio::sync::Mutex::new(())),
            speech_lock: Arc::new(Mutex::new(())),
            static_names: web::StaticNames::default(),
            opgg_sections_cache: HashMap::new(),
            ranked_stats_cache: HashMap::new(),
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
    let mini_window = MiniMatchWindow::new().unwrap();

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
    initial_state.ai_backend = saved_settings.ai_backend.clone();
    initial_state.maohou_bin = saved_settings.maohou_bin.clone();
    initial_state.deepseek_web_risk_accepted = saved_settings.deepseek_web_risk_accepted;
    initial_state.auto_apply_rune = saved_settings.auto_apply_rune;
    initial_state.auto_apply_builds = saved_settings.auto_apply_builds;
    initial_state.auto_accept_match = saved_settings.auto_accept_match;
    initial_state.reminder_tier = saved_settings.reminder_tier;
    initial_state.mini_live_enabled = saved_settings.mini_live_window;
    // 显示器固定: 手动配置, 启动时枚举一次; 插拔显示器后重启 app 生效。
    initial_state.pinned_monitor = saved_settings.pinned_monitor;
    initial_state.monitors = monitors::list_monitors();
    initial_state.lmstudio_config = DeepSeekConfig {
        api_key: saved_settings.lmstudio_api_key.clone(),
        base_url: saved_settings.lmstudio_base_url.clone(),
        model: saved_settings.lmstudio_model.clone(),
        thinking_enabled: false,
        reasoning_effort: String::new(),
        stream_enabled: false,
    };
    initial_state.openai_config = DeepSeekConfig {
        api_key: saved_settings.openai_api_key.clone(),
        base_url: saved_settings.openai_base_url.clone(),
        model: saved_settings.openai_model.clone(),
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
    tts_settings_window.set_deepseek_base_url(SharedString::from(&saved_settings.deepseek_base_url));
    tts_settings_window.set_deepseek_model(SharedString::from(&saved_settings.deepseek_model));
    tts_settings_window.set_deepseek_thinking(saved_settings.deepseek_thinking);
    tts_settings_window.set_deepseek_stream(saved_settings.deepseek_stream);
    tts_settings_window.set_deepseek_reasoning_effort(SharedString::from(
        &saved_settings.deepseek_reasoning_effort,
    ));
    tts_settings_window.set_ai_provider(SharedString::from(&saved_settings.ai_provider));
    tts_settings_window.set_ai_backend(SharedString::from(&saved_settings.ai_backend));
    tts_settings_window.set_maohou_bin(SharedString::from(&saved_settings.maohou_bin));
    // 引擎探测报告: 显式路径 > 自动定位 > "未找到将走直连"
    {
        let detected = if !saved_settings.maohou_bin.is_empty() {
            let p = std::path::PathBuf::from(&saved_settings.maohou_bin);
            if p.is_file() {
                format!("引擎: {}(手动指定)", p.display())
            } else {
                format!("⚠ 指定的引擎路径不存在: {}", p.display())
            }
        } else if let Some(bin) = lcu::maohou::locate_binary() {
            format!("引擎: {}(自动发现)", bin.display())
        } else {
            "未找到 maohou 引擎 —— 走直连备用通道; 安装 houmao-mac\\engine 后移除此行".to_string()
        };
        tts_settings_window.set_maohou_status(SharedString::from(detected));
    }
    tts_settings_window.set_deepseek_web_risk_accepted(saved_settings.deepseek_web_risk_accepted);
    tts_settings_window.set_lmstudio_base_url(SharedString::from(&saved_settings.lmstudio_base_url));
    tts_settings_window.set_lmstudio_model(SharedString::from(&saved_settings.lmstudio_model));
    tts_settings_window.set_lmstudio_api_key(SharedString::from(&saved_settings.lmstudio_api_key));
    tts_settings_window.set_openai_base_url(SharedString::from(&saved_settings.openai_base_url));
    tts_settings_window.set_openai_model(SharedString::from(&saved_settings.openai_model));
    tts_settings_window.set_openai_api_key(SharedString::from(&saved_settings.openai_api_key));
    runes_window.set_auto_apply_enabled(saved_settings.auto_apply_rune);
    runes_window.set_auto_builds_enabled(saved_settings.auto_apply_builds);
    tts_settings_window.set_auto_accept_match(saved_settings.auto_accept_match);
    sources_window.set_reminder_tier(saved_settings.reminder_tier);
    sources_window.set_mini_live_enabled(saved_settings.mini_live_window);

    // 显示器下拉: "不固定" + 各显示器(枚举顺序即索引)
    {
        let s = state.lock().unwrap();
        let mut items = vec![SharedString::from("不固定(跟随系统当前屏)")];
        for m in &s.monitors {
            items.push(SharedString::from(monitors::label(m)));
        }
        tts_settings_window.set_monitor_options(ModelRc::new(VecModel::from(items)));
        let current = if saved_settings.pinned_monitor >= 0
            && (saved_settings.pinned_monitor as usize) < s.monitors.len()
        {
            saved_settings.pinned_monitor + 1
        } else {
            0
        };
        tts_settings_window.set_pinned_monitor(current);
    }

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
        info!("launch-lol clicked (lol_running check next)");
        if lcu::cmd::check_if_lol_running() {
            // 客户端还在(包括对局中后台的 LeagueClientUx): 这次点击的
            // 语义是把客户端窗口唤到前台, 不再是一句静态提示。
            let activated = game_screen::activate_lol_client_window();
            info!("LoL client already running; activate window -> {activated}");
            let w = launch_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    win.set_launch_ok(true);
                    win.set_launch_status(SharedString::from(if activated {
                        "已唤出 LoL 客户端窗口"
                    } else {
                        "LoL 在运行但没找到客户端窗口(可能在对局中)"
                    }));
                }
            });
            return;
        }

        // 启动路径要防丢日志 + 防堵 UI: 定位/拉起/UAC 都可能在
        // spawn 内部同步等 UAC 确认(用户看不到提示时表现就是"点了没反应"),
        // 整体挪到后台线程, UI 立即给出"正在启动"反馈。
        let preferred_path = {
            let s = launch_state.lock().unwrap();
            s.lol_launcher_path.clone()
        };
        {
            let w = launch_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = w.upgrade() {
                    win.set_launch_ok(true);
                    win.set_launch_status(SharedString::from("正在启动 LoL... (留意 UAC 确认框)"));
                }
            });
        }
        let lw_worker = launch_weak.clone();
        std::thread::Builder::new()
            .name("lol-launch".into())
            .spawn(move || {
                info!(
                    "launch-lol: preferred_path={:?} (empty = auto locate)",
                    preferred_path
                );
                let result = if preferred_path.trim().is_empty() {
                    lcu::cmd::launch_lol_client()
                } else {
                    lcu::cmd::launch_lol_client_with_path(Some(preferred_path.as_str()))
                };
                match &result {
                    Ok(path) => info!("launch-lol: spawned {}", path.display()),
                    Err(err) => warn!("launch-lol: ERROR {err}"),
                }
                let launch_ok = result.is_ok();
                let msg = match result {
                    Ok(path) => format!("LoL 客户端已启动: {}", path.display()),
                    Err(err) => format!("无法启动 LoL 客户端: {err}"),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = lw_worker.upgrade() {
                        win.set_launch_ok(launch_ok);
                        win.set_launch_status(SharedString::from(&msg));
                    }
                });
            })
            .map_err(|e| warn!("launch-lol: failed to spawn worker thread: {e}"))
            .ok();
    });

    let tts_window_for_open = tts_settings_window.as_weak();
    sources_window.on_open_tts_settings_clicked({
        let state_pin = state.clone();
        move || {
            if let Some(win) = tts_window_for_open.upgrade() {
                win.show().unwrap();
                pin_window_to_monitor(win.window(), &state_pin, PinAnchor::Center);
            }
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

            match result {
                Ok(reply) => {
                    let (tts_config, speech_lock) = {
                        let state = state.lock().unwrap();
                        (state.tts_config.clone(), state.speech_lock.clone())
                    };
                    tokio::task::spawn_blocking(move || {
                        let _guard = speech_lock.lock().unwrap();
                        let _ = tts::speak_windows_tts_with_config(&reply, &tts_config);
                    });
                }
                Err(err) => {
                    let message = format!("System: Coach error: {err}");
                    append_system_log(&weak, &state, &message);
                }
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
    let sources_weak_for_pin = sources_window.as_weak();
    tts_settings_window.on_apply_clicked(move || {
        let Some(win) = tts_window_for_apply.upgrade() else {
            return;
        };

        let rate = win.get_tts_rate();
        let volume = win.get_tts_volume();
        let voice = win.get_tts_voice().to_string();
        let launcher_path = win.get_lol_launcher_path().to_string();
        let deepseek_api_key = win.get_deepseek_api_key().to_string();
        let deepseek_base_url = win.get_deepseek_base_url().to_string();
        let deepseek_model = win.get_deepseek_model().to_string();
        let deepseek_thinking = win.get_deepseek_thinking();
        let deepseek_stream = win.get_deepseek_stream();
        let deepseek_reasoning_effort = win.get_deepseek_reasoning_effort().to_string();
        let ai_provider = win.get_ai_provider().to_string();
        let ai_backend = win.get_ai_backend().to_string();
        let maohou_bin = win.get_maohou_bin().to_string();
        let deepseek_web_risk_accepted = win.get_deepseek_web_risk_accepted();
        let lmstudio_base_url = win.get_lmstudio_base_url().to_string();
        let lmstudio_model = win.get_lmstudio_model().to_string();
        let lmstudio_api_key = win.get_lmstudio_api_key().to_string();
        let auto_accept_match = win.get_auto_accept_match();
        let openai_base_url = win.get_openai_base_url().to_string();
        let openai_model = win.get_openai_model().to_string();
        let openai_api_key = win.get_openai_api_key().to_string();
        // 下拉里 0 = "不固定", 实际显示器索引从 1 起
        let pinned_monitor = win.get_pinned_monitor() - 1;

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
                base_url: if deepseek_base_url.is_empty() {
                    "https://api.deepseek.com".to_string()
                } else {
                    deepseek_base_url.clone()
                },
                model: deepseek_model.clone(),
                thinking_enabled: deepseek_thinking,
                reasoning_effort: deepseek_reasoning_effort.clone(),
                stream_enabled: deepseek_stream,
            };
            state.ai_provider = ai_provider.clone();
            state.ai_backend = ai_backend.clone();
            state.auto_accept_match = auto_accept_match;
            state.maohou_bin = maohou_bin.clone();
            state.engine_fallback_warned = false;
            state.deepseek_web_risk_accepted = deepseek_web_risk_accepted;
            state.lmstudio_config = DeepSeekConfig {
                api_key: lmstudio_api_key.clone(),
                base_url: lmstudio_base_url.clone(),
                model: lmstudio_model.clone(),
                thinking_enabled: false,
                reasoning_effort: String::new(),
                stream_enabled: false,
            };
            state.openai_config = DeepSeekConfig {
                api_key: openai_api_key.clone(),
                base_url: openai_base_url.clone(),
                model: openai_model.clone(),
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
        settings.deepseek_base_url = deepseek_base_url;
        settings.deepseek_model = deepseek_model;
        settings.deepseek_thinking = deepseek_thinking;
        settings.deepseek_stream = deepseek_stream;
        settings.deepseek_reasoning_effort = deepseek_reasoning_effort;
        settings.ai_provider = ai_provider;
        settings.ai_backend = ai_backend;
        settings.maohou_bin = maohou_bin;
        settings.auto_accept_match = auto_accept_match;
        settings.deepseek_web_risk_accepted = deepseek_web_risk_accepted;
        settings.lmstudio_base_url = lmstudio_base_url;
        settings.lmstudio_model = lmstudio_model;
        settings.lmstudio_api_key = lmstudio_api_key;
        settings.openai_base_url = openai_base_url;
        settings.openai_model = openai_model;
        settings.openai_api_key = openai_api_key;
        settings.pinned_monitor = pinned_monitor;
        settings.save();

        // 显示器固定立即生效: 重摆当前可见的主窗/设置窗(隐藏窗下次 show 时生效)
        {
            tts_state_for_apply.lock().unwrap().pinned_monitor = pinned_monitor;
        }
        let win_handle = win.window();
        pin_window_to_monitor(win_handle, &tts_state_for_apply, PinAnchor::Center);
        if let Some(main_win) = sources_weak_for_pin.upgrade() {
            if main_win.window().is_visible() {
                pin_window_to_monitor(main_win.window(), &tts_state_for_apply, PinAnchor::Center);
            }
        }

        win.hide().unwrap();
    });

    let web_login_window = tts_settings_window.as_weak();
    let web_login_state = state.clone();
    let web_login_handle = rt_handle_ref.clone();
    tts_settings_window.on_open_deepseek_web_login_clicked(move || {
        let weak = web_login_window.clone();
        let state = web_login_state.clone();
        web_login_handle.spawn(async move {
            let status = match deepseek_web_sidecar(&state) {
                Ok(sidecar) => match sidecar.open_login().await {
                    Ok(status) => format!("状态: {}", status.state),
                    Err(err) => format!("启动失败: {err}"),
                },
                Err(err) => format!("启动失败: {err}"),
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_deepseek_web_status(SharedString::from(status));
                }
            });
        });
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

    // -- Runes window: close (marks this session as user-dismissed) --
    let runes_weak = runes_window.as_weak();
    runes_window.on_close_requested({
        let state_dismiss = state.clone();
        move || {
            state_dismiss.lock().unwrap().runes_window_dismissed = true;
            if let Some(win) = runes_weak.upgrade() {
                win.hide().unwrap();
            }
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

    // -- Runes window: one-click best rune for the current lane --
    let runes_best_weak = runes_window.as_weak();
    let state_best = state.clone();
    let handle_best = rt_handle_ref.clone();
    runes_window.on_apply_best_rune_clicked({
        move || {
            let (auth, champion_id, position) = {
                let s = state_best.lock().unwrap();
                (
                    s.auth_url.clone(),
                    s.current_champion_id,
                    s.current_assigned_position.clone(),
                )
            };

            if champion_id == 0 {
                let weak = runes_best_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak.upgrade() {
                        win.set_apply_rune_status(SharedString::from("请先在客户端选择英雄"));
                    }
                });
                return;
            }

            handle_best.spawn(apply_best_rune_for_position(
                runes_best_weak.clone(),
                auth,
                champion_id,
                position,
                "",
            ));
        }
    });

    // -- Runes window: one-click counter-rule rune page --
    let runes_counter_weak = runes_window.as_weak();
    let state_counter = state.clone();
    let handle_counter = rt_handle_ref.clone();
    runes_window.on_apply_counter_rune_clicked({
        move || {
            let (auth, plan) = {
                let s = state_counter.lock().unwrap();
                (s.auth_url.clone(), s.counter_plan.clone())
            };

            if plan.is_none() {
                let weak = runes_counter_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak.upgrade() {
                        win.set_apply_rune_status(SharedString::from(
                            "暂无Counter方案(需对位锁定且 counters 数据充足)",
                        ));
                    }
                });
                return;
            }

            handle_counter.spawn(apply_counter_rune_plan(
                runes_counter_weak.clone(),
                auth,
                plan.unwrap(),
                "[Counter] ",
            ));
        }
    });

    // -- Runes window: auto-apply toggle (persisted) --
    let state_auto = state.clone();
    runes_window.on_auto_apply_toggled(move |enabled| {
        state_auto.lock().unwrap().auto_apply_rune = enabled;
        let mut settings = settings::Settings::load();
        settings.auto_apply_rune = enabled;
        settings.save();
    });

    // -- Runes window: auto-builds toggle (persisted) --
    let state_auto_builds = state.clone();
    runes_window.on_auto_builds_toggled(move |enabled| {
        state_auto_builds.lock().unwrap().auto_apply_builds = enabled;
        let mut settings = settings::Settings::load();
        settings.auto_apply_builds = enabled;
        settings.save();
    });

    // -- Main window: objective reminder tier (persisted) --
    let state_tier = state.clone();
    sources_window.on_reminder_tier_changed(move |tier| {
        let tier = tier.clamp(0, 2);
        state_tier.lock().unwrap().reminder_tier = tier;
        let mut settings = settings::Settings::load();
        settings.reminder_tier = tier;
        settings.save();
    });

    // -- Main window: mini live window toggle (persisted) --
    let state_mini = state.clone();
    let mini_weak_toggle = mini_window.as_weak();
    sources_window.on_mini_live_toggled(move |enabled| {
        state_mini.lock().unwrap().mini_live_enabled = enabled;
        let mut settings = settings::Settings::load();
        settings.mini_live_window = enabled;
        settings.save();
        // 立即生效: 关闭就藏; 打开时若正在对局, 下个生命周期 tick 会显示。
        if !enabled {
            let weak = mini_weak_toggle.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.hide().unwrap();
                }
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
    let match_lifecycle_mini = mini_window.as_weak();
    rt_handle.spawn(match_lifecycle_task(
        match_lifecycle_weak,
        match_lifecycle_state,
        match_lifecycle_mini,
    ));

    // Live match panel in the main window (score/objectives/matchup, no LLM cost).
    let panel_weak = sources_window.as_weak();
    let panel_state = state.clone();
    let panel_mini = mini_window.as_weak();
    rt_handle.spawn(live_match_panel_task(panel_weak, panel_state, panel_mini));

    // Event-driven objective reminders (first blood / monsters / spawn timers).
    let reminder_weak = sources_window.as_weak();
    let reminder_state = state.clone();
    rt_handle.spawn(objective_reminder_task(reminder_weak, reminder_state));

    let names_state = state.clone();
    rt_handle.spawn(async move {
        let names = web::fetch_static_names().await;
        info!(
            "static names loaded: {} champions, {} runes, {} items",
            names.champions_cn.len(),
            names.runes_cn.len(),
            names.items_cn.len()
        );
        names_state.lock().unwrap().static_names = names;
    });

    // 对位心理图谱: DDragon championFull.json (zh_CN) 单请求全英雄。
    // 失败为空图 = 心理卡自动隐藏, 不阻塞其他功能。
    let playbook_state = state.clone();
    rt_handle.spawn(async move {
        match lcu::tips::fetch_playstyle_atlas().await {
            Ok(atlas) => {
                info!("playstyle atlas loaded: {} champions", atlas.champions.len());
                playbook_state.lock().unwrap().playbook = atlas;
            }
            Err(_) => {
                warn!("playstyle atlas fetch failed; 对位心理卡不可用");
            }
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
    // 手动固定的显示器(在 show 之后才有 size())
    pin_window_to_monitor(sources_window.window(), &state, PinAnchor::Center);
    // 系统托盘: 左键召唤主窗, 右键菜单退出。
    // 独占全屏游戏会盖住主窗 —— 没有托盘就"启动后找不到应用"。
    setup_tray(sources_window.as_weak());
    slint::run_event_loop().unwrap();
}

/// 托盘图标: 常驻, 左键点击=显示主窗(并放到前台), 菜单含"退出 ChampR"。
/// 图标在运行时用 image 现画(金底深框), 不依赖外部资源文件。
fn setup_tray(main_weak: Weak<SourcesWindow>) {
    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::{TrayIconBuilder, TrayIconEvent};

    let quit_item = MenuItem::new("退出 ChampR", true, None);
    let quit_id = quit_item.id().clone();
    let menu = Menu::with_items(&[&quit_item]).expect("tray menu");

    // 真应用图标(编译期内嵌 64px PNG): 与任务栏/Alt-Tab 同一个金色 L。
    // 解码失败才退回代码画的金底深框 —— 编译产物自包含, 不应发生。
    let icon_png = include_bytes!("../ui/icons/champr-64.png");
    let icon = image::load_from_memory(icon_png)
        .map(|img| {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            tray_icon::Icon::from_rgba(rgba.into_raw(), w, h).expect("tray icon pixels")
        })
        .expect("embedded tray icon decode");

    let _tray = TrayIconBuilder::new()
        .with_tooltip("ChampR")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .expect("create tray icon");
    // 托盘事件泵: 专用线程阻塞收 channel, 再转进 slint 事件循环。
    // tray-icon 0.19 在 Windows 上自带消息线程, 任意线程 recv 即可。
    std::thread::Builder::new()
        .name("tray-events".into())
        .spawn(move || {
            let menu_rx = tray_icon::menu::MenuEvent::receiver();
            loop {
                if let Ok(event) = TrayIconEvent::receiver().recv() {
                    if let TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let w = main_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(win) = w.upgrade() {
                                let _ = win.show();
                                win.window().request_redraw();
                            }
                        });
                    }
                }
                while let Ok(menu_event) = menu_rx.try_recv() {
                    if menu_event.id == quit_id {
                        std::process::exit(0);
                    }
                }
            }
        })
        .expect("spawn tray event thread");

    // 托盘对象必须活过整个进程生命周期
    std::mem::forget(_tray);
}

// ---------------------------------------------------------------------------
//  Task: fetch sources + champions + runes metadata at startup
// ---------------------------------------------------------------------------

async fn fetch_sources_task(sources_weak: Weak<SourcesWindow>, state: SharedState) {
    match web::fetch_champion_list().await {
        Ok(champions_map) => {
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
    // WS 连接失败去重: 只对新的 auth_url 报一次 warn。
    let mut last_ws_err: Option<String> = None;
    // 排队就绪态守卫: 每次 InProgress 翻转只允许一次自动 accept。
    let mut ready_accept_done = false;

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
                    s.current_assigned_position.clear();
                    s.last_auto_applied_champion = 0;
                    s.last_applied_plan_sig.clear();
                    s.counter_plan = None;
                    s.runes_window_dismissed = false;
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
                s.current_assigned_position.clear();
                s.last_auto_applied_champion = 0;
                s.last_applied_plan_sig.clear();
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
                info!("LCU WebSocket subscribed ({})", &current_auth_url);
                last_ws_err = None;

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

                            // 排队就绪: 自动接受对局(可选, 默认关)。
                            // 幂等守卫: InProgress 翻转瞬间只 POST 一次。
                            if uri == Some("/lol-matchmaking/v1/ready-check") {
                                let rc_state = data
                                    .and_then(|v| v.get("data"))
                                    .and_then(|v| v.get("state"))
                                    .and_then(|v| v.as_str());
                                if rc_state == Some("InProgress") {
                                    if !ready_accept_done {
                                        ready_accept_done = true;
                                        // 每次从 state 现读, 设置页开关即时生效
                                        let enabled = { state.lock().unwrap().auto_accept_match };
                                        if enabled {
                                            let url = current_auth_url.clone();
                                            tokio::spawn(async move {
                                                match lcu::lcu_api::accept_ready_check(&url).await {
                                                    Ok(_) => info!("Auto-accepted match (ready check)"),
                                                    Err(e) => warn!("Auto-accept failed: {:?}", e),
                                                }
                                            });
                                        }
                                    }
                                } else {
                                    ready_accept_done = false;
                                }
                                continue;
                            }

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
                                            s.current_assigned_position.clear();
                                            s.last_auto_applied_champion = 0;
                                            s.last_applied_plan_sig.clear();
                                            s.counter_plan = None;
                                            s.runes_window_dismissed = false;
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

                                // Keep the local lane in sync so rune suggestions stay matchup-aware.
                                let assigned_position =
                                    extract_assigned_position_from_session(session_data);
                                {
                                    let mut s = state.lock().unwrap();
                                    s.current_assigned_position = assigned_position.clone();
                                }

                                // 选人排面(双方 1~5 楼/分路/已选英雄): 每次 session 更新都刷新
                                {
                                    let roster = {
                                        let s = state.lock().unwrap();
                                        render_roster_texts(session_data, &s)
                                    };
                                    let rw = runes_weak.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(win) = rw.upgrade() {
                                            if let Some((my, enemy)) = roster {
                                                win.set_roster_my(SharedString::from(my));
                                                win.set_roster_enemy(SharedString::from(enemy));
                                            } else {
                                                win.set_roster_my(SharedString::from(""));
                                                win.set_roster_enemy(SharedString::from(""));
                                            }
                                        }
                                    });
                                }

                                // Locked-in pick: counter 符文跟随选人/对位状态持续生成。
                                // 我锁了之后, 每次 session 更新都重算方案;
                                // 方案签名变化(对手锁人/换英雄/换位置) → 重写符文页;
                                // 签名不变 → 跳过(不刷 LCU 写请求)。
                                // 对位不可识别(盲选)时退回 OP.GG 最优页, 每英雄一次。
                                let locked_cid =
                                    extract_locked_champion_id_from_session(session_data);
                                if locked_cid > 0 {
                                    let (auto_rune, auto_builds, already_applied, auth, lol_dir, is_tencent) = {
                                        let s = state.lock().unwrap();
                                        (
                                            s.auto_apply_rune,
                                            s.auto_apply_builds,
                                            s.last_auto_applied_champion == locked_cid,
                                            s.auth_url.clone(),
                                            s.lol_dir.clone(),
                                            s.is_tencent,
                                        )
                                    };
                                    if auto_rune && !auth.is_empty() {
                                        let (plan, _reason) = {
                                            let s = state.lock().unwrap();
                                            compute_counter_plan(session_data, &s)
                                        };
                                        match plan {
                                            Some(plan) => {
                                                let mut sig = format!(
                                                    "{}:{}:",
                                                    plan.primary_style_id, plan.sub_style_id
                                                );
                                                for id in &plan.selected_perk_ids {
                                                    sig.push_str(&id.to_string());
                                                    sig.push(',');
                                                }
                                                let changed = {
                                                    let mut s = state.lock().unwrap();
                                                    if s.last_applied_plan_sig != sig {
                                                        s.last_applied_plan_sig = sig;
                                                        s.counter_plan = Some(plan.clone());
                                                        true
                                                    } else {
                                                        false
                                                    }
                                                };
                                                if changed {
                                                    info!(
                                                        "auto-applying counter rune for {locked_cid}: {}",
                                                        plan.line
                                                    );
                                                    tokio::spawn(apply_counter_rune_plan(
                                                        runes_weak.clone(),
                                                        auth.clone(),
                                                        plan,
                                                        "[自动·Counter] ",
                                                    ));
                                                }
                                            }
                                            None => {
                                                if !already_applied {
                                                    info!("auto-applying rune for locked champion {locked_cid} ({assigned_position})");
                                                    tokio::spawn(apply_best_rune_for_position(
                                                        runes_weak.clone(),
                                                        auth.clone(),
                                                        locked_cid,
                                                        assigned_position.clone(),
                                                        "[自动] ",
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                    if auto_builds && !lol_dir.is_empty() && !already_applied {
                                        info!("auto-writing item builds for locked champion {locked_cid}");
                                        tokio::spawn(auto_write_builds(
                                            sources_weak.clone(),
                                            lol_dir,
                                            is_tencent,
                                            locked_cid,
                                        ));
                                    }
                                    if !already_applied {
                                        state.lock().unwrap().last_auto_applied_champion = locked_cid;
                                    }
                                }

                                // Refresh the local counter-rune plan whenever the
                                // champ-select session changes (hover / opponent pick)
                                // and publish it to the runes window.
                                let (plan_available, plan_summary, plan_newly_available, plan_status, intel_header, intel_body, war_header, war_body) = {
                                    let mut s = state.lock().unwrap();
                                    let (plan, reason) = compute_counter_plan(session_data, &s);
                                    let status = if plan.is_some() { "" } else { reason };
                                    let summary = plan.as_ref().map(|p| {
                                        let mut text = p.line.clone();
                                        if !p.diffs.is_empty() {
                                            text.push_str(&format!(
                                                "\n差异: {}",
                                                p.diffs.join("; ")
                                            ));
                                        }
                                        if let Some(reason) = p.reasons.first() {
                                            text.push_str(&format!("\n理由: {reason}"));
                                        }
                                        text
                                    });
                                    let (ih, ib) = compute_opponent_intel(session_data, &s)
                                        .map(|(h, b)| (h, b))
                                        .unwrap_or_default();
                                    let (wh, wb) = compute_war_text(session_data, &s)
                                        .map(|(h, b)| (h, b))
                                        .unwrap_or_default();
                                    let previously = s.counter_plan.is_some();
                                    let available = plan.is_some();
                                    let newly = available && !previously;
                                    s.counter_plan = plan;
                                    (available, summary, newly, status, ih, ib, wh, wb)
                                };
                                {
                                    let rw = runes_weak.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(win) = rw.upgrade() {
                                            win.set_counter_available(plan_available);
                                            win.set_counter_summary(SharedString::from(
                                                plan_summary.unwrap_or_default(),
                                            ));
                                            win.set_counter_status(SharedString::from(plan_status));
                                            win.set_opponent_header(SharedString::from(
                                                intel_header,
                                            ));
                                            win.set_opponent_intel(SharedString::from(intel_body));
                                            win.set_war_header(SharedString::from(war_header));
                                            win.set_war_body(SharedString::from(war_body));
                                            // Signal the moment a counter plan first
                                            // becomes actionable (e.g. opponent locked
                                            // after our auto-apply already did its pass).
                                            if plan_newly_available {
                                                win.set_apply_rune_status(
                                                    SharedString::from(
                                                        "对位已确认: 可应用 Counter 符文(见摘要)",
                                                    ),
                                                );
                                            }
                                        }
                                    });
                                }

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
                // 同一 auth_url 只报一次, 避免 LoL 下班期间 2.5s 刷爆日志
                let key = current_auth_url.clone();
                if last_ws_err.as_deref() != Some(key.as_str()) {
                    warn!("error creating WebSocket client: {:?}", e);
                    last_ws_err = Some(key);
                }
                // 客户端可能根本没起来(如刚启动时端口未监听):
                // 清掉 auth_url 让下一轮走完整授权流程(拿最新端口 + 召唤师名)
                current_auth_url.clear();
                auth_prompted_for_pid = None;
                {
                    let mut s = state.lock().unwrap();
                    s.auth_url.clear();
                }
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

fn deepseek_web_sidecar(state: &SharedState) -> anyhow::Result<Arc<BrowserSidecar>> {
    let mut state = state.lock().unwrap();
    if let Some(sidecar) = &state.deepseek_web {
        return Ok(sidecar.clone());
    }
    if !state.deepseek_web_risk_accepted {
        anyhow::bail!("请先在设置中确认 DeepSeek Web 实验功能风险");
    }
    let sidecar = Arc::new(BrowserSidecar::discover()?);
    state.deepseek_web = Some(sidecar.clone());
    Ok(sidecar)
}

async fn chat_with_selected_provider(
    state: &SharedState,
    messages: Vec<ChatMessage>,
) -> anyhow::Result<String> {
    let request_lock = {
        let state = state.lock().unwrap();
        state.coach_request_lock.clone()
    };
    let _request_guard = request_lock.lock().await;
    let (provider, deepseek_config, lmstudio_config, openai_config, backend, maohou_bin_setting) = {
        let state = state.lock().unwrap();
        (
            state.ai_provider.clone(),
            state.deepseek_config.clone(),
            state.lmstudio_config.clone(),
            state.openai_config.clone(),
            state.ai_backend.clone(),
            state.maohou_bin.clone(),
        )
    };
    if provider == "deepseek_web" {
        return deepseek_web_sidecar(state)?.chat_messages(messages).await;
    }
    let config = match provider.as_str() {
        "lmstudio" => lmstudio_config,
        // 任意 OpenAI 兼容端点: key 可空(本地推理站), base_url 必填
        "openai" => {
            if openai_config.base_url.is_empty() {
                anyhow::bail!("OpenAI 兼容端点: 请先在设置中填 Base URL");
            }
            openai_config
        }
        _ => {
            if deepseek_config.api_key.is_empty() {
                anyhow::bail!("DeepSeek API Key is not configured");
            }
            deepseek_config
        }
    };

    // LLM 通道: 优先 houmao 引擎子进程(与 houmao-mac 共用引擎实现对齐功能);
    // 二进制缺失 → 一次性 warn 后降级直连 reqwest(可用性优先, 见 analyzer wave1)。
    // 引擎调用失败原样上抛(不双请求双扣)。
    if backend == "maohou" {
        let bin = if !maohou_bin_setting.is_empty() {
            let p = std::path::PathBuf::from(&maohou_bin_setting);
            p.is_file().then_some(p)
        } else {
            lcu::maohou::locate_binary()
        };
        match bin {
            Some(bin) => {
                // deepseek 走字面方法(注释签名); openai/lmstudio 同一条
                // "key 可空" 通用路径(for_lmstudio 名字历史遗留, 语义是通用 OpenAI 兼容)。
                let target = if provider == "deepseek" {
                    lcu::maohou::MaohouTarget::for_deepseek(&config)
                } else {
                    lcu::maohou::MaohouTarget::for_lmstudio(
                        &config.base_url,
                        &config.model,
                        &config.api_key,
                    )
                };
                return lcu::maohou::chat(&bin, &target, &messages)
                    .await
                    .map_err(|e| anyhow::anyhow!("maohou 调用失败(provider={provider}): {e:#}"));
            }
            None => {
                let mut s = state.lock().unwrap();
                if !s.engine_fallback_warned {
                    s.engine_fallback_warned = true;
                    warn!(
                        "maohou 引擎二进制未找到(MAOHOU_BIN/兄弟仓/PATH 均无果), \
                         本次起直连 reqwest; 可安装 houmao-mac/engine 或在设置中指定路径"
                    );
                    s.ui_log
                        .push_str("[提示] 未找到 maohou 引擎二进制, LLM 走直连 reqwest(备用通道)。\n\n");
                }
            }
        }
    }
    DeepSeekClient::new(config).chat_messages(messages).await
}

/// Persist the freshly built prompt so follow-up coach questions can reuse it.
fn remember_prompt(state: &SharedState, prompt: &str) {
    let mut s = state.lock().unwrap();
    s.coach_last_prompt = prompt.to_string();
    s.last_progress_key = prompt.to_string();
}

/// Write raw interface snapshots to `.cache/` for debugging
/// (enabled with `CHAMPR_DUMP_SNAPSHOTS=1`).
async fn dump_snapshot(tag: &str, data: &Value) {
    if std::env::var("CHAMPR_DUMP_SNAPSHOTS").ok().as_deref() != Some("1") {
        return;
    }
    let Ok(pretty) = lcu::serde_json::to_string_pretty(data) else {
        return;
    };
    let dir = std::path::Path::new(".cache");
    if tokio::fs::create_dir_all(dir).await.is_err() {
        return;
    }
    let _ = tokio::fs::write(dir.join(format!("{tag}.json")), pretty).await;
}

/// Resolve the local player's champion id in a live game via the Data Dragon map.
fn live_local_champion_id(game_data: &Value, champions: &ChampionsMap) -> i64 {
    let Ok(snapshot) = lcu::match_context::LiveSnapshot::from_all_game_data(game_data) else {
        return 0;
    };
    let Some(local) = snapshot.local_player() else {
        return 0;
    };
    champions
        .values()
        .find(|c| c.name == local.champion_name || c.id == local.champion_name)
        .and_then(|c| c.key.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Fetch and cache OP.GG build sections for the given champions (skips cached ones).
async fn ensure_opgg_sections(state: &SharedState, champion_ids: &[i64]) {
    let missing: Vec<i64> = {
        let s = state.lock().unwrap();
        champion_ids
            .iter()
            .copied()
            .filter(|id| *id > 0 && !s.opgg_sections_cache.contains_key(id))
            .collect()
    };
    if missing.is_empty() {
        return;
    }

    let source = DEFAULT_SOURCE_VALUE.to_string();
    let fetched = futures_util::future::join_all(missing.iter().map(|id| {
        let source = source.clone();
        async move { (*id, web::list_builds_by_id(&source, *id).await) }
    }))
    .await;

    let mut s = state.lock().unwrap();
    for (id, result) in fetched {
        if let Ok(sections) = result {
            if !sections.is_empty() {
                s.opgg_sections_cache.insert(id, sections);
            }
        }
    }
}

fn rank_tier_zh(tier: &str) -> String {
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

fn format_ranked_stats(stats: &Value) -> Option<String> {
    let solo = stats.get("queueMap")?.get("RANKED_SOLO_5x5")?;
    let tier = solo.get("tier").and_then(Value::as_str).unwrap_or("");
    if tier.is_empty() || tier.eq_ignore_ascii_case("NONE") {
        return None;
    }
    let division = solo.get("division").and_then(Value::as_str).unwrap_or("");
    let lp = solo.get("leaguePoints").and_then(Value::as_i64).unwrap_or(0);
    let wins = solo.get("wins").and_then(Value::as_i64).unwrap_or(0);
    let losses = solo.get("losses").and_then(Value::as_i64).unwrap_or(0);
    Some(format!(
        "单双{}{}{}{}{}",
        rank_tier_zh(tier),
        division,
        if lp > 0 { format!(" {lp}胜点") } else { String::new() },
        if wins > 0 { format!(" {wins}胜") } else { String::new() },
        if losses > 0 { format!("{losses}负") } else { String::new() },
    ))
}

/// Resolve one summoner's solo queue rank text (best effort).
async fn lookup_rank_text(endpoint: &str, summoner_id: i64) -> Option<String> {
    let summoner = lcu_api::get_summoner_by_id(endpoint, summoner_id).await.ok()?;
    let puuid = summoner
        .get("puuid")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|puuid| !puuid.is_empty())?;
    let stats = lcu_api::get_ranked_stats(endpoint, &puuid).await.ok()?;
    format_ranked_stats(&stats)
}

/// Build the champ-select prompt enriched with rune page, ranks and OP.GG stats.
async fn build_champ_select_prompt(
    state: &SharedState,
    endpoint: &str,
    session: &Value,
    champions_map: &ChampionsMap,
    static_names: &web::StaticNames,
) -> anyhow::Result<String> {
    let snapshot = lcu::match_context::ChampSelectSnapshot::from_session(session)?;

    // 1) The local player's currently equipped rune page (best effort).
    let rune_page = lcu_api::get_current_rune_page(endpoint).await.ok();

    // 2) Solo queue ranks for every visible teammate/opponent (cached per summoner).
    let to_lookup: Vec<i64> = {
        let s = state.lock().unwrap();
        let known = &s.ranked_stats_cache;
        let ids: std::collections::HashSet<i64> = snapshot
            .my_team
            .iter()
            .chain(snapshot.their_team.iter())
            .map(|member| member.summoner_id)
            .filter(|id| *id > 0)
            .collect();
        ids.into_iter().filter(|id| !known.contains_key(id)).collect()
    };
    if !to_lookup.is_empty() {
        let endpoint_owned = endpoint.to_string();
        let fetched = futures_util::future::join_all(to_lookup.into_iter().map(|summoner_id| {
            let endpoint = endpoint_owned.clone();
            async move { (summoner_id, lookup_rank_text(&endpoint, summoner_id).await) }
        }))
        .await;

        let mut s = state.lock().unwrap();
        for (summoner_id, text) in fetched {
            if let Some(text) = text {
                s.ranked_stats_cache.insert(summoner_id, text);
            }
        }
    }

    // 3) OP.GG sections for every locked/hovered champion so the advisor can do
    //    matchup analysis and targeted rune comparison.
    let champion_ids: Vec<i64> = {
        let ids: std::collections::HashSet<i64> = snapshot
            .my_team
            .iter()
            .chain(snapshot.their_team.iter())
            .map(|member| member.effective_champion())
            .filter(|id| *id > 0)
            .collect();
        ids.into_iter().collect()
    };
    ensure_opgg_sections(state, &champion_ids).await;

    let (ranks, sections_map, atlas) = {
        let s = state.lock().unwrap();
        (
            s.ranked_stats_cache.clone(),
            s.opgg_sections_cache.clone(),
            s.playbook.clone(),
        )
    };

    advisor::build_lineup_prompt(
        session,
        champions_map,
        static_names,
        rune_page.as_ref(),
        &ranks,
        &sections_map,
        &atlas,
    )
}

async fn build_current_coach_prompt(state: &SharedState) -> Option<String> {
    let (auth_url, champions_map, static_names, last_prompt) = {
        let s = state.lock().unwrap();
        (
            s.auth_url.clone(),
            s.champions_map.clone(),
            s.static_names.clone(),
            s.coach_last_prompt.clone(),
        )
    };

    if auth_url.is_empty() {
        return None;
    }

    let endpoint = format!("https://{auth_url}");

    // 1) In-game: full Live Client Data (players' KDA/CS/runes/spells/items,
    //    active player's gold/abilities/stats, objective events, kill feed).
    if let Ok(game_data) = live_client::fetch_all_game_data().await {
        dump_snapshot("live-allgamedata", &game_data).await;

        let local_champion_id = live_local_champion_id(&game_data, &champions_map);
        if local_champion_id > 0 {
            ensure_opgg_sections(state, &[local_champion_id]).await;
        }
        let (sections_map, atlas) = {
            let s = state.lock().unwrap();
            (s.opgg_sections_cache.clone(), s.playbook.clone())
        };

        if let Ok(prompt) = advisor::build_live_game_prompt(
            &game_data,
            &champions_map,
            &static_names,
            Some(&sections_map),
            local_champion_id,
            &atlas,
        ) {
            remember_prompt(state, &prompt);
            return Some(prompt);
        }
    }

    // 2) Champ select: bans, teams, lane opponent, current rune page, ranks, OP.GG.
    if let Ok(session) = lcu_api::get_champ_select_session(&endpoint).await {
        dump_snapshot("champ-select-session", &session).await;
        if let Ok(prompt) =
            build_champ_select_prompt(state, &endpoint, &session, &champions_map, &static_names).await
        {
            let prompt = format!("当前无 Live Client Data，可能未进入对局\n\n{prompt}");
            remember_prompt(state, &prompt);
            return Some(prompt);
        }
    }

    // 3) Gameflow fallback: at least report both teams' champions.
    if let Ok(session) = lcu_api::get_gameflow_session(&endpoint).await {
        if let Ok(prompt) = advisor::build_gameflow_prompt(&session, &champions_map, &static_names) {
            let prompt = format!("当前无 Live Client Data，可能未进入对局\n\n{prompt}");
            remember_prompt(state, &prompt);
            return Some(prompt);
        }
    }

    if !last_prompt.is_empty() {
        return Some(format!(
            "当前无 Live Client Data，可能对局已结束或不在对局中\n\n{last_prompt}"
        ));
    }

    None
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

    let (auth_url, history) = {
        let s = state.lock().unwrap();
        (
            s.auth_url.clone(),
            s.coach_messages.clone(),
        )
    };

    if auth_url.is_empty() {
        anyhow::bail!("League Client is not connected");
    }

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

    let advice = chat_with_selected_provider(state, messages).await?;

    append_coach_message(weak, state, "assistant", &advice);

    Ok(advice)
}

async fn greet_coach(weak: Weak<SourcesWindow>, state: SharedState) {
    match send_coach_message(&weak, &state, "hi".to_string()).await {
        Ok(reply) => {
            let (tts_config, speech_lock) = {
                let s = state.lock().unwrap();
                (s.tts_config.clone(), s.speech_lock.clone())
            };
            tokio::task::spawn_blocking(move || {
                let _guard = speech_lock.lock().unwrap();
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

/// Polls the open client interfaces every few seconds and renders the compact
/// match panel text for the main window (no LLM involved).
async fn live_match_panel_task(
    weak: Weak<SourcesWindow>,
    state: SharedState,
    mini_weak: Weak<MiniMatchWindow>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(2500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_text = String::new();

    loop {
        interval.tick().await;

        let (auth_url, champions_map, static_names, ranks, sections_map) = {
            let s = state.lock().unwrap();
            (
                s.auth_url.clone(),
                s.champions_map.clone(),
                s.static_names.clone(),
                s.ranked_stats_cache.clone(),
                s.opgg_sections_cache.clone(),
            )
        };

        let mut text = String::new();
        if !auth_url.is_empty() {
            let endpoint = format!("https://{auth_url}");
            if let Ok(game_data) = live_client::fetch_all_game_data().await {
                if let Ok(rendered) =
                    advisor::build_live_panel_text(&game_data, &champions_map, &static_names)
                {
                    text = rendered;
                }
            }
            if text.is_empty() {
                if let Ok(session) = lcu_api::get_champ_select_session(&endpoint).await {
                    if let Ok(rendered) = advisor::build_champ_select_panel_text(
                        &session,
                        &champions_map,
                        &static_names,
                        &ranks,
                        &sections_map,
                    ) {
                        text = rendered;
                    }
                }
            }
        }

        if text != last_text {
            last_text = text.clone();
            let weak = weak.clone();
            let mini = mini_weak.clone();
            let text_for_main = text.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_live_match_text(SharedString::from(text_for_main));
                }
                if let Some(win) = mini.upgrade() {
                    win.set_match_text(SharedString::from(text));
                }
            });
        }
    }
}

/// Speaks short template reminders for objective events and spawn timers while
/// the match-assistance toggle is on. Live data is polled cheaply every 5s.
async fn objective_reminder_task(weak: Weak<SourcesWindow>, state: SharedState) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut engine = advisor::ReminderEngine::new();

    loop {
        interval.tick().await;

        let (auth_url, enabled, champions_map, static_names, tts_config, speech_lock, tier) = {
            let s = state.lock().unwrap();
            (
                s.auth_url.clone(),
                s.llm_assistance_enabled,
                s.champions_map.clone(),
                s.static_names.clone(),
                s.tts_config.clone(),
                s.speech_lock.clone(),
                s.reminder_tier,
            )
        };

        if !enabled || auth_url.is_empty() {
            continue;
        }

        let Ok(game_data) = live_client::fetch_all_game_data().await else {
            engine.reset();
            continue;
        };
        let Ok(snapshot) = lcu::match_context::LiveSnapshot::from_all_game_data(&game_data) else {
            continue;
        };

        for reminder in engine.collect(&snapshot, &champions_map, &static_names) {
            // Tier 1 keeps only key events (first blood / dragons / baron).
            if tier == 1 && reminder.kind != advisor::ReminderKind::EventKey {
                continue;
            }
            info!("objective reminder: {}", reminder.text);
            append_system_log(&weak, &state, &format!("提醒: {}", reminder.text));
            // Tier 2 = quiet: log only, no voice.
            if tier == 2 {
                continue;
            }
            let tts_config = tts_config.clone();
            let speech_lock = speech_lock.clone();
            let text = reminder.text;
            tokio::task::spawn_blocking(move || {
                let _guard = speech_lock.lock().unwrap();
                let _ = tts::speak_windows_tts_with_config(&text, &tts_config);
            });
        }
    }
}

async fn match_lifecycle_task(
    weak: Weak<SourcesWindow>,
    state: SharedState,
    mini_weak: Weak<MiniMatchWindow>,
) {
    let mut last_session_label = String::new();
    // Drives the mini live window per phase: shown once on entering InProgress
    // (user may close it for the rest of the game), hidden the moment the
    // game leaves InProgress.
    let mut mini_shown_this_game = false;

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

        // Mini live window orchestration (research baseline: 对局中默认只留
        // 一个置顶迷你窗, 阶段结束自动收起)。
        let current_phase = {
            let s = state.lock().unwrap();
            s.match_phase.clone()
        };
        let mini_enabled = {
            let s = state.lock().unwrap();
            s.mini_live_enabled
        };
        if current_phase == MatchPhase::InProgress && !mini_shown_this_game {
            mini_shown_this_game = true;
            if mini_enabled {
                // 铁律: 游戏必须完整独占它自己的屏。迷你窗只落在"非游戏屏";
                // 单屏 / TF识别不到游戏窗口 → 本局不弹(信息走 TTS 与主窗)。
                let mons = {
                    let s = state.lock().unwrap();
                    s.monitors.clone()
                };
                let found = game_screen::game_screen_and_hwnd(&mons);
                let target = monitors::mini_target(&mons, found.map(|(idx, _)| idx));
                match (target, found) {
                    (Some(mi), Some((_, game_hwnd))) => {
                        let monitor = mons.iter().find(|m| m.index == mi).cloned();
                        let mini = mini_weak.clone();
                        let status = label.to_string();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let (Some(win), Some(m)) = (mini.upgrade(), monitor) {
                                win.set_match_status(SharedString::from(status));
                                win.show().unwrap();
                                pin_window_on_monitor(win.window(), &m, PinAnchor::TopRight);
                                // show() 夺焦会让独占全屏游戏最小化 —— 把焦点还回去
                                game_screen::restore_focus(game_hwnd);
                            }
                        });
                    }
                    _ => {
                        kv_log_macro::info!(
                            "迷你窗本局不弹: {}",
                            if mons.len() < 2 {
                                "仅单屏, 游戏必须完整占屏".to_string()
                            } else {
                                "未识别到游戏窗口(gamescreen unknown)".to_string()
                            }
                        );
                    }
                }
            }
        } else if current_phase != MatchPhase::InProgress {
            if mini_shown_this_game {
                let mini = mini_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = mini.upgrade() {
                        win.hide().unwrap();
                    }
                });
            }
            mini_shown_this_game = false;
        }
    }
}

async fn advice_loop(sources_weak: Weak<SourcesWindow>, state: SharedState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Token saver: when nothing changed since the last call (idle lobby,
    // paused champ select, loading screen), skip the request entirely.
    let mut last_sent_prompt: Option<String> = None;

    loop {
        interval.tick().await;

        let (auth_url, deepseek_api_key, ai_provider) = {
            let s = state.lock().unwrap();
            (
                s.auth_url.clone(),
                s.deepseek_config.api_key.clone(),
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

        if ai_provider == "deepseek" && deepseek_api_key.is_empty() {
            let weak = sources_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_advice_text(SharedString::from("DeepSeek API Key 未配置"));
                }
            });
            continue;
        }

        let prompt = match build_current_coach_prompt(&state).await {
            Some(prompt) => prompt,
            None => "当前无法读取实时对局数据，请给出通用对局建议，并提醒玩家等待数据恢复。".to_string(),
        };

        // Skip a byte-identical resend: any actual state motion changes the prompt
        // (game clock, KDA, picks, bans, phase), so this only silences dead time.
        if last_sent_prompt.as_deref() == Some(prompt.as_str()) {
            continue;
        }
        last_sent_prompt = Some(prompt.clone());

        let history = {
            let s = state.lock().unwrap();
            s.coach_messages.clone()
        };
        let context_message = format!("LIVE MATCH STATE UPDATE\n{prompt}");
        let mut messages = vec![ChatMessage::system(advisor::DEFAULT_SYSTEM_PROMPT)];
        messages.extend(history);
        messages.push(ChatMessage::user(context_message.clone()));

        append_coach_message(&sources_weak, &state, "user", &context_message);

        match chat_with_selected_provider(&state, messages).await {
            Ok(advice) => {
                append_coach_message(&sources_weak, &state, "assistant", &advice);
                let tts_text = advice.clone();
                let weak = sources_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = weak.upgrade() {
                        win.set_advice_text(SharedString::from(&advice));
                    }
                });
                let (tts_config_for_speech, speech_lock) = {
                    let s = state.lock().unwrap();
                    (s.tts_config.clone(), s.speech_lock.clone())
                };
                tokio::task::spawn_blocking(move || {
                    let _guard = speech_lock.lock().unwrap();
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

/// Locate the local player's entry inside a champ-select session's myTeam array.
fn find_local_member<'a>(session: &'a Value) -> Option<&'a Value> {
    let cell_id = session.get("localPlayerCellId")?.as_i64()?;
    session
        .get("myTeam")?
        .as_array()?
        .iter()
        .find(|member| member.get("cellId").and_then(|v| v.as_i64()) == Some(cell_id))
}

/// The local player's assigned lane ("top"/"jungle"/"middle"/"bottom"/"utility").
fn extract_assigned_position_from_session(session: Option<&Value>) -> String {
    session
        .and_then(find_local_member)
        .and_then(|member| member.get("assignedPosition"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// The locked-in champion of the local player (0 while only hovering).
/// In the myTeam array, championId is set only after the pick is locked.
fn extract_locked_champion_id_from_session(session: Option<&Value>) -> i64 {
    session
        .and_then(find_local_member)
        .and_then(|member| member.get("championId"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
//  Apply the best OP.GG rune page for the current lane
// ---------------------------------------------------------------------------

/// Compute the local counter-rule rune plan from the current champ-select
/// session. Pure CPU (snapshot + cached OP.GG sections), tuned per matchup.
/// Returns None when no lane matchup is known or no rule fires.
/// 返回 (方案, 失败原因)。没有方案时原因必须能在 UI 上说清(反静默失败)。
fn compute_counter_plan(
    session_data: Option<&Value>,
    s: &AppState,
) -> (Option<lcu::counter::RunePlan>, &'static str) {
    let Some(session) = session_data else {
        return (None, "选人会话未建立");
    };
    let Ok(snapshot) = lcu::match_context::ChampSelectSnapshot::from_session(session) else {
        return (None, "选人数据格式不兼容");
    };
    let Some(local) = snapshot.local_member() else {
        return (None, "本地玩家未识别");
    };
    let local_id = local.effective_champion();
    if local_id == 0 {
        return (None, "先悬停或锁定你的英雄");
    }
    if local.assigned_position.is_empty() {
        return (None, "本模式没有分路信息(排位选人期可用)");
    }
    let Some(opponent) = snapshot.lane_opponent() else {
        return (None, "对位英雄未识别(对面分路数据缺失)");
    };
    let opponent_id = opponent.effective_champion();
    if opponent_id == 0 {
        return (None, "等对面选出你的对位英雄");
    }
    let Some(opponent_info) = s
        .champions_map
        .values()
        .find(|c| c.key == opponent_id.to_string())
    else {
        return (None, "冠军表缺该英雄");
    };
    let profile = lcu::counter::profile_of(opponent_info);
    let Some(sections) = s.opgg_sections_cache.get(&local_id) else {
        return (None, "OP.GG 数据还在拉取, 稍后再看");
    };
    let Some(base) = lcu::advisor::best_rune_for_position(sections, &local.assigned_position) else {
        return (None, "该英雄本位置没有推荐符文页");
    };
    match lcu::counter::plan_for_matchup_reasons(
        base,
        local_id,
        &local.assigned_position,
        opponent_id,
        &opponent_info.id.to_lowercase(),
        Some(&profile),
        &s.opgg_sections_cache,
    ) {
        Ok(plan) => (Some(plan), ""),
        Err(reason) => (None, reason),
    }
}

/// 冠军 id → 中文名(静态名表优先, 兜底 DDragon 英文名)。
fn zh_champion_name(s: &AppState, champion_id: i64) -> Option<String> {
    let key = champion_id.to_string();
    let champ = s.champions_map.values().find(|c| c.key == key)?;
    Some(
        s.static_names
            .champion(&champ.key)
            .map(str::to_string)
            .unwrap_or_else(|| champ.name.clone()),
    )
}

/// 选人排面: 我方/敌方 1~5 楼 + 分路 + 已选英雄, 每次 session 更新都刷。
/// 卡片挂在符文窗标题卡下方; session 结构不兼容时静默隐藏卡(返回 None)。
fn render_roster_texts(session_data: Option<&Value>, s: &AppState) -> Option<(String, String)> {
    let session = session_data?;
    let snap = lcu::match_context::ChampSelectSnapshot::from_session(session).ok()?;
    let my = snap.roster_line(false, &|id| zh_champion_name(s, id));
    let enemy = snap.roster_line(true, &|id| zh_champion_name(s, id));
    Some((format!("我方  {my}"), format!("敌方  {enemy}")))
}

/// 渲染当前对位英雄的心理图谱(UI "敌方心理"卡的原始文本)。
/// 与 compute_counter_plan 共用同一 lane 判定, 但对符文规则不敏感:
/// 只要对面已选出对位英雄就显示, 即使数据样本不足以出符文方案。
fn compute_opponent_intel(
    session_data: Option<&Value>,
    s: &AppState,
) -> Option<(String, String)> {
    let session = session_data?;
    let snapshot = lcu::match_context::ChampSelectSnapshot::from_session(session).ok()?;
    let opponent = snapshot.lane_opponent()?;
    let opp_id = opponent.effective_champion();
    if opp_id == 0 {
        return None;
    }
    let champ = s
        .champions_map
        .values()
        .find(|c| c.key == opp_id.to_string())?;
    let zh = s
        .static_names
        .champion(&champ.key)
        .map(str::to_string)
        .unwrap_or_else(|| champ.name.clone());
    s.playbook.render_opponent(&champ.key.clone(), &zh)
}

/// 组装心战卡(战略+战术): 敌我均选自即出, 与 counter 符文互为基准。
fn compute_war_text(
    session_data: Option<&Value>,
    s: &AppState,
) -> Option<(String, String)> {
    let session = session_data?;
    let snapshot = lcu::match_context::ChampSelectSnapshot::from_session(session).ok()?;
    let local = snapshot.local_member()?;
    let local_id = local.effective_champion();
    let opponent = snapshot.lane_opponent()?;
    let opp_id = opponent.effective_champion();
    if local_id == 0 || opp_id == 0 {
        return None;
    }
    let own_info = s
        .champions_map
        .values()
        .find(|c| c.key == local_id.to_string())?;
    let opp_info = s
        .champions_map
        .values()
        .find(|c| c.key == opp_id.to_string())?;
    let pressure = lcu::counter::pressure_of_matchup(
        local_id,
        &local.assigned_position,
        opp_id,
        &opp_info.id.to_lowercase(),
        &s.opgg_sections_cache,
    );
    let card = lcu::war::WarSystem::load().war_card(own_info, opp_info, &pressure);
    Some(lcu::war::render_ui(&card))
}

/// Apply a counter-rule rune plan produced by the local rule engine.
async fn apply_counter_rune_plan(
    runes_weak: Weak<RunesWindow>,
    auth_url: String,
    plan: lcu::counter::RunePlan,
    status_prefix: &str,
) {
    if auth_url.is_empty() {
        return;
    }

    let set_status = |text: String| {
        let weak = runes_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak.upgrade() {
                win.set_apply_rune_status(SharedString::from(text));
            }
        });
    };

    let line = plan.line.clone();
    set_status(format!("{status_prefix}正在应用Counter符文: {line}"));

    let endpoint = format!("https://{auth_url}");
    let rune = plan.to_rune_page();
    let msg = match lcu_api::apply_rune(endpoint, rune).await {
        Ok(()) => format!("{status_prefix}已应用Counter符文({line})"),
        Err(err) => format!("{status_prefix}Counter符文应用失败: {err:?}"),
    };
    info!("apply counter rune: {msg}");
    set_status(msg);
}

async fn apply_best_rune_for_position(
    runes_weak: Weak<RunesWindow>,
    auth_url: String,
    champion_id: i64,
    assigned_position: String,
    status_prefix: &str,
) {
    if auth_url.is_empty() {
        return;
    }

    let set_status = |text: String| {
        let weak = runes_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak.upgrade() {
                win.set_apply_rune_status(SharedString::from(text));
            }
        });
    };

    set_status(format!("{status_prefix}正在应用本路最优符文…"));

    let source = DEFAULT_SOURCE_VALUE.to_string();
    let result = web::list_builds_by_id(&source, champion_id).await;
    let rune = result.ok().and_then(|sections| {
        let section_position = advisor::best_section(&sections, &assigned_position)
            .map(|section| section.position.clone())
            .unwrap_or_default();
        advisor::best_rune_for_position(&sections, &assigned_position)
            .cloned()
            .map(|rune| (rune, section_position))
    });

    let Some((rune, section_position)) = rune else {
        set_status(format!("{status_prefix}无可用符文数据(OP.GG)"));
        return;
    };

    let endpoint = format!("https://{auth_url}");
    let msg = match lcu_api::apply_rune(endpoint, rune).await {
        Ok(()) => {
            let zh = lcu::match_context::position_label(&section_position);
            format!("{status_prefix}已应用{zh}最优符文({champion_id})")
        }
        Err(err) => format!("{status_prefix}应用失败: {err:?}"),
    };
    info!("apply best rune: {msg}");
    set_status(msg);
}

// ---------------------------------------------------------------------------
//  Auto write recommended item builds on champion lock-in
// ---------------------------------------------------------------------------

async fn auto_write_builds(
    sources_weak: Weak<SourcesWindow>,
    lol_dir: String,
    is_tencent: bool,
    champion_id: i64,
) {
    let set_status = |applying: bool, ok: bool, text: String| {
        let weak = sources_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(win) = weak.upgrade() {
                win.set_applying_builds(applying);
                win.set_apply_ok(ok);
                win.set_apply_status(SharedString::from(text));
            }
        });
    };

    set_status(true, false, format!("自动写入推荐出装(英雄 {champion_id})…"));

    let source = DEFAULT_SOURCE_VALUE.to_string();
    let result = lcu::builds::apply_builds_from_id(&lol_dir, &source, champion_id, is_tencent).await;
    let ok = result.is_ok();
    let msg = if ok {
        format!("已自动写入推荐出装(英雄 {champion_id})")
    } else {
        format!("自动写入出装失败(英雄 {champion_id})")
    };
    info!("auto write builds: {msg}");
    set_status(false, ok, msg);
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

    // Determine champion name from champions_map, preferring the zh_CN static name.
    let champion_name = {
        let s = state.lock().unwrap();
        s.champions_map
            .values()
            .find(|c| c.key == champion_id.to_string())
            .map(|c| {
                s.static_names
                    .champion(&c.key)
                    .map(str::to_string)
                    .unwrap_or_else(|| c.name.clone())
            })
            .unwrap_or_default()
    };

    // Current lane label for position-aware rune suggestions.
    let position_zh = {
        let s = state.lock().unwrap();
        lcu::match_context::position_label(&s.current_assigned_position)
    };

    // 用户手动关过符文窗本次选人就不再自动弹出(尊重显式关闭意图);
    // 窗口状态仍会更新, 重新打开时就是最新数据。
    let user_dismissed = { state.lock().unwrap().runes_window_dismissed };

    // Update the runes window with champion info
    let weak = runes_weak.clone();
    let champ_name = SharedString::from(&champion_name);
    let state_pin = state.clone();

    let _ = slint::invoke_from_event_loop(move || {
        if let Some(win) = weak.upgrade() {
            win.set_champion_id(champion_id as i32);
            win.set_champion_name(champ_name);
            win.set_has_champion(true);
            let position_label = if position_zh == "待分配" {
                String::new()
            } else {
                position_zh.clone()
            };
            win.set_position_label(SharedString::from(position_label));

            if let Some(px) = avatar_pixels {
                let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &px.rgba_data,
                    px.width,
                    px.height,
                );
                win.set_champion_avatar(Image::from_rgba8(buffer));
            }

            if !user_dismissed {
                win.show().unwrap();
                pin_window_to_monitor(win.window(), &state_pin, PinAnchor::Center);
            }
        }
    });

    fetch_and_show_runes(
        runes_weak,
        state,
        DEFAULT_SOURCE_VALUE.to_string(),
        champion_id,
        0,
    )
    .await;
}

// ---------------------------------------------------------------------------
//  显示器固定
//  原则: 用户在设置里手动选屏; 自动弹出的窗口(runes/mini)也受同一约束,
//  保证游戏主屏不被遮挡。坐标全是物理像素, 与 EnumDisplayMonitors 的输出一致。
// ---------------------------------------------------------------------------

/// 窗口在目标屏上的落位方式。
enum PinAnchor {
    /// 工作区右上角(迷你窗, 贴"视口角落"更自然)
    TopRight,
    /// 工作区居中
    Center,
}

/// 将窗口放进 pinned_monitor 指定显示器的工作区。
/// 在 `show()` 之后调用才能拿到尺寸做精确锚定; 未配置/无该显示器时 no-op。
fn pin_window_to_monitor(window: &slint::Window, state: &SharedState, anchor: PinAnchor) {
    let (idx, mons) = {
        let Ok(s) = state.lock() else {
            return;
        };
        (s.pinned_monitor, s.monitors.clone())
    };
    if idx < 0 {
        return;
    }
    let Some(m) = mons.get(idx as usize) else {
        // 索引越界 = 之前配置的显示器现在不在, 静默回退不固定
        return;
    };
    let (l, t, r, _b) = m.work_rect;
    let size = window.size(); // 物理像素; show() 后才有意义
    let (x, y) = match anchor {
        PinAnchor::TopRight => ((r - size.width as i32).max(l) - 24, t + 40),
        PinAnchor::Center => (
            (l + r).div_euclid(2) - (size.width as i32).div_euclid(2),
            (t + _b).div_euclid(2) - (size.height as i32).div_euclid(2),
        ),
    };
    window.set_position(slint::WindowPosition::Physical(slint::PhysicalPosition::new(x, y)));
}

/// 将窗口放进**指定**显示器的工作区(不读 settings, 调用方已经决定了屏)。
/// 迷你窗用这条: 目标屏由 game_screen 规则求出, 与用户 pinned_monitor 无关。
fn pin_window_on_monitor(window: &slint::Window, m: &monitors::Monitor, anchor: PinAnchor) {
    let (l, t, r, b) = m.work_rect;
    let size = window.size();
    let (x, y) = match anchor {
        PinAnchor::TopRight => ((r - size.width as i32).max(l) - 24, t + 40),
        PinAnchor::Center => (
            (l + r).div_euclid(2) - (size.width as i32).div_euclid(2),
            (t + b).div_euclid(2) - (size.height as i32).div_euclid(2),
        ),
    };
    window.set_position(slint::WindowPosition::Physical(slint::PhysicalPosition::new(x, y)));
}

// ---------------------------------------------------------------------------
//  Fetch runes for a champion from a source and display them
// ---------------------------------------------------------------------------

/// 递归安全包装: 8s 重试的 tokio::spawn 要求 Future: Send + 'static,
/// 直接自递归会造成环; 用 BoxFuture 把返回类型定下来。
fn fetch_and_show_runes(
    runes_weak: Weak<RunesWindow>,
    state: SharedState,
    source: String,
    champion_id: i64,
    attempt: u32,
) -> futures_util::future::BoxFuture<'static, ()> {
    Box::pin(fetch_and_show_runes_inner(
        runes_weak,
        state,
        source,
        champion_id,
        attempt,
    ))
}

async fn fetch_and_show_runes_inner(
    runes_weak: Weak<RunesWindow>,
    state: SharedState,
    source: String,
    champion_id: i64,
    attempt: u32,
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
            // 评审 should-fix 落地: 选人阶段的确定性数据通道。
            // counter 规则引擎/兵法压力档都消费 opgg_sections_cache;
            // 此前该缓存只在 LLM prompt 路径回填 → 关闭 LLM 辅助时
            // counter 符文和心战战术档被静默饿死。选定英雄抓取 sections 时
            // 直接入库, 不再依赖 LLM 开关。
            {
                let mut s = state.lock().unwrap();
                s.opgg_sections_cache
                    .insert(champion_id, sections.clone());
            }
            let assigned_position = {
                let s = state.lock().unwrap();
                s.current_assigned_position.clone()
            };
            let mut runes: Vec<Rune> = sections.iter().flat_map(|s| s.runes.clone()).collect();
            // Put the pages for the current lane first (stable sort keeps OP.GG's
            // popularity order inside each group).
            if !assigned_position.is_empty() {
                let aliases = lcu::match_context::opgg_position_aliases(&assigned_position);
                runes.sort_by_key(|rune| {
                    let on_lane = aliases
                        .iter()
                        .any(|a| rune.position.eq_ignore_ascii_case(a));
                    if on_lane {
                        0
                    } else {
                        1
                    }
                });
            }

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
        Err(err) => {
            // 反静默失败: 真因上状态行(server 没起/网络断/反序列化错都写清),
            // 并在用户还在选这英雄时 8 秒后自动重试一次(server 可能慢起)。
            let err_text = format!("{err:?}");
            warn!("fetch_and_show_runes({champion_id}): {err_text}");
            let hint = if err_text.contains("3030") || err_text.contains("connect") {
                format!("符文数据加载失败: 后端服务未运行({err_text}); 检查 ChampR Server 窗口")
            } else {
                format!("符文数据加载失败: {err_text}")
            };
            let weak = runes_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_rune_status(SharedString::from("error"));
                    win.set_apply_rune_status(SharedString::from(&hint));
                }
            });

            if attempt == 0 {
                let rw = runes_weak.clone();
                let st = state.clone();
                let src = source.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(8)).await;
                    let current = st.lock().unwrap().current_champion_id;
                    if current == champion_id && current > 0 {
                        info!("retrying rune fetch for {champion_id} after 8s");
                        fetch_and_show_runes(rw, st, src, champion_id, 1).await;
                    }
                });
            }
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
