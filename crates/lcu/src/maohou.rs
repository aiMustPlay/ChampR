//! maohou(houmao 引擎)子进程适配层。
//!
//! ChampR 不再自己持有 OpenAI 兼容 HTTP 客户端直连远端; 改把 LLM 调用交给
//! `maohou exec` 一次性命令——两个仓共用同一个引擎的实现(协议方言、base_url
//! 归一化、thinking 剥离、安全栈)只维护一份。
//!
//! 设计边界(与 houmao-mac/engine/docs/DESIGN.md §2 配置模型对齐):
//! - 一次性调用 = 一进程一调用, LLM 参数用启动 flag / 子进程环境注入,
//!   永远不写 ChampR 的任何配置文件。
//! - 引擎缺失(未安装/未构建)由调用方决定降级(直连 reqwest), 本层不隐藏。
//! - 引擎调用失败原样上抛, 不做第二次请求(避免同一 prompt 计费两次)。
//!
//! 当前差集(引擎 OpenAI 方言暂不转发):
//! temperature / max_tokens / DEEPSEEK_THINKING / DEEPSEEK_REASONING_EFFORT /
//! streaming。参数有需要的先在引擎侧加(它是协议职责所在), 别再绕回去写直连。

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use log::warn;

use crate::deepseek::{self, ChatMessage};

/// 引擎调用超时: maohou 是 std 阻塞 + 无内部 LLM 超时的进程, 上层必须给保险丝。
const ENGINE_TIMEOUT: Duration = Duration::from_secs(150);
/// 子进程注入 key 用的环境变量名(避免 --api-key 明文上命令行/任务管理器)。
const KEY_ENV_NAME: &str = "CHAMPR_ENGINE_KEY";

/// 引擎连接参数。`api_key_env` 优先于 `api_key`(引擎 --api-key-env 语义);
/// 两个都没有 = 目标不需要鉴权(本地 lmstudio / llama-server)。
#[derive(Debug, Clone, Default)]
pub struct MaohouTarget {
    pub base_url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
}

impl MaohouTarget {
    /// DeepSeek 通道: key 来自 ChampR 设置页(不一定是宿主环境变量),
    /// 统一走子进程 env 注入通道 —— 引擎进程内表现为 --api-key-env CHAMPR_ENGINE_KEY。
    pub fn for_deepseek(cfg: &deepseek::DeepSeekConfig) -> Self {
        Self {
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            api_key_env: None,
            api_key: (!cfg.api_key.is_empty()).then(|| cfg.api_key.clone()),
        }
    }

    pub fn for_lmstudio(base_url: &str, model: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key_env: None,
            api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
        }
    }
}

/// 定位 maohou 二进制:
/// 1. `MAOHOU_BIN` 环境变量(显式, 最高优先)
/// 2. 当前进程可执行文件的祖先目录里找 `houmao-mac/engine/target/release/maohou(.exe)`
///    (开发机同 checkout 惯例, 不硬编码绝对路径)
/// 3. PATH 里的 `maohou`
pub fn locate_binary() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("MAOHOU_BIN") {
        let path = PathBuf::from(&explicit);
        if path.is_file() {
            return Some(path);
        }
        warn!("MAOHOU_BIN 指向不存在的文件: {explicit}");
    }

    let bin_name = if cfg!(windows) { "maohou.exe" } else { "maohou" };
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1) {
            // 仓库兄弟布局: <root>/ChampR/... exe → <root>/houmao-mac/engine/target/release/maohou
            let candidate = dir
                .join("houmao-mac")
                .join("engine")
                .join("target")
                .join("release")
                .join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // PATH 查找: 跑 --version 试探(成败即真伪, 不产生副作用)
    let probe = std::process::Command::new("maohou")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if matches!(probe, Ok(status) if status.success()) {
        return Some(PathBuf::from("maohou"));
    }
    None
}

/// 构造 `maohou exec` 参数(纯函数, 测试覆盖顺序与缺省)。
pub fn build_args(prompt: &str, target: &MaohouTarget) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        prompt.to_string(),
        "--no-tools".to_string(),
    ];
    if !target.base_url.is_empty() {
        args.push("--base-url".to_string());
        args.push(target.base_url.clone());
    }
    if !target.model.is_empty() {
        args.push("--model".to_string());
        args.push(target.model.clone());
    }
    if let Some(env_name) = &target.api_key_env {
        args.push("--api-key-env".to_string());
        args.push(env_name.clone());
    } else if target.api_key.is_some() {
        // key 走 child env 注入, 见 chat()
        args.push("--api-key-env".to_string());
        args.push(KEY_ENV_NAME.to_string());
    }
    args
}

/// system 消息与 user 消息合成 exec 单 prompt(引擎会自带一份 agent system)。
pub fn compose_prompt(messages: &[ChatMessage]) -> String {
    let systems: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .collect();
    let users: Vec<&str> = messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| m.content.as_str())
        .collect();
    let mut out = String::new();
    for sys in systems {
        out.push_str(sys);
        out.push_str("\n\n");
    }
    for (i, user) in users.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n---\n\n");
        }
        out.push_str(user);
    }
    out.trim().to_string()
}

/// 调用 maohou exec 并回收正文。失败原样 bail(不隐性双请求)。
pub async fn chat(bin: &std::path::Path, target: &MaohouTarget, messages: &[ChatMessage]) -> anyhow::Result<String> {
    let prompt = compose_prompt(messages);
    if prompt.is_empty() {
        bail!("maohou prompt 为空");
    }
    let args = build_args(&prompt, target);

    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let (Some(key), None) = (&target.api_key, &target.api_key_env) {
        cmd.env(KEY_ENV_NAME, key);
    }

    let child = cmd.spawn().context("maohou spawn 失败")?;
    let output = match tokio::time::timeout(ENGINE_TIMEOUT, child.wait_with_output()).await {
        Ok(res) => res.context("maohou wait 失败")?,
        Err(_) => bail!("maohou 调用超时({}s)", ENGINE_TIMEOUT.as_secs()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!(
            "maohou 退出码 {:?}: {}",
            output.status.code(),
            stderr.trim().lines().next().unwrap_or("(无 stderr)")
        );
    }
    let text = deepseek::normalize_plain_text(stdout.trim());
    if text.is_empty() {
        bail!("maohou 返回空内容: {}", stderr.trim().lines().next().unwrap_or(""));
    }
    if !deepseek::contains_cjk(&text) {
        bail!("LLM response must be in Chinese");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_order_and_env_channel() {
        let target = MaohouTarget {
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-v4-flash".into(),
            api_key_env: Some("DEEPSEEK_API_KEY".into()),
            api_key: None,
        };
        let args = build_args("测试", &target);
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "测试");
        assert!(args.contains(&"--no-tools".to_string()));
        let p = args.iter().position(|a| a == "--api-key-env").unwrap();
        assert_eq!(args[p + 1], "DEEPSEEK_API_KEY");
        assert!(!args.iter().any(|a| a == "--api-key"), "key 不应上命令行");
    }

    #[test]
    fn deepseek_settings_key_goes_through_child_env() {
        let cfg = deepseek::DeepSeekConfig {
            api_key: "sekret".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-v4-flash".into(),
            thinking_enabled: false,
            reasoning_effort: "high".into(),
            stream_enabled: false,
        };
        let target = MaohouTarget::for_deepseek(&cfg);
        assert_eq!(target.api_key.as_deref(), Some("sekret"));
        let args = build_args("p", &target);
        let p = args.iter().position(|a| a == "--api-key-env").unwrap();
        assert_eq!(args[p + 1], KEY_ENV_NAME);
        assert!(!args.iter().any(|a| a == "sekret"), "key 值不进命令行");
    }

    #[test]
    fn inline_key_goes_through_child_env() {
        let target = MaohouTarget::for_lmstudio("http://localhost:1234/v1", "m", "sekret");
        let args = build_args("p", &target);
        let p = args.iter().position(|a| a == "--api-key-env").unwrap();
        assert_eq!(args[p + 1], KEY_ENV_NAME);
        assert!(!args.iter().any(|a| a == "sekret"));
    }

    #[test]
    fn no_auth_target_omits_key_flags() {
        let target = MaohouTarget::for_lmstudio("http://localhost:1234", "m", "");
        let args = build_args("p", &target);
        assert!(!args.iter().any(|a| a.starts_with("--api-key")));
    }

    #[test]
    fn prompt_composition_keeps_systems_before_users() {
        let messages = vec![
            ChatMessage::system("你是助手"),
            ChatMessage::user("第一句"),
            ChatMessage::system("补充规则"),
            ChatMessage::user("第二句"),
        ];
        let prompt = compose_prompt(&messages);
        let sys_first = prompt.find("你是助手").unwrap();
        let sys_second = prompt.find("补充规则").unwrap();
        let u1 = prompt.find("第一句").unwrap();
        let u2 = prompt.find("第二句").unwrap();
        assert!(sys_first < u1 && sys_second < u1 && u1 < u2);
        assert!(prompt.contains("---"));
    }

    #[test]
    fn empty_prompt_is_empty() {
        assert!(compose_prompt(&[]).is_empty());
    }
}
