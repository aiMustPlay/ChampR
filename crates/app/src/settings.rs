use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct Settings {
    /// Source identifiers the user has checked (e.g. ["op.gg", "u.gg"])
    #[serde(default)]
    pub selected_sources: Vec<String>,
    /// Which source to show runes from in the overlay window
    #[serde(default)]
    pub rune_source: String,
    #[serde(default)]
    pub tts_rate: i32,
    #[serde(default = "default_tts_volume")]
    pub tts_volume: i32,
    #[serde(default = "default_tts_voice")]
    pub tts_voice: String,
    #[serde(default = "default_lol_launcher_path")]
    pub lol_launcher_path: String,
    #[serde(default)]
    pub deepseek_api_key: String,
    #[serde(default = "default_deepseek_base_url")]
    pub deepseek_base_url: String,
    #[serde(default = "default_deepseek_model")]
    pub deepseek_model: String,
    #[serde(default)]
    pub deepseek_thinking: bool,
    #[serde(default)]
    pub deepseek_stream: bool,
    #[serde(default = "default_deepseek_reasoning_effort")]
    pub deepseek_reasoning_effort: String,
    #[serde(default = "default_ai_provider")]
    pub ai_provider: String,
    #[serde(default)]
    pub deepseek_web_risk_accepted: bool,
    /// Automatically apply the best OP.GG rune page for the locked position.
    #[serde(default)]
    pub auto_apply_rune: bool,
    /// Automatically write recommended item builds on champion lock-in.
    #[serde(default)]
    pub auto_apply_builds: bool,
    /// 排队就绪时自动点"接受对局"。默认开(用户 2026-09-30 拍板),
    /// 可以在设置里关。
    #[serde(default = "default_true")]
    pub auto_accept_match: bool,
    /// 自动禁人(用户 2026-10-05 要求"默认禁人")。默认**开**, 但名单为空时不会瞎禁
    /// (什么都不做), 填了名单才真正生效 —— 即"开了也不会做错事"。
    #[serde(default = "default_true")]
    pub auto_ban: bool,
    /// 自动选人(**不做预选**, 倒计时到阈值一次性锁定; 预选阶段 2026-10-05 被用户拍板删除)。
    /// 默认**开**(用户要求"默认选人"): 锁定目标优先沿用你自己悬停的英雄,
    /// 没悬停时才用 OP.GG 该分路最高胜率。
    #[serde(default = "default_true")]
    pub auto_pick: bool,
    /// 优先禁用名单(英雄 id, 按顺序取第一个还没被禁的)。
    #[serde(default)]
    pub auto_ban_list: Vec<i64>,
    /// 选人自动锁定阈值(秒): 剩余时间 <= 该值就提交锁定; 0 = 轮到我就直接锁定。
    /// 默认 3 秒 = 倒计时最后 3 秒才出手, 之前绝不碰客户端, 留了整段的反悔窗口。
    #[serde(default = "default_auto_pick_lock_seconds")]
    pub auto_pick_lock_seconds: f64,
    /// Objective reminder tier: 0 = all, 1 = key events only, 2 = quiet (log only).
    #[serde(default)]
    pub reminder_tier: i32,
    /// 固定窗口出现的显示器索引(-1 = 不固定, 跟随系统等默认行为)。
    /// 手动配置, 不做自动跟随游戏屏——稳定优先。
    #[serde(default = "default_pinned_monitor")]
    pub pinned_monitor: i32,
    #[serde(default = "default_lmstudio_base_url")]
    pub lmstudio_base_url: String,
    #[serde(default = "default_lmstudio_model")]
    pub lmstudio_model: String,
    #[serde(default)]
    pub lmstudio_api_key: String,
    /// 任意 OpenAI 兼容端点(本地推理站/第三方网关; Claude/Anthropic 不走这里)。
    #[serde(default)]
    pub openai_base_url: String,
    #[serde(default)]
    pub openai_model: String,
    #[serde(default)]
    pub openai_api_key: String,
    /// LLM 通道: "maohou"(经 houmao 引擎子进程, 默认) / "direct"(直连 reqwest)。
    #[serde(default = "default_ai_backend")]
    pub ai_backend: String,
    /// 显式指定 maohou 二进制路径(留空=自动: MAOHOU_BIN → 兄弟仓 → PATH)。
    #[serde(default)]
    pub maohou_bin: String,
}

fn default_ai_backend() -> String {
    "maohou".to_string()
}

fn default_auto_pick_lock_seconds() -> f64 {
    // 倒计时最后 3 秒才一次性锁定: 之前过程全是用户的, 工具不插手
    3.0
}

fn default_true() -> bool {
    true
}

fn default_tts_volume() -> i32 {
    100
}

fn default_tts_voice() -> String {
    "zh-CN-XiaoxiaoNeural".to_string()
}

fn default_lol_launcher_path() -> String {
    r"C:\WeGameApps\英雄联盟（含经典模式）\WeGameLauncher\launcher.exe".to_string()
}

fn default_deepseek_base_url() -> String {
    "https://api.deepseek.com".to_string()
}

fn default_deepseek_model() -> String {
    "deepseek-v4-flash".to_string()
}

fn default_deepseek_reasoning_effort() -> String {
    "high".to_string()
}

fn default_ai_provider() -> String {
    "deepseek".to_string()
}

fn default_lmstudio_base_url() -> String {
    "http://localhost:1234/v1".to_string()
}

fn default_lmstudio_model() -> String {
    "local-model".to_string()
}

fn default_pinned_monitor() -> i32 {
    -1
}

fn settings_path() -> PathBuf {
    let mut dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.push("champr");
    dir.push("settings.toml");
    dir
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            selected_sources: Vec::new(),
            rune_source: String::new(),
            tts_rate: 0,
            tts_volume: default_tts_volume(),
            tts_voice: default_tts_voice(),
            lol_launcher_path: default_lol_launcher_path(),
            deepseek_api_key: String::new(),
            deepseek_base_url: default_deepseek_base_url(),
            deepseek_model: default_deepseek_model(),
            deepseek_thinking: false,
            deepseek_stream: false,
            deepseek_reasoning_effort: default_deepseek_reasoning_effort(),
            ai_provider: default_ai_provider(),
            deepseek_web_risk_accepted: false,
            auto_apply_rune: false,
            auto_apply_builds: false,
            auto_accept_match: true,
            auto_ban: default_true(),
            auto_pick: default_true(),
            auto_ban_list: Vec::new(),
            auto_pick_lock_seconds: default_auto_pick_lock_seconds(),
            reminder_tier: 0,
            pinned_monitor: default_pinned_monitor(),
            lmstudio_base_url: default_lmstudio_base_url(),
            lmstudio_model: default_lmstudio_model(),
            lmstudio_api_key: String::new(),
            openai_base_url: String::new(),
            openai_model: String::new(),
            openai_api_key: String::new(),
            ai_backend: default_ai_backend(),
            maohou_bin: String::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let mut settings: Self = toml::from_str(&contents).unwrap_or_default();
                settings.normalize_defaults();
                settings
            }
            Err(_) => Self::default(),
        }
    }

    fn normalize_defaults(&mut self) {
        if self.tts_voice.is_empty() {
            self.tts_voice = default_tts_voice();
        }
        if self.lol_launcher_path.is_empty() {
            self.lol_launcher_path = default_lol_launcher_path();
        }
        if self.deepseek_base_url.is_empty() {
            self.deepseek_base_url = default_deepseek_base_url();
        }
        if self.deepseek_model.is_empty() {
            self.deepseek_model = default_deepseek_model();
        }
        if !matches!(
            self.ai_provider.as_str(),
            "deepseek" | "deepseek_web" | "lmstudio" | "openai"
        ) {
            self.ai_provider = default_ai_provider();
        }
        if !matches!(self.ai_backend.as_str(), "maohou" | "direct") {
            self.ai_backend = default_ai_backend();
        }
        self.reminder_tier = self.reminder_tier.clamp(0, 2);
    }

    pub fn save(&self) {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(contents) = toml::to_string_pretty(self) {
            let _ = fs::write(&path, contents);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_include_working_ai_endpoints() {
        let settings = Settings::default();
        assert_eq!(settings.deepseek_base_url, "https://api.deepseek.com");
        assert_eq!(settings.deepseek_model, "deepseek-v4-flash");
        assert_eq!(settings.ai_provider, "deepseek");
        assert!(!settings.deepseek_web_risk_accepted);
    }
}
