#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kv_log_macro::{info, warn};
use slint::{
    ComponentHandle, Image, Model, ModelRc, SharedPixelBuffer, SharedString, VecModel, Weak,
};

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
mod cache;
mod clipboard;
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
    /// 自动禁人 / 自动选人(用户 2026-10-05)。默认关, 需先配名单再打开。
    auto_ban: bool,
    auto_pick: bool,
    /// 优先禁用名单(英雄 id, 按顺序取第一个还没被禁的)
    auto_ban_list: Vec<i64>,
    /// 选人自动锁定阈值(秒): 剩余时间 <= 该值就锁定; 0 = 悬停后立刻锁定
    auto_pick_lock_seconds: f64,
    /// 上一次已提交的自动动作(action_id, champion_id, completed)。
    /// 避免同一个动作每来一次 session 事件就重复 PATCH。
    auto_action_last: Option<(i64, i64, bool)>,
    /// 排队就绪自动接受对局(设置可开关, 默认关)。
    auto_accept_match: bool,
    /// Objective reminder tier: 0 = all, 1 = key events only, 2 = quiet (log only).
    reminder_tier: i32,
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
    /// 手动点选对位: 用户在符文窗 Counter 卡上点的敌方英雄 id。
    /// 腾讯 LCU 可能把敌方分路藏起来导致自动对位失效时的人工兜底;
    /// 我方换英雄/会话重建即清空。
    manual_counter_target: Option<i64>,
    /// 主窗输出区最后一次**手动**切 Tab 的时刻。
    /// 手动切台后 UI_TAB_MANUAL_HOLD 内不被"谁输出谁上前台"抢走 ——
    /// 用户正在读大师回答时, 2.5s 一次的对局快照不许把它顶掉。
    ui_tab_manual_at: Option<std::time::Instant>,
    /// 符文对比卡内容(我方 vs 对位符文特性 + 扬长避短), 由对局轮询计算。
    rune_compare: Option<(String, String)>,
    /// 选人期缓存的选手档案(段位/OP.GG 分路胜率), 开局后合并进同一张状态表格。
    match_roster: Vec<lcu::advisor::RosterEntry>,
    /// 「对局数据」表格的纯文本版本(表格是自绘的, 选中不了, 复制按钮用这份文本)。
    live_table_text: String,
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
    ranked_stats_cache: HashMap<i64, lcu::advisor::RankInfo>,
    /// 段位/个人胜率的抓取时间(summonerId -> unix 秒), 落盘用 —— 避免"每局重新查一遍"(用户 2026-10-05)
    ranked_stats_at: HashMap<i64, u64>,
    /// OP.GG 分路数据的抓取时间(英雄 id -> unix 秒), 落盘用
    opgg_sections_at: HashMap<i64, u64>,
    /// 上次尝试抓取 OP.GG 数据的时间(含失败), 只用于节流重试, 不落盘
    opgg_attempt_at: HashMap<i64, std::time::Instant>,
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
        // 启动时先读磁盘缓存: 熟面孔/常见英雄直接命中, 不再"每局重查一遍"
        let (cached_sections, cached_section_times) = cache::load_sections();
        let (cached_ranks, cached_rank_times) = cache::load_ranks();
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
            auto_ban: false,
            auto_pick: false,
            auto_ban_list: Vec::new(),
            auto_pick_lock_seconds: 0.0,
            auto_action_last: None,
            auto_accept_match: true,
            reminder_tier: 0,
            pinned_monitor: -1,
            monitors: Vec::new(),
            playbook: lcu::tips::PlaystyleAtlas::default(),
            last_auto_applied_champion: 0,
            last_applied_plan_sig: String::new(),
            counter_plan: None,
    manual_counter_target: None,
    ui_tab_manual_at: None,
    rune_compare: None,
    live_table_text: String::new(),
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
            opgg_sections_cache: cached_sections,
            ranked_stats_cache: cached_ranks,
            ranked_stats_at: cached_rank_times,
            opgg_sections_at: cached_section_times,
            opgg_attempt_at: HashMap::new(),
            match_id: String::new(),
            match_phase: MatchPhase::Idle,
            match_roster: Vec::new(),
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

/// 日志同时写控制台与文件。
///
/// 起因(2026-10-04): 控制台窗口一关(或 app 退出后启动器窗口消失), 日志就没了,
/// "启动后什么都没有"变成无法复盘的悬案 —— 已发生两次。落盘后任何异常都留痕。
struct TeeLogger {
    /// 文件句柄; None = 落盘失败(只打控制台)
    file: Option<std::sync::Mutex<std::fs::File>>,
}

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // 控制台: 保留 femme 风格的 "target 消息", 便于对照历史日志
        eprintln!("{} {}", record.target(), record.args());
        if let Some(file) = &self.file {
            if let Ok(mut file) = file.lock() {
                use std::io::Write;
                let _ = writeln!(
                    file,
                    "{:>5} {} {}",
                    record.level(),
                    record.target(),
                    record.args()
                );
                let _ = file.flush();
            }
        }
    }

    fn flush(&self) {}
}

/// 日志初始化: 控制台 + `.cache/champr.log`(>2MB 轮转一次), 并挂 panic 钩子。
fn init_logging() {
    let path = std::path::Path::new(".cache/champr.log");
    let _ = std::fs::create_dir_all(".cache");
    // 简单轮转: 上一份留成 champr.log.1
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > 2 * 1024 * 1024 {
            let _ = std::fs::rename(path, ".cache/champr.log.1");
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(std::sync::Mutex::new);

    log::set_max_level(log::LevelFilter::Info);
    let _ = log::set_boxed_logger(Box::new(TeeLogger { file }));

    // panic 也要留痕: 默认只在 stderr 打, 控制台关了就没证据
    std::panic::set_hook(Box::new(|info| {
        let text = format!("PANIC: {info}");
        log::error!("{text}");
        eprintln!("{text}");
    }));
}

fn main() {
    init_logging();
    info!("=== ChampR starting (pid {}) ===", std::process::id());

    // 一次性剪贴板自检入口(自动化验证用): 设 CHAMPR_CLIPBOARD_SELFTEST=<文本> 时
    // 只把文本写进剪贴板并退出, 不建窗口。0 = 成功, 2 = 失败。
    if let Ok(text) = std::env::var("CHAMPR_CLIPBOARD_SELFTEST") {
        let ok = clipboard::set_text(&text);
        info!("clipboard selftest: ok={ok}");
        std::process::exit(if ok { 0 } else { 2 });
    }

    // -- Create windows --
    // 符文窗已并入主窗(用户 2026-10-04 拍板): 符文面板现在是主窗的「符文」Tab,
    // 所有 runes_* 弱引用都指向主窗(sources_window), 变量名保留以压小改动面。
    let sources_window = SourcesWindow::new().unwrap();
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
    initial_state.ai_backend = saved_settings.ai_backend.clone();
    initial_state.maohou_bin = saved_settings.maohou_bin.clone();
    initial_state.deepseek_web_risk_accepted = saved_settings.deepseek_web_risk_accepted;
    initial_state.auto_apply_rune = saved_settings.auto_apply_rune;
    initial_state.auto_apply_builds = saved_settings.auto_apply_builds;
    initial_state.auto_accept_match = saved_settings.auto_accept_match;
    initial_state.auto_ban = saved_settings.auto_ban;
    initial_state.auto_pick = saved_settings.auto_pick;
    initial_state.auto_ban_list = saved_settings.auto_ban_list.clone();
    initial_state.auto_pick_lock_seconds = saved_settings.auto_pick_lock_seconds;
    initial_state.reminder_tier = saved_settings.reminder_tier;
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
    sources_window.set_auto_apply_enabled(saved_settings.auto_apply_rune);
    sources_window.set_auto_builds_enabled(saved_settings.auto_apply_builds);
    tts_settings_window.set_auto_accept_match(saved_settings.auto_accept_match);
    // 自动禁人/选人: 开关与名单都推给界面(名单在界面上按英雄中文名显示)
    sources_window.set_auto_ban_enabled(saved_settings.auto_ban);
    sources_window.set_auto_pick_enabled(saved_settings.auto_pick);
    {
        let names_text = |ids: &[i64]| -> String {
            ids.iter()
                .map(|id| {
                    state
                        .lock()
                        .ok()
                        .and_then(|s| {
                            s.static_names
                                .champion(&id.to_string())
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| id.to_string())
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        let ban_text = names_text(&saved_settings.auto_ban_list);
        sources_window.set_auto_ban_list_text(SharedString::from(&ban_text));

        let mut status = format!(
            "自动禁人 {} | 自动选人 {}{}",
            if saved_settings.auto_ban { "开" } else { "关" },
            if saved_settings.auto_pick { "开" } else { "关" },
            if saved_settings.auto_pick_lock_seconds > 0.0 {
                format!(" | 锁定阈值 {:.0}s", saved_settings.auto_pick_lock_seconds)
            } else {
                " | 悬停即锁定".to_string()
            }
        );
        if saved_settings.auto_ban && saved_settings.auto_ban_list.is_empty() {
            status.push_str(" | 禁用名单为空, 自动禁人不会生效");
        }
        sources_window.set_auto_select_status(SharedString::from(&status));
        info!("auto champ-select: {status}");
    }
    sources_window.set_reminder_tier(saved_settings.reminder_tier);

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

    // -- Apply Builds button removed (2026-10-02, 用户拍板) --
    // 出装写入是往客户端 Config\Champions\<英雄>\Recommended 写 item set JSON,
    // 只有进游戏开商店才看得见结果, 本机从未成功产出过文件(目录都不存在),
    // 属于"看不见又没反馈"的僵尸功能。锁后自动出装开关保留(默认关)。
    let rt_handle = tokio::runtime::Runtime::new().unwrap();
    let rt_handle_ref = rt_handle.handle().clone();

    // -- 主窗输出区: 手动切 Tab(45 秒静默期, 期间自动跟随让位) --
    let state_tab = state.clone();
    sources_window.on_output_tab_clicked(move |tab| {
        state_tab.lock().unwrap().ui_tab_manual_at = Some(std::time::Instant::now());
        info!("output tab switched manually -> {tab} (auto-follow held {UI_TAB_MANUAL_HOLD:?})");
    });
    // 打字续期: 只更新时间戳, 不刷日志(每个按键都触发)
    let state_hold = state.clone();
    sources_window.on_output_tab_hold(move || {
        state_hold.lock().unwrap().ui_tab_manual_at = Some(std::time::Instant::now());
    });

    // -- 复制输出区(用户 2026-10-05: 输出框均需要支持直接复制) --
    // 表格与符文卡片是自绘的 Text, 系统层面选不中; TextEdit(选人面板/大师对话)
    // 本来就能 Ctrl+C, 这里也给个一键复制整段。
    let copy_weak = sources_window.as_weak();
    let copy_state = state.clone();
    sources_window.on_copy_output_clicked(move |tab| {
        let win = copy_weak.upgrade();
        // 符文页与大师对话直接复制"界面上正在显示的文字"(props 是唯一真源);
        // 对局表格是自绘的行模型, 复制 Rust 侧同时生成的纯文本版本。
        let (text, what) = match (tab, win.as_ref()) {
            (UI_TAB_RUNES, Some(win)) => (render_runes_copy_text(win), "符文页"),
            (UI_TAB_MATCH, _) => (
                copy_state
                    .lock()
                    .map(|s| s.live_table_text.clone())
                    .unwrap_or_default(),
                "对局数据",
            ),
            (_, Some(win)) => (win.get_coach_chat_log().to_string(), "大师对话"),
            _ => (String::new(), "输出区"),
        };

        let status = if text.trim().is_empty() {
            format!("{what} 暂无内容")
        } else if clipboard::set_text(&text) {
            let lines = text.lines().count();
            info!(
                "copied {what} to clipboard ({lines} lines, {} chars)",
                text.len()
            );
            format!("已复制 {what} {lines} 行")
        } else {
            // 剪贴板被别的程序占着时 OpenClipboard 会失败 —— 明确说出来, 不静默
            warn!("failed to write {what} to clipboard");
            "复制失败: 剪贴板被占用".to_string()
        };

        if let Some(win) = copy_weak.upgrade() {
            win.set_copy_status(SharedString::from(&status));
        }
        // 3 秒后清掉提示
        let clear_weak = copy_weak.clone();
        slint::Timer::single_shot(Duration::from_secs(3), move || {
            if let Some(win) = clear_weak.upgrade() {
                win.set_copy_status(SharedString::from(""));
            }
        });
    });

    // -- 主窗不再有「启动/唤出 LoL」按钮(用户 2026-10-03 精简) --
    // 启动: 程序启动后 0.5s 自动拉起(见 auto-launch task), 无需按钮;
    // 唤出: 托盘右键菜单「唤出 LoL 客户端窗口」。

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

    // 打开日志: 启动器不再留常驻控制台, 日志只在 .cache/champr.log 里, 所以给个入口。
    sources_window.on_open_log_clicked(|| {
        let log = std::path::Path::new(".cache/champr.log");
        if !log.exists() {
            warn!("log file not found: {}", log.display());
            return;
        }
        // explorer 用默认程序打开(通常记事本); 不弹控制台
        match std::process::Command::new("explorer").arg(log).spawn() {
            Ok(_) => info!("opened log file {}", log.display()),
            Err(err) => warn!("failed to open log file: {err}"),
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

    // -- Runes panel: 选择符文后应用(原符文窗按钮, 现属主窗「符文」Tab) --
    let runes_weak = sources_window.as_weak();
    let state_c = state.clone();
    let handle_c = rt_handle_ref.clone();
    sources_window.on_apply_rune_clicked({
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
    let runes_best_weak = sources_window.as_weak();
    let state_best = state.clone();
    let handle_best = rt_handle_ref.clone();
    sources_window.on_apply_best_rune_clicked({
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
    let runes_counter_weak = sources_window.as_weak();
    let state_counter = state.clone();
    let handle_counter = rt_handle_ref.clone();
    sources_window.on_apply_counter_rune_clicked({
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

    // -- Runes window: 手动点选对位(用户指定"我这局打谁") --
    // 自动识别履带失效(敌方分路隐藏/盲选)时的人工兜底。
    let runes_pick_weak = sources_window.as_weak();
    let state_pick = state.clone();
    let handle_pick = rt_handle_ref.clone();
    sources_window.on_counter_pick_opponent(move |enemy_cid| {
        let (auth, me) = {
            let mut s = state_pick.lock().unwrap();
            if s.current_champion_id == 0 {
                return;
            }
            s.manual_counter_target = Some(enemy_cid as i64);
            (s.auth_url.clone(), s.current_champion_id)
        };
        if auth.is_empty() {
            let weak = runes_pick_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_apply_rune_status(SharedString::from("LCU 尚未连接"));
                }
            });
            return;
        }
        info!("manual counter target picked: enemy {enemy_cid} vs my {me}");
        let win_weak = runes_pick_weak.clone();
        let state2 = state_pick.clone();
        handle_pick.spawn(async move {
            let session = match lcu::lcu_api::get_champ_select_session(&auth).await {
                Ok(v) => v,
                Err(e) => {
                    let weak = win_weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = weak.upgrade() {
                            win.set_counter_status(SharedString::from(format!(
                                "手动对位失败: 选人会话读不到({e:?})"
                            )));
                        }
                    });
                    return;
                }
            };
            let (available, summary, status, intel, war) = {
                let mut s = state2.lock().unwrap();
                let (plan, reason) =
                    compute_counter_plan(Some(&session), &s, Some(enemy_cid as i64));
                let availability = plan.is_some();
                let status_text = if availability { String::new() } else { reason.to_string() };
                let summary_text = plan.as_ref().map(|p| {
                    let mut text = p.line.clone();
                    if !p.diffs.is_empty() {
                        text.push_str(&format!("\n差异: {}", p.diffs.join("; ")));
                    }
                    if let Some(first) = p.reasons.first() {
                        text.push_str(&format!("\n理由: {first}"));
                    }
                    text
                });
                // 一次点选同时点亮三张对位卡(counter / 对位心理 / 兵法心战)
                let intel = compute_opponent_intel(Some(&session), &s, Some(enemy_cid as i64))
                    .unwrap_or_default();
                let war = compute_war_text(Some(&session), &s, Some(enemy_cid as i64))
                    .unwrap_or_default();
                s.counter_plan = plan;
                (availability, summary_text, status_text, intel, war)
            };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = win_weak.upgrade() {
                    win.set_counter_available(available);
                    win.set_counter_summary(SharedString::from(summary.unwrap_or_default()));
                    win.set_counter_status(SharedString::from(status));
                    win.set_opponent_header(SharedString::from(intel.0));
                    win.set_opponent_intel(SharedString::from(intel.1));
                    win.set_war_header(SharedString::from(war.0));
                    win.set_war_body(SharedString::from(war.1));
                    if available {
                        win.set_apply_rune_status(SharedString::from(
                            "对位已手动确认: 可点 [应用 Counter 符文]",
                        ));
                    }
                }
            });
        });
    });

    // -- Runes window: auto-apply toggle (persisted) --
    let state_auto = state.clone();
    sources_window.on_auto_apply_toggled(move |enabled| {
        state_auto.lock().unwrap().auto_apply_rune = enabled;
        let mut settings = settings::Settings::load();
        settings.auto_apply_rune = enabled;
        settings.save();
    });

    // -- Runes window: auto-builds toggle (persisted) --
    let state_auto_builds = state.clone();
    sources_window.on_auto_builds_toggled(move |enabled| {
        state_auto_builds.lock().unwrap().auto_apply_builds = enabled;
        let mut settings = settings::Settings::load();
        settings.auto_apply_builds = enabled;
        settings.save();
    });

    // -- 自动禁人 / 自动选人(用户 2026-10-05): 开关与名单都持久化 --
    // 关掉时顺手清掉"上次已提交"的记录, 免得下次打开误以为已提交过。
    let state_auto_ban = state.clone();
    sources_window.on_auto_ban_toggled(move |enabled| {
        let mut s = state_auto_ban.lock().unwrap();
        s.auto_ban = enabled;
        s.auto_action_last = None;
        drop(s);
        let mut settings = settings::Settings::load();
        settings.auto_ban = enabled;
        settings.save();
        info!("auto ban toggled: {enabled}");
    });

    let state_auto_pick = state.clone();
    sources_window.on_auto_pick_toggled(move |enabled| {
        let mut s = state_auto_pick.lock().unwrap();
        s.auto_pick = enabled;
        s.auto_action_last = None;
        drop(s);
        let mut settings = settings::Settings::load();
        settings.auto_pick = enabled;
        settings.save();
        info!("auto pick toggled: {enabled}");
    });

    let state_ban_list = state.clone();
    sources_window.on_auto_ban_list_edited(move |text| {
        let ids = parse_champion_list(&text, &state_ban_list);
        {
            let mut s = state_ban_list.lock().unwrap();
            s.auto_ban_list = ids.clone();
            s.auto_action_last = None;
        }
        let mut settings = settings::Settings::load();
        settings.auto_ban_list = ids.clone();
        settings.save();
        info!("auto ban list updated: {ids:?}");
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

    // -- 自绘标题栏(无边框主窗): 拖动 / 最小化 / 双击最大化 / 关闭 --
    // Slint 没有内置的"拖动窗口"能力, 只能由标题栏把指针位移交给这里, 再 set_position。
    {
        let drag_weak = sources_window.as_weak();
        sources_window.on_title_drag(move |dx, dy| {
            if let Some(win) = drag_weak.upgrade() {
                let window = win.window();
                // 最大化状态下拖动: 先还原, 否则位置改不动(系统行为)
                if window.is_maximized() {
                    window.set_maximized(false);
                }
                let scale = window.scale_factor() as f32;
                let pos = window.position();
                window.set_position(slint::PhysicalPosition::new(
                    pos.x + (dx * scale).round() as i32,
                    pos.y + (dy * scale).round() as i32,
                ));
            }
        });
    }
    {
        let minimize_weak = sources_window.as_weak();
        sources_window.on_title_minimize_clicked(move || {
            if let Some(win) = minimize_weak.upgrade() {
                win.window().set_minimized(true);
            }
        });
    }
    {
        let maximize_weak = sources_window.as_weak();
        sources_window.on_title_maximize_clicked(move || {
            if let Some(win) = maximize_weak.upgrade() {
                let window = win.window();
                let next = !window.is_maximized();
                window.set_maximized(next);
                info!("main window maximize -> {next}");
            }
        });
    }
    {
        let close_weak = sources_window.as_weak();
        sources_window.on_title_close_clicked(move || {
            info!("title bar close clicked; quitting");
            let _ = close_weak.upgrade();
            let _ = slint::quit_event_loop();
        });
    }

    // -- Spawn background tasks --
    let sources_weak2 = sources_window.as_weak();
    let state_c2 = state.clone();
    rt_handle.spawn(fetch_sources_task(sources_weak2, state_c2));

    let runes_weak2 = sources_window.as_weak();
    let sources_weak3 = sources_window.as_weak();
    let state_c3 = state.clone();
    rt_handle.spawn(lcu_monitor_task(sources_weak3, runes_weak2, state_c3));

    // 用户拍板(2026-10-02): 主窗不再显示 "LM Studio 在线/离线" 灯——
    // 一律走 API, 本地推理站健康状态与主界面状态无关。
    // 对应的 health_loop/models_url/health_status 三个函数一并删除。

    let match_lifecycle_weak = sources_window.as_weak();
    let match_lifecycle_state = state.clone();
    rt_handle.spawn(match_lifecycle_task(
        match_lifecycle_weak,
        match_lifecycle_state,
    ));

    // Live match panel in the main window (score/objectives/matchup, no LLM cost).
    let panel_weak = sources_window.as_weak();
    let panel_state = state.clone();
    rt_handle.spawn(live_match_panel_task(panel_weak, panel_state));

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
    let auto_launch_state = state.clone();
    rt_handle.spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;

        let msg = if lcu::cmd::check_if_lol_running() {
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
                Ok(path) => format!("LoL client launched: {}", path.display()),
                Err(err) => format!("Unable to launch LoL client: {err}"),
            }
        };
        info!("auto-launch on startup: {msg}");
    });

    // -- Show sources window and run event loop --
    // 统一深色(用户 2026-10-03 拍板): Tokens 只管我们自绘的部分,
    // TextEdit/LineEdit/CheckBox/ComboBox 跟随 OS 主题, 只能从 Rust 强制翻转
    // (Palette.color-scheme 与风格内部 FluentPalette.color-scheme 双向绑定)。
    // 赋值会解掉它到系统主题的绑定, 因此之后系统换主题也不会把我们带回浅色。
    let dark = slint::language::ColorScheme::Dark;
    sources_window
        .global::<Palette>()
        .set_color_scheme(dark);
    tts_settings_window
        .global::<Palette>()
        .set_color_scheme(dark);

    // 构建戳: 一眼看出跑的是哪一版(排查"改了没生效")
    let build_stamp = format!(
        "{}{}",
        option_env!("CHAMPR_BUILD_HASH").unwrap_or("unknown"),
        option_env!("CHAMPR_BUILD_DIRTY").unwrap_or("")
    );
    sources_window.set_build_stamp(SharedString::from(&build_stamp));
    info!("ChampR build {build_stamp}");

    // 关闭主窗 = 退出程序。
    // Slint 的默认行为只是把窗口藏起来、事件循环继续跑, 于是进程留在后台 ——
    // 表现是"关了窗口但进程还在", 启动器无法优雅重启(只能强杀 → exit 1 → 误报失败,
    // 2026-10-05 事故)。托盘仍在, 退出也可以走托盘菜单。
    sources_window.on_close_requested(|| {
        info!("main window close requested; quitting");
        let _ = slint::quit_event_loop();
    });

    sources_window.show().unwrap();
    // 手动固定的显示器(在 show 之后才有 size())
    pin_window_to_monitor(sources_window.window(), &state, PinAnchor::Center);
    // show() 刚回来时布局可能还没落定(size()==0 → 摆位无效), 延迟补摆一次。
    {
        let win_weak = sources_window.as_weak();
        let state_pin = state.clone();
        slint::Timer::single_shot(Duration::from_millis(200), move || {
            if let Some(win) = win_weak.upgrade() {
                pin_window_to_monitor(win.window(), &state_pin, PinAnchor::Center);
                let pos = win.window().position();
                let size = win.window().size();
                info!(
                    "main window placed: pos=({}, {}) size={}x{}",
                    pos.x, pos.y, size.width, size.height
                );
            }
        });
    }
    // 窗口几何写日志: "启动后什么都看不见" 这类问题, 有坐标就能一眼判断
    // 是没起来、还是跑到屏幕外去了(2026-10-03)。
    {
        let w = sources_window.window();
        let pos = w.position();
        let size = w.size();
        info!(
            "main window shown: pos=({}, {}) size={}x{} logical, monitors={}",
            pos.x,
            pos.y,
            size.width,
            size.height,
            state.lock().map(|s| s.monitors.len()).unwrap_or(0)
        );
    }
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
    // 从主窗快捷操作里挪过来的能力(用户 2026-10-03 拍板精简主窗):
    // LoL 客户端窗口被全屏游戏盖住/丢失时, 这里是唯一还能把它拉回来的入口。
    let summon_lol_item = MenuItem::new("唤出 LoL 客户端窗口", true, None);
    let summon_lol_id = summon_lol_item.id().clone();
    let menu = Menu::with_items(&[&summon_lol_item, &quit_item]).expect("tray menu");

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
                    if menu_event.id == summon_lol_id {
                        let activated = game_screen::activate_lol_client_window();
                        if activated {
                            info!("tray: LoL client window brought to front");
                        } else {
                            warn!("tray: no LoL client window to summon");
                        }
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
    // 启动器现在是"服务端与 app 并发拉起"(不再等端口), 所以后端可能比我们晚就绪。
    // 没有这层重试的话, 冠军表拉取失败会让中文名/counter/排面全线降级(2026-10-04)。
    let mut attempt: u32 = 0;
    const MAX_ATTEMPTS: u32 = 40; // 3s * 40 ≈ 2 分钟
    loop {
        attempt += 1;
        match web::fetch_champion_list().await {
            Ok(champions_map) => {
                let count = champions_map.len();
                {
                    let mut s = state.lock().unwrap();
                    s.champions_map = champions_map;
                }
                info!("champion list loaded: {count} champions (attempt {attempt})");
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = sources_weak.upgrade() {
                        win.set_status(SharedString::from("success"));
                    }
                });
                return;
            }
            Err(err) => {
                // 第 1 次和之后每 5 次写一行, 免得 3 秒一条刷屏
                if attempt == 1 || attempt % 5 == 0 {
                    warn!(
                        "champion list fetch failed (attempt {attempt}/{MAX_ATTEMPTS}): {err:?} — 后端可能还没起来, 3s 后重试"
                    );
                }
                if attempt >= MAX_ATTEMPTS {
                    warn!("champion list unavailable after {MAX_ATTEMPTS} attempts; 数据源相关功能将降级");
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = sources_weak.upgrade() {
                            win.set_status(SharedString::from("error"));
                        }
                    });
                    return;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
//  Task: LCU process polling + WebSocket champion-select monitoring
// ---------------------------------------------------------------------------

async fn lcu_monitor_task(
    sources_weak: Weak<SourcesWindow>,
    runes_weak: Weak<SourcesWindow>,
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
                    s.manual_counter_target = None;
                    s.current_champion_alias.clear();
                    s.current_assigned_position.clear();
                    s.last_auto_applied_champion = 0;
                    s.last_applied_plan_sig.clear();
                    s.counter_plan = None;
                }

                let sw = sources_weak.clone();
                let rw = runes_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = sw.upgrade() {
                        win.set_lcu_status(SharedString::from("disconnected"));
                        win.set_lcu_summoner(SharedString::from(""));
                    }
                    // 符文面板现在就在主窗里: 清数据即可, 不再 hide 一个独立窗口
                    // (对主窗调用 hide() 会把整个界面藏起来 —— 2026-10-04 合并时特别注意)
                    if let Some(win) = rw.upgrade() {
                        win.set_has_champion(false);
                        win.set_champion_id(0);
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
                    // 主窗不再显示 LoL 运行灯(用户精简); 这里只保证窗口重绘
                    win.window().request_redraw();
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
                                // 诊断留痕: 不受 DUMP 开关约束的常开落盘, 覆盖最新一局。
                                // 用来回答"对面分路到底给不给"这种杀手锏问题。
                                if let Some(session) = data.and_then(|v| v.get("data")) {
                                    dump_champ_select_session(session).await;
                                }
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
                                            s.manual_counter_target = None;
                                            s.current_champion_alias.clear();
                                            s.current_assigned_position.clear();
                                            s.last_auto_applied_champion = 0;
                                            s.last_applied_plan_sig.clear();
                                            s.counter_plan = None;
                                            // 注意: 这里**不能**清 match_roster —— 选人会话
                                            // Delete 恰恰是"选人结束、正在进游戏"的时刻, 清掉就
                                            // 会让对局里的段位/OP.GG 胜率全变 "-"(用户 2026-10-05
                                            // 报的"队友英雄胜率拿不到"就是这个)。
                                            // 档案会在下一次选人开始时整体刷新, 并在对局结束
                                            // (阶段回 Idle)时清空, 见 match_lifecycle_task。
                                            s.auto_action_last = None;
                                        }
                                        let rw = runes_weak.clone();
                                        let _ = slint::invoke_from_event_loop(move || {
                                            if let Some(win) = rw.upgrade() {
                                                win.set_has_champion(false);
                                                win.set_champion_id(0);
                                            }
                                        });
                                    }
                                    continue;
                                }

                                // Extract champion ID from session data
                                let session_data = data.and_then(|v| v.get("data"));
                                let cid = extract_champion_id_from_session(session_data);

                                // 自动禁人 / 自动选人(用户 2026-10-05): 决策在 lcu::autopick,
                                // 这里只在"轮到我的动作"时提交。LCU 不许并发改同一个动作,
                                // 所以每次 session 事件最多提交一个动作, 由下一次事件驱动下一步。
                                if let Some(session) = session_data {
                                    maybe_auto_champ_select_action(
                                        &state,
                                        session,
                                        &current_auth_url,
                                    )
                                    .await;
                                }

                                // Keep the local lane in sync so rune suggestions stay matchup-aware.
                                let assigned_position =
                                    extract_assigned_position_from_session(session_data);
                                {
                                    let mut s = state.lock().unwrap();
                                    s.current_assigned_position = assigned_position.clone();
                                }

                                // 选人排面(双方 1~5 楼/分路/已选英雄): 每次 session 更新都刷新;
                                // 顺带刷新 Counter 卡的手动对位候选(敌方已选/悬停英雄)。
                                {
                                    let (roster, cand_ids, cand_names) = {
                                        let s = state.lock().unwrap();
                                        let (ids, names) = counter_candidates(session_data, &s);
                                        (render_roster_texts(session_data, &s), ids, names)
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
                                            win.set_counter_cand_ids(ModelRc::new(VecModel::from(
                                                cand_ids,
                                            )));
                                            win.set_counter_cand_names(ModelRc::new(VecModel::from(
                                                cand_names
                                                    .iter()
                                                    .map(SharedString::from)
                                                    .collect::<Vec<_>>(),
                                            )));
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
                                            compute_counter_plan(
                                                session_data,
                                                &s,
                                                s.manual_counter_target,
                                            )
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
                                    let (plan, reason) = compute_counter_plan(
                                        session_data,
                                        &s,
                                        s.manual_counter_target,
                                    );
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
                                    let (ih, ib) = compute_opponent_intel(
                                        session_data,
                                        &s,
                                        s.manual_counter_target,
                                    )
                                    .map(|(h, b)| (h, b))
                                    .unwrap_or_default();
                                    let (wh, wb) =
                                        compute_war_text(session_data, &s, s.manual_counter_target)
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
                                        s.manual_counter_target = None;
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

/// 主窗内容区 Tab: 0 = 符文(选人/符文), 1 = 对局数据(2.5s 覆盖快照), 2 = 大师对话(追加流)。
const UI_TAB_RUNES: i32 = 0;
const UI_TAB_MATCH: i32 = 1;
const UI_TAB_COACH: i32 = 2;
/// 手动切台后的静默期: 期间谁输出都不抢台。
const UI_TAB_MANUAL_HOLD: Duration = Duration::from_secs(45);

/// 单一输出区"谁刚出内容谁上前台"(用户 2026-10-02 拍板)。
/// 手动点过 Tab 的静默期内直接让位给用户 —— 可解释优先于自动。
fn activate_output_tab(weak: &Weak<SourcesWindow>, state: &SharedState, tab: i32) {
    {
        let s = state.lock().unwrap();
        if let Some(at) = s.ui_tab_manual_at {
            if at.elapsed() < UI_TAB_MANUAL_HOLD {
                return;
            }
        }
    }
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(win) = weak.upgrade() {
            win.set_output_tab(tab);
        }
    });
}

fn refresh_coach_chat_log(weak: &Weak<SourcesWindow>, state: &SharedState) {
    let output_log = {
        let s = state.lock().unwrap();
        s.ui_log.clone()
    };
    // 大师出了新内容 → 输出区切到对话页(手动静默期内不抢)
    activate_output_tab(weak, state, UI_TAB_COACH);
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

/// 选人会话常开留痕: 每次选人更新就覆盖 `.cache/champ-select-session.json`。
/// (.cache 已 gitignore; 仅选人阶段落盘, 对局期间不写文件)
async fn dump_champ_select_session(session: &Value) {
    let Ok(pretty) = lcu::serde_json::to_string_pretty(session) else {
        return;
    };
    let dir = std::path::Path::new(".cache");
    let _ = tokio::fs::create_dir_all(dir).await;
    let _ = tokio::fs::write(dir.join("champ-select-session.json"), pretty).await;
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

/// 取 OP.GG 分路数据(按英雄 id), 命中缓存/在重试冷却期内直接返回。
///
/// 用户 2026-10-05: "每次对局都要重新查一遍胜率" —— 对局任务每 2.5s 会为场上所有英雄
/// 调一次这里, 所以必须:
///   1. 已缓存(含启动时从磁盘读入)的不再回源
///   2. 上次尝试失败的, 十分钟内不重试(否则每 2.5s 白跑一次)
///   3. 新拿到的数据落盘, 下次启动直接命中
async fn ensure_opgg_sections(state: &SharedState, champion_ids: &[i64]) {
    let now = std::time::Instant::now();
    // "每个英雄只查一次": 规则在 cache::select_fetches 里, 有单测锁住
    let missing: Vec<i64> = {
        let s = state.lock().unwrap();
        cache::select_fetches(
            champion_ids,
            |id| s.opgg_sections_cache.contains_key(&id),
            |id| s.opgg_attempt_at.get(&id).copied(),
            now,
        )
    };
    if missing.is_empty() {
        return;
    }
    // 让日志能自证"每个英雄只查一次": 只有真正要抓的才会出现, 之后每次都是空的
    info!(
        "OP.GG 抓取 {} 个英雄(其余走缓存, 不再重复查): {:?}",
        missing.len(),
        missing
    );

    {
        // 先记下尝试时间: 即使失败也进冷却, 避免每 tick 重试
        let mut s = state.lock().unwrap();
        for id in &missing {
            s.opgg_attempt_at.insert(*id, now);
        }
    }

    let source = DEFAULT_SOURCE_VALUE.to_string();
    let fetched = futures_util::future::join_all(missing.iter().map(|id| {
        let source = source.clone();
        async move { (*id, web::list_builds_by_id(&source, *id).await) }
    }))
    .await;

    let stamp = cache::now_secs();
    let mut s = state.lock().unwrap();
    let mut changed = false;
    for (id, result) in fetched {
        match result {
            Ok(sections) if !sections.is_empty() => {
                s.opgg_sections_cache.insert(id, sections);
                s.opgg_sections_at.insert(id, stamp);
                changed = true;
            }
            Ok(_) => {
                // 后端没有这个英雄的数据: 记日志, 冷却期内不再打扰
                s.opgg_attempt_at.insert(id, std::time::Instant::now());
            }
            Err(err) => {
                log::warn!("OP.GG 分路数据抓取失败(champion {id}): {err:?}");
            }
        }
    }
    if changed {
        cache::save_sections(&s.opgg_sections_cache, &s.opgg_sections_at);
    }
}

/// 查一个召唤师的排位信息(段位 + 个人胜率 + 战绩), best effort。
/// 结构体与解析都在 `lcu::advisor::RankInfo`(可单测), 这里只负责调 LCU 接口。
async fn lookup_rank_text(endpoint: &str, summoner_id: i64) -> Option<lcu::advisor::RankInfo> {
    let summoner = lcu_api::get_summoner_by_id(endpoint, summoner_id).await.ok()?;
    let puuid = summoner
        .get("puuid")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|puuid| !puuid.is_empty())?;
    let stats = lcu_api::get_ranked_stats(endpoint, &puuid).await.ok()?;
    lcu::advisor::parse_ranked_stats(&stats)
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
        let now = cache::now_secs();
        for (summoner_id, text) in fetched {
            if let Some(text) = text {
                s.ranked_stats_at.insert(summoner_id, now);
                s.ranked_stats_cache.insert(summoner_id, text);
            }
        }
        // 落盘: 下一局/下次启动直接命中, 不再重复查(用户 2026-10-05 "每次对局都要重新查一遍")
        cache::save_ranks(&s.ranked_stats_cache, &s.ranked_stats_at);
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

/// 把「符文」页当前显示的内容拼成可复制的纯文本。
///
/// 直接读 props(界面唯一真源), 因此复制到的和看到的一定一致; 卡片按界面顺序排列,
/// 空卡片跳过。自绘的 Text 无法选中, 这是它唯一的导出通道。
fn render_runes_copy_text(win: &SourcesWindow) -> String {
    /// 追加一段(标题 + 正文); 正文空白则整段跳过。
    fn push_section(out: &mut String, title: &str, body: &str) {
        let body = body.trim();
        if body.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        if !title.is_empty() {
            out.push_str(&format!("【{title}】\n"));
        }
        out.push_str(body);
        out.push('\n');
    }

    let mut out = String::new();

    let champion = win.get_champion_name().to_string();
    let position = win.get_position_label().to_string();
    let header = match (champion.is_empty(), position.is_empty()) {
        (false, false) => format!("{champion} · {position}"),
        (false, true) => champion.clone(),
        _ => String::new(),
    };
    if !header.is_empty() {
        out.push_str(&format!("【{header}】\n"));
    }

    // 推荐符文页: 模型里的每一行(名字/分路/场次/胜率), 界面上是卡片, 这里拍平成行
    let runes = win.get_runes();
    for index in 0..runes.row_count() {
        if let Some(rune) = runes.row_data(index) {
            let mut line = format!("{}. {}", index + 1, rune.name);
            if !rune.position.is_empty() {
                line.push_str(&format!(" [{}]", rune.position));
            }
            if !rune.win_rate.is_empty() {
                line.push_str(&format!(" 胜率 {} 场次 {}", rune.win_rate, rune.pick_count));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }

    push_section(&mut out, "对位方案", &win.get_counter_summary());
    push_section(&mut out, "对位心理", &win.get_opponent_intel());
    push_section(&mut out, "兵法心战", &win.get_war_body());
    push_section(&mut out, "符文对比", &win.get_rune_compare_body());
    push_section(&mut out, "我方阵容", &win.get_roster_my());
    push_section(&mut out, "敌方阵容", &win.get_roster_enemy());

    out.trim_end().to_string()
}

/// 表格内边距/色条/列间距(逻辑像素), 与 app.slint 的 DataTable 保持一致。
const TABLE_STRIP_W: f32 = 3.0;
const TABLE_GAP: f32 = 4.0;
/// 页面 + 卡片的内边距合计(逻辑像素)。
const TABLE_CHROME: f32 = 2.0 * 16.0 + 2.0 * 12.0 + 8.0;

/// 把"弹性列"(宽度 <= 0, 例如装备列)折算成实际像素。
///
/// 为什么不在 Slint 里用 horizontal-stretch: 给 Text 写 width: 0px 会把宽度钉死,
/// stretch 不生效, 那一列直接消失(2026-10-05 预览发现装备列不见了)。
/// 所以在这里按窗口实际宽度算出来, 列宽全部显式给出。
fn resolve_flex_columns(columns: &mut [advisor::TableColumn], content_width: f32) {
    let count = columns.len() as f32;
    if count == 0.0 {
        return;
    }
    let fixed: f32 = columns
        .iter()
        .filter(|col| col.width > 0)
        .map(|col| col.width as f32)
        .sum();
    let overhead = TABLE_STRIP_W + 16.0 + TABLE_GAP * (count - 1.0);
    let flex = (content_width - fixed - overhead).max(64.0);
    for col in columns.iter_mut() {
        if col.width <= 0 {
            col.width = flex.round() as i32;
        }
    }
}

/// 表格可用内容宽度(逻辑像素): 窗口宽 - 页面/卡片内边距。
fn table_content_width(win: &SourcesWindow) -> f32 {
    let scale = win.window().scale_factor() as f32;
    let physical = win.window().size().width as f32;
    let logical = if scale > 0.0 { physical / scale } else { physical };
    (logical - TABLE_CHROME).max(320.0)
}

/// 把用户输入的英雄名单("暗裔剑魔,九尾妖狐" / "Aatrox 103")解析成英雄 id。
/// 认三种写法: 中文名、Data Dragon 别名(英文)、数字 key。认不出来的词写日志跳过,
/// 不静默丢弃(用户会想知道哪个词没生效)。
fn parse_champion_list(text: &SharedString, state: &SharedState) -> Vec<i64> {
    let (names, champions) = {
        let Ok(s) = state.lock() else {
            return Vec::new();
        };
        (s.static_names.clone(), s.champions_map.clone())
    };

    let mut out: Vec<i64> = Vec::new();
    for raw in text.split([',', '，', ';', '；', ' ', '\n', '\t']) {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        let mut found: Option<i64> = None;
        // 数字 key / 纯数字 id
        if let Ok(id) = token.parse::<i64>() {
            if champions
                .values()
                .any(|info| info.key == token)
                || names.champion(token).is_some()
            {
                found = Some(id);
            }
        }
        if found.is_none() {
            // 中文名(静态名表反查)
            if let Some((key, _)) = names
                .champions_cn
                .iter()
                .find(|(_, value)| value.as_str() == token)
            {
                found = key.parse::<i64>().ok();
            }
        }
        if found.is_none() {
            // Data Dragon 别名(英文)
            if let Some(info) = champions.get(token) {
                found = info.key.parse::<i64>().ok();
            }
        }
        match found {
            Some(id) if !out.contains(&id) => out.push(id),
            Some(_) => {}
            None => warn!("auto champ-select list: unknown champion '{token}', ignored"),
        }
    }
    out
}

/// 自动禁人/选人的执行器。
///
/// 决策本身在 `lcu::autopick`(纯函数 + 单测): 只处理 actorCellId == 本机的、
/// 正在进行且未完成的动作。这里负责读取偏好、提交 PATCH、写日志与播报。
async fn maybe_auto_champ_select_action(
    state: &SharedState,
    session: &Value,
    auth_url: &str,
) {
    let (prefs, names, sections) = {
        let Ok(s) = state.lock() else {
            return;
        };
        (
            lcu::autopick::AutoPrefs {
                auto_ban: s.auto_ban,
                auto_pick: s.auto_pick,
                ban_list: s.auto_ban_list.clone(),
                lock_before_seconds: s.auto_pick_lock_seconds,
            },
            s.static_names.clone(),
            s.opgg_sections_cache.clone(),
        )
    };
    if !prefs.any_enabled() {
        return;
    }

    let (action_id, champion_id, completed, kind, reason) =
        match lcu::autopick::decide(session, &prefs, &names, &sections) {
            lcu::autopick::AutoAction::None => return,
            lcu::autopick::AutoAction::Ban {
                action_id,
                champion_id,
                reason,
            } => (action_id, champion_id, true, "ban", reason),
            lcu::autopick::AutoAction::Pick {
                action_id,
                champion_id,
                completed,
                reason,
            } => (action_id, champion_id, completed, "pick", reason),
        };

    if champion_id <= 0 {
        return;
    }

    // 同一个动作的同一个提交只做一次; 等客户端回执(下一次 session 事件)再继续
    {
        let Ok(mut s) = state.lock() else {
            return;
        };
        if s.auto_action_last == Some((action_id, champion_id, completed)) {
            return;
        }
        s.auto_action_last = Some((action_id, champion_id, completed));
    }

    match lcu::lcu_api::patch_champ_select_action(auth_url, action_id, champion_id, completed).await
    {
        Ok(_) => {
            info!(
                "auto {kind}: {reason} (action={action_id} champion={champion_id} completed={completed})"
            );
            // 播报一句, 让用户知道工具替他做了什么(选人阶段没有游戏内干扰)
            let say = match kind {
                "ban" => format!("已禁用{}", champion_name_of(state, champion_id)),
                _ if completed => format!("已锁定{}", champion_name_of(state, champion_id)),
                _ => format!("已预选{}", champion_name_of(state, champion_id)),
            };
            speak_status(state, &say);
        }
        Err(err) => {
            warn!("auto {kind} failed (action={action_id} champion={champion_id}): {err:?}");
            // 失败(常见: 还没轮到我 / 客户端拒绝)要允许下一次 session 事件重试,
            // 否则自动动作会永远卡在"已提交"状态。
            if let Ok(mut s) = state.lock() {
                s.auto_action_last = None;
            }
        }
    }
}

/// 英雄中文名(查不到就退化成 id)。
fn champion_name_of(state: &SharedState, champion_id: i64) -> String {
    state
        .lock()
        .ok()
        .and_then(|s| {
            s.static_names
                .champion(&champion_id.to_string())
                .map(str::to_string)
        })
        .unwrap_or_else(|| champion_id.to_string())
}

/// 短语音播报(best-effort: 没配 TTS 或失败都不影响主流程)。
fn speak_status(state: &SharedState, text: &str) {
    let (config, lock) = {
        let Ok(s) = state.lock() else {
            return;
        };
        (s.tts_config.clone(), s.speech_lock.clone())
    };
    let text = text.to_string();
    tokio::task::spawn_blocking(move || {
        let _guard = lock.lock().unwrap();
        if let Err(err) = tts::speak_windows_tts_with_config(&text, &config) {
            warn!("auto champ-select speech failed: {err}");
        }
    });
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
) {
    let mut interval = tokio::time::interval(Duration::from_millis(2500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_text = String::new();

    loop {
        interval.tick().await;

        let (auth_url, champions_map, static_names, ranks) = {
            let s = state.lock().unwrap();
            (
                s.auth_url.clone(),
                s.champions_map.clone(),
                s.static_names.clone(),
                s.ranked_stats_cache.clone(),
            )
        };

        // 表格优先: 对局中给实时表, 选人阶段给阵容表 —— 同一个 DataTable 形态,
        // 用户 2026-10-05 要求"展示数据就用表格, 从选人到对局一直使用"。
        let mut table: Option<advisor::DataTable> = None;
        let mut text = String::new();
        // 选人期缓存的选手档案(段位/OP.GG 胜率): 开局后合进同一张状态表格,
        // 这样列在整局里不变, 只是逐渐填满(用户 2026-10-05 的要求)。
        let cached_roster = {
            state
                .lock()
                .map(|s| s.match_roster.clone())
                .unwrap_or_default()
        };
        if !auth_url.is_empty() {
            let endpoint = format!("https://{auth_url}");
            if let Ok(game_data) = live_client::fetch_all_game_data().await {
                // 对局中也要为**全部 10 个玩家**准备 OP.GG 分路数据: 队友/对手的胜率列
                // 就靠它(选人期缓存缺失时是唯一来源; 用户 2026-10-05 报"队友胜率拿不到")。
                {
                    let ids: Vec<i64> = game_data
                        .get("allPlayers")
                        .and_then(|value| value.as_array())
                        .map(|players| {
                            players
                                .iter()
                                .filter_map(|player| {
                                    let alias = player.get("championName")?.as_str()?;
                                    champions_map
                                        .get(alias)
                                        .and_then(|info| info.key.parse::<i64>().ok())
                                })
                                .filter(|id| *id > 0)
                                .collect::<std::collections::HashSet<i64>>()
                                .into_iter()
                                .collect()
                        })
                        .unwrap_or_default();
                    if !ids.is_empty() {
                        ensure_opgg_sections(&state, &ids).await;
                    }
                }
                let sections_now = {
                    let s = state.lock().unwrap();
                    s.opgg_sections_cache.clone()
                };
                match advisor::build_live_table(
                    &game_data,
                    &champions_map,
                    &static_names,
                    &cached_roster,
                    &sections_now,
                ) {
                    Ok(built) if !built.rows.is_empty() => table = Some(built),
                    _ => {
                        // Live 数据在但字段不全: 退回文字面板, 至少不显示空白
                        if let Ok(rendered) =
                            advisor::build_live_panel_text(&game_data, &champions_map, &static_names)
                        {
                            text = rendered;
                        }
                    }
                }
            }
            if table.is_none() {
                if let Ok(session) = lcu_api::get_champ_select_session(&endpoint).await {
                    // 用户 2026-10-05: "每个选定英雄的时候就查一次即可" —— 有人选/悬停英雄的
                    // 那一刻就抓那个英雄(每英雄一次, 之后走缓存), 而不是等别处顺手抓。
                    {
                        let picked: Vec<i64> = session
                            .get("myTeam")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .chain(
                                session
                                    .get("theirTeam")
                                    .and_then(Value::as_array)
                                    .into_iter()
                                    .flatten(),
                            )
                            .filter_map(|member| {
                                let locked =
                                    member.get("championId").and_then(Value::as_i64).unwrap_or(0);
                                let intent = member
                                    .get("championPickIntent")
                                    .and_then(Value::as_i64)
                                    .unwrap_or(0);
                                let id = if locked > 0 { locked } else { intent };
                                (id > 0).then_some(id)
                            })
                            .collect();
                        if !picked.is_empty() {
                            ensure_opgg_sections(&state, &picked).await;
                        }
                    }
                    // 用刚抓到的数据重建表格(否则会晚一个 tick 才显示)
                    let sections_map = {
                        let s = state.lock().unwrap();
                        s.opgg_sections_cache.clone()
                    };
                    match advisor::build_champ_select_table(
                        &session,
                        &champions_map,
                        &static_names,
                        &ranks,
                        &sections_map,
                    ) {
                        Ok((built, roster)) => {
                            // 缓存档案供开局后合并; 表本身照旧显示
                            if let Ok(mut s) = state.lock() {
                                s.match_roster = roster;
                            }
                            if !built.rows.is_empty() {
                                table = Some(built);
                            }
                        }
                        _ => {
                            if text.is_empty() {
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
                }
            }
        }

        // 变化检测: 表格用"内容签名"比较, 免得每 2.5s 都重建模型
        let signature = match &table {
            Some(t) => {
                let mut sig = format!("T|{}|{}", t.summary, t.sub_lines.join("/"));
                for row in &t.rows {
                    sig.push('|');
                    if !row.section.is_empty() {
                        sig.push_str(&row.section);
                    } else {
                        sig.push_str(&row.cells.join(","));
                        sig.push(if row.mine { '*' } else { ' ' });
                    }
                }
                sig.push('|');
                sig.push_str(&t.notes.join(";"));
                sig.push('|');
                sig.push_str(&t.footnote);
                sig
            }
            None => format!("X|{text}"),
        };

        if signature != last_text {
            last_text = signature;
            // 有新内容 → 输出区切到对局页(手动静默期内不抢)
            activate_output_tab(&weak, &state, UI_TAB_MATCH);
            let weak = weak.clone();
            let state_for_table = state.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak.upgrade() {
                    win.set_live_match_text(SharedString::from(text));
                    match table {
                        Some(mut table) => {
                            // 弹性列(装备/场次)按窗口实际宽度折算, 否则宽度 0 会整列消失
                            let content_width = table_content_width(&win);
                            resolve_flex_columns(&mut table.columns, content_width);

                            // 表格是自绘的, 复制按钮用这份纯文本(必须在搬走字段之前算)
                            let table_text = advisor::render_table_text(&table);
                            if let Ok(mut s) = state_for_table.lock() {
                                s.live_table_text = table_text;
                            }

                            win.set_table_summary(SharedString::from(table.summary));
                            win.set_table_sub_lines(slint::ModelRc::new(slint::VecModel::from(
                                table
                                    .sub_lines
                                    .into_iter()
                                    .map(SharedString::from)
                                    .collect::<Vec<_>>(),
                            )));
                            win.set_table_footnote(SharedString::from(table.footnote));
                            win.set_table_notes(slint::ModelRc::new(slint::VecModel::from(
                                table
                                    .notes
                                    .into_iter()
                                    .map(SharedString::from)
                                    .collect::<Vec<_>>(),
                            )));
                            win.set_table_columns(slint::ModelRc::new(slint::VecModel::from(
                                table
                                    .columns
                                    .into_iter()
                                    .map(|col| TableColumn {
                                        title: SharedString::from(col.title),
                                        width: col.width as f32,
                                        emphasis: col.emphasis,
                                    })
                                    .collect::<Vec<_>>(),
                            )));
                            win.set_table_rows(slint::ModelRc::new(slint::VecModel::from(
                                table
                                    .rows
                                    .into_iter()
                                    .map(|row| TableRow {
                                        section: SharedString::from(row.section),
                                        cells: slint::ModelRc::new(slint::VecModel::from(
                                            row.cells
                                                .into_iter()
                                                .map(SharedString::from)
                                                .collect::<Vec<_>>(),
                                        )),
                                        mine_team: row.mine_team,
                                        mine: row.mine,
                                        opponent: row.opponent,
                                    })
                                    .collect::<Vec<_>>(),
                            )));
                        }
                        None => {
                            // 没有表格(等待数据): 清空, 让 UI 显示文字面板
                            win.set_table_rows(slint::ModelRc::new(slint::VecModel::from(
                                Vec::<TableRow>::new(),
                            )));
                        }
                    }
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

        // ---- 符文对比卡(用户 2026-10-04 诉求): 我方 vs 对方符文特性 + 扬长避短 ----
        // 对手符文只有对局内 Live Client Data 才公开(基石 + 主/副系), 因此这里算;
        // 选人阶段此卡为空, 由 counter 卡承担"对位建议"。
        if let Some((header, body)) = compute_rune_compare(&snapshot, &static_names) {
            {
                let mut s = state.lock().unwrap();
                s.rune_compare = Some((header.clone(), body.clone()));
            }
            let weak_compare = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(win) = weak_compare.upgrade() {
                    win.set_rune_compare_header(SharedString::from(header));
                    win.set_rune_compare_body(SharedString::from(body));
                }
            });
        }

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
) {
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
        } else if phase == MatchPhase::Idle && old_phase == MatchPhase::Ended {
            // 对局彻底结束、回到大厅: 这时候才清掉选人档案(段位/OP.GG 胜率)。
            // 不能在选人会话 Delete 时清 —— 那时正在进游戏, 清了对局里就什么都看不到。
            let mut s = state.lock().unwrap();
            if !s.match_roster.is_empty() {
                info!("match roster cleared (game over, back to idle)");
                s.match_roster.clear();
            }
            s.match_phase = phase.clone();
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

        // 悬浮迷你窗已按用户要求移除(2026-10-04): 对局信息统一在主窗「对局数据」
        // Tab 输出, 不再有任何置顶小窗; 目标提醒仍走 TTS。
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
            warn!("advice loop: DeepSeek API Key 未配置, 跳过本轮");
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
                info!("advice ready ({} chars)", advice.chars().count());
                let tts_text = advice.clone();
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
                let msg = format!("大师请求失败: {err}");
                warn!("{msg}");
                append_coach_message(&sources_weak, &state, "assistant", &msg);
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
    manual_target: Option<i64>,
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
    // 对位英雄: 手动点选优先(腾讯客户端敌方分路可能藏字段, 自动识别失效
    // 时用户在 Counter 卡点"我打谁", 就成为对位目标)。
    let opponent_id = if let Some(target) = manual_target {
        target
    } else {
        let Some(opponent) = snapshot.lane_opponent() else {
            return (None, "对位英雄未识别(下方点选你的对位即可)");
        };
        let id = opponent.effective_champion();
        if id == 0 {
            return (None, "等对面选出你的对位英雄");
        }
        id
    };
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

/// Counter 卡手动点选候选: 敌方阵容本局已选/悬停的英雄 (id, 中文名) 对。
/// 与 roster 同源, 每次 session 变化都重算(悬停也算入, 给先做功课的空间)。
fn counter_candidates(session_data: Option<&Value>, s: &AppState) -> (Vec<i32>, Vec<String>) {
    let Some(session) = session_data else {
        return (Vec::new(), Vec::new());
    };
    let Ok(snap) = lcu::match_context::ChampSelectSnapshot::from_session(session) else {
        return (Vec::new(), Vec::new());
    };
    let mut ids: Vec<i32> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for member in &snap.their_team {
        let cid = member.effective_champion();
        if cid > 0 && !ids.contains(&(cid as i32)) {
            ids.push(cid as i32);
            names.push(zh_champion_name(s, cid).unwrap_or_else(|| format!("#{cid}")));
        }
    }
    (ids, names)
}

/// 冠军 id → 中文名(静态名表优先, 兜底 DDragon 英文名)。
fn zh_champion_name(s: &AppState, champion_id: i64) -> Option<String> {    let key = champion_id.to_string();
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
    manual_target: Option<i64>,
) -> Option<(String, String)> {
    let session = session_data?;
    let snapshot = lcu::match_context::ChampSelectSnapshot::from_session(session).ok()?;
    // 对位来源: 手动点选优先(腾讯客户端敌方分路恒为空, 自动匹配不可用)
    let opp_id = match manual_target {
        Some(target) => target,
        None => {
            let opponent = snapshot.lane_opponent()?;
            let id = opponent.effective_champion();
            if id == 0 {
                return None;
            }
            id
        }
    };
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
    manual_target: Option<i64>,
) -> Option<(String, String)> {
    let session = session_data?;
    let snapshot = lcu::match_context::ChampSelectSnapshot::from_session(session).ok()?;
    let local = snapshot.local_member()?;
    let local_id = local.effective_champion();
    // 对位来源: 手动点选优先(与 counter 卡同一把钥匙)
    let opp_id = match manual_target {
        Some(target) => target,
        None => {
            let opponent = snapshot.lane_opponent()?;
            let id = opponent.effective_champion();
            if id == 0 {
                return None;
            }
            id
        }
    };
    if local_id == 0 {
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

/// 符文对比卡: 我方(完整页) vs 对方(基石+主副系) → 特性描述 + 扬长避短提醒。
///
/// 数据来源: 对局内 Live Client Data。我方用 active 玩家的 fullRunes(含属性碎片),
/// 对方只有 keystone + 主/副系(游戏只公开这些), 所以按"风格配对"给建议。
fn compute_rune_compare(
    snapshot: &lcu::match_context::LiveSnapshot,
    names: &lcu::web::StaticNames,
) -> Option<(String, String)> {
    let me = snapshot.local_player()?;
    let opponent = snapshot.lane_opponent()?;
    if me.champion_name.is_empty() || opponent.champion_name.is_empty() {
        return None;
    }

    let my_key = names.rune(me.keystone_id, &me.keystone_name);
    let my_primary = names.rune(me.primary_tree_id, &me.primary_tree_name);
    let my_secondary = names.rune(me.secondary_tree_id, &me.secondary_tree_name);
    let opp_key = names.rune(opponent.keystone_id, &opponent.keystone_name);
    let opp_primary = names.rune(opponent.primary_tree_id, &opponent.primary_tree_name);

    let my_style = lcu::runetraits::style_of(me.keystone_id, me.primary_tree_id);
    let opp_style = lcu::runetraits::style_of(opponent.keystone_id, opponent.primary_tree_id);

    // 属性碎片: 只有自己的可见(fullRunes 里 5000 段就是属性碎片)
    let stat_zh: Vec<String> = snapshot
        .active
        .as_ref()
        .map(|a| {
            a.full_runes
                .iter()
                .filter(|(id, _)| (5000..=5099).contains(id))
                .map(|(id, fallback)| names.rune(*id, fallback))
                .collect()
        })
        .unwrap_or_default();

    let header = format!(
        "符文对比 · 我({my_key}) vs 对位 {opp_key}({})",
        lcu::runetraits::style_label(opp_style)
    );

    let mut body = String::new();
    body.push_str(&format!(
        "我方: {} · {}({}) · 主 {} / 副 {}",
        lcu::runetraits::style_label(my_style),
        my_key,
        lcu::runetraits::keystone_trait(me.keystone_id),
        my_primary,
        my_secondary
    ));
    if !stat_zh.is_empty() {
        body.push_str(&format!(" · 属性 {}", stat_zh.join("/")));
    }
    body.push('\n');
    body.push_str(&format!(
        "对方: {} · {}({}) · 主 {} / 副 {}",
        lcu::runetraits::style_label(opp_style),
        opp_key,
        lcu::runetraits::keystone_trait(opponent.keystone_id),
        opp_primary,
        lcu::runetraits::tree_trait(opponent.secondary_tree_id)
    ));
    body.push('\n');
    for line in lcu::runetraits::advice(my_style, opp_style) {
        body.push_str("提醒: ");
        body.push_str(line);
        body.push('\n');
    }
    Some((header, body.trim_end().to_string()))
}

async fn apply_counter_rune_plan(
    runes_weak: Weak<SourcesWindow>,
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
    runes_weak: Weak<SourcesWindow>,
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

async fn auto_write_builds(lol_dir: String, is_tencent: bool, champion_id: i64) {
    info!("自动写入推荐出装(英雄 {champion_id})…");

    let source = DEFAULT_SOURCE_VALUE.to_string();
    let result = lcu::builds::apply_builds_from_id(&lol_dir, &source, champion_id, is_tencent).await;
    if result.is_ok() {
        info!("已自动写入推荐出装(英雄 {champion_id})");
    } else {
        warn!("自动写入出装失败(英雄 {champion_id})");
    }
}

// ---------------------------------------------------------------------------
//  Show champion runes: fetch avatar, populate source list, fetch runes
// ---------------------------------------------------------------------------

async fn show_champion_runes(
    runes_weak: Weak<SourcesWindow>,
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

    // 选人开始 → 自动把主窗内容区切到「符文」Tab(替代原来的"自动弹符文窗")。
    // 不抢焦点、不新开窗口, 用户手点过 Tab 的静默期内也不抢(见 activate_output_tab)。
    activate_output_tab(&runes_weak, &state, UI_TAB_RUNES);

    // Update the runes panel with champion info
    let weak = runes_weak.clone();
    let champ_name = SharedString::from(&champion_name);

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
    /// 工作区居中
    Center,
}

/// 窗口外框(标题栏+边框)相对客户区的额外物理像素。
///
/// 主窗从 2026-10-05 起是**无边框**(no-frame, 自绘标题栏), 外框 = 客户区, 所以这里是 0;
/// 有边框时的实测值(100% 缩放: 高 37 / 宽 15)保留在注释里备查 ——
/// 当年不算它的话, 客户区"刚好放得下"时外框仍会顶出工作区(2026-10-03)。
const FRAME_H: i32 = 0;
const FRAME_W: i32 = 0;

/// 选目标显示器并把窗口**完整**放进工作区(尺寸超了就按比例缩, 位置再纠偏)。
///
/// 2026-10-04 事故: Tokens 里的窗口尺寸是**逻辑像素**, 而工作区与窗口 size()
/// 是物理像素。150% 缩放下 840×1360 逻辑 → 1245×2015 物理, 比 1440 高的屏幕还大
/// 1.4 倍 —— 用户看到的就是"什么都看不见"。所以这里统一在逻辑坐标系里夹紧:
///   scale = window.scale_factor(); 逻辑工作区 = 物理工作区 / scale
fn place_window_in_work_area(window: &slint::Window, m: &monitors::Monitor, anchor: PinAnchor) {
    let (l, t, r, b) = m.work_rect;
    let scale = window.scale_factor() as f64; // 每显示器 DPI, 1.0 / 1.25 / 1.5 ...
    let frame_w = FRAME_W as f64;
    let frame_h = FRAME_H as f64;
    // 逻辑坐标系下的可用区(扣掉外框与一点边距)
    let avail_w = (((r - l) as f64 / scale) - frame_w - 16.0).max(320.0);
    let avail_h = (((b - t) as f64 / scale) - frame_h - 16.0).max(240.0);

    let size = window.size(); // 物理像素
    let (phys_w, phys_h) = (size.width as f64, size.height as f64);
    if phys_w <= 1.0 || phys_h <= 1.0 {
        // 尺寸还没定: 摆了也是错的位置, 交给调用方的延迟补摆
        return;
    }
    let mut w_log = phys_w / scale;
    let mut h_log = phys_h / scale;

    if w_log > avail_w || h_log > avail_h {
        let k = f64::min(avail_w / w_log, avail_h / h_log);
        w_log = (w_log * k).max(320.0);
        h_log = (h_log * k).max(240.0);
        warn!(
            "window {:.0}x{:.0} logical (scale {scale}) exceeds logical work area {:.0}x{:.0}; resized to {:.0}x{:.0}",
            phys_w / scale, phys_h / scale, avail_w, avail_h, w_log, h_log
        );
        window.set_size(slint::PhysicalSize::new(
            (w_log * scale).round() as u32,
            (h_log * scale).round() as u32,
        ));
    }

    // 位置: 用缩放后的物理尺寸, 并让外框完整落在工作区内
    let phys_w = (w_log * scale).round() as i32;
    let phys_h = (h_log * scale).round() as i32;
    let max_x = (r - phys_w - FRAME_W).max(l);
    let max_y = (b - phys_h - FRAME_H).max(t);
    let (x, y) = match anchor {
        PinAnchor::Center => (
            (l + r).div_euclid(2) - phys_w.div_euclid(2),
            (t + b).div_euclid(2) - phys_h.div_euclid(2),
        ),
    };
    window.set_position(slint::WindowPosition::Physical(slint::PhysicalPosition::new(
        x.clamp(l, max_x),
        y.clamp(t, max_y),
    )));
}

/// 将窗口放进 pinned_monitor 指定显示器的工作区(未固定时用主显示器)。
fn pin_window_to_monitor(window: &slint::Window, state: &SharedState, anchor: PinAnchor) {
    let (idx, mons) = {
        let Ok(s) = state.lock() else {
            return;
        };
        (s.pinned_monitor, s.monitors.clone())
    };
    let target = if idx >= 0 {
        mons.get(idx as usize)
    } else {
        // 未固定或索引越界 → 主显示器(找不到 primary 就用第一个)
        mons.iter().find(|m| m.is_primary).or_else(|| mons.first())
    };
    if let Some(m) = target {
        place_window_in_work_area(window, m, anchor);
    }
}

// ---------------------------------------------------------------------------
//  Fetch runes for a champion from a source and display them
// ---------------------------------------------------------------------------

/// 递归安全包装: 8s 重试的 tokio::spawn 要求 Future: Send + 'static,
/// 直接自递归会造成环; 用 BoxFuture 把返回类型定下来。
fn fetch_and_show_runes(
    runes_weak: Weak<SourcesWindow>,
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
    runes_weak: Weak<SourcesWindow>,
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
