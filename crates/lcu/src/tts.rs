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
            voice: Some("Microsoft Huihui".to_string()),
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
pub fn speak_windows_tts(text: &str) -> anyhow::Result<()> {
    speak_windows_tts_with_config(text, &TtsConfig::default())
}

#[cfg(target_os = "windows")]
pub fn speak_windows_tts_with_config(
    text: &str,
    config: &TtsConfig,
) -> anyhow::Result<()> {
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
    $synth.SelectVoice({voice}) | Out-Null
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
