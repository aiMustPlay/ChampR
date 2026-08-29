use anyhow::{Context, bail};

#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub rate: i32,
    pub volume: i32,
    pub voice: Option<String>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            rate: 0,
            volume: 100,
            voice: Some("zh-CN-XiaoxiaoNeural".to_string()),
        }
    }
}

impl TtsConfig {
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            rate: std::env::var("CHAMPR_TTS_RATE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default.rate),
            volume: std::env::var("CHAMPR_TTS_VOLUME")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default.volume),
            voice: std::env::var("CHAMPR_TTS_VOICE")
                .ok()
                .filter(|value| !value.is_empty())
                .or(default.voice),
        }
    }
}

#[cfg(target_os = "windows")]
fn edge_tts_path() -> Option<std::path::PathBuf> {
    let candidates = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];

    candidates
        .iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.exists())
}

#[cfg(target_os = "windows")]
fn speak_with_edge(text: &str, config: &TtsConfig) -> anyhow::Result<()> {
    use base64::{engine::general_purpose, Engine as _};
    use std::{fs, path::PathBuf, process::Command};

    let edge = edge_tts_path().context("Microsoft Edge not found")?;
    let encoded = general_purpose::STANDARD_NO_PAD.encode(text.as_bytes());
    let voice_name = config.voice.as_deref().unwrap_or("Xiaoxiao");
    let html = format!(
        r#"<html><body><script>
const text = decodeURIComponent(escape(atob('{encoded}')));
const utter = new SpeechSynthesisUtterance(text);
utter.rate = {rate};
utter.volume = {volume};
function speak() {{
    const voices = speechSynthesis.getVoices();
    const voice = voices.find(v => v.name.includes('{voice_name}')) || voices[0];
    if (voice) utter.voice = voice;
    utter.onend = () => window.close();
    speechSynthesis.speak(utter);
}}
if (speechSynthesis.getVoices().length) speak();
else speechSynthesis.onvoiceschanged = speak;
</script></body></html>"#,
        rate = config.rate,
        volume = config.volume as f64 / 100.0,
    );

    let file_path: PathBuf = std::env::temp_dir().join(format!("champr-tts-{}.html", nanoid::nanoid!()));
    fs::write(&file_path, html).context("failed to write Edge TTS page")?;
    let url = format!("file:///{}", file_path.display());

    Command::new(edge)
        .args(["--app", &url, "--no-first-run", "--disable-features=msEdgeFirstRunExperience"])
        .spawn()
        .context("failed to launch Edge TTS")?;

    Ok(())
}

#[cfg(target_os = "windows")]
fn speak_with_node_sidecar(text: &str, config: &TtsConfig) -> anyhow::Result<()> {
    use std::{path::PathBuf, process::Command};

    let cli_path = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("packages")
        .join("audio")
        .join("cli.js");
    if !cli_path.exists() {
        bail!("Node.js TTS sidecar not found");
    }

    let voice = config.voice.as_deref().unwrap_or("zh-CN-XiaoxiaoNeural");
    Command::new("node")
        .arg(&cli_path)
        .args([
            "--text",
            text,
            "--voice",
            voice,
            "--volume",
            &config.volume.to_string(),
        ])
        .spawn()
        .context("failed to spawn Node.js TTS sidecar")?;

    Ok(())
}

#[cfg(target_os = "windows")]
pub fn speak_windows_tts(text: &str) -> anyhow::Result<()> {
    speak_windows_tts_with_config(text, &TtsConfig::default())
}

#[cfg(target_os = "windows")]
pub fn speak_windows_tts_with_config(
    text: &str,
    config: &TtsConfig,
) -> anyhow::Result<()> {
    if speak_with_node_sidecar(text, config).is_ok() {
        return Ok(());
    }

    if speak_with_edge(text, config).is_ok() {
        return Ok(());
    }

    use base64::{engine::general_purpose, Engine as _};
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let encoded = general_purpose::STANDARD_NO_PAD.encode(text.as_bytes());
    let voice = config
        .voice
        .as_deref()
        .map(|voice| format!("'{}'", voice.replace('\'', "''")))
        .unwrap_or_else(|| "$null".to_string());
    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$text = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String('{encoded}'))
Add-Type -AssemblyName System.Speech
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$voices = $synth.GetInstalledVoices()
if ($null -eq $voices -or $voices.Count -eq 0) {{
    Write-Error '未检测到 Windows 语音包，请在 系统设置 > 时间和语言 > 语音 中安装语音包。'
    exit 1
}}
$synth.Rate = {rate}
$synth.Volume = {volume}
if ({voice} -ne $null) {{
    $selected = $voices | Where-Object {{ $_.VoiceInfo.Name -eq {voice} }} | Select-Object -First 1
    if ($null -eq $selected) {{
        Write-Error ('未找到语音 {voice}，请先在 Windows 讲述人设置中安装该语音包。')
        exit 1
    }}
    $synth.SelectVoice($selected.VoiceInfo.Name) | Out-Null
}}
$synth.Speak($text)
$synth.Dispose()
"#,
        rate = config.rate,
        volume = config.volume,
        voice = voice,
    );

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(0x08000000)
        .output()
        .context("failed to start Windows PowerShell for TTS")?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Windows TTS failed: {stderr}")
    }
}

#[cfg(not(target_os = "windows"))]
pub fn speak_windows_tts(_text: &str) -> anyhow::Result<()> {
    bail!("Windows TTS is only supported on Windows")
}

#[cfg(not(target_os = "windows"))]
pub fn speak_windows_tts_with_config(
    _text: &str,
    _config: &TtsConfig,
) -> anyhow::Result<()> {
    bail!("Windows TTS is only supported on Windows")
}
