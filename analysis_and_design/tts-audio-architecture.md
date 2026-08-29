# TTS 音频架构

## 目标

- 使用 Microsoft Edge 神经语音 `zh-CN-XiaoxiaoNeural`
- 不弹出浏览器窗口
- 不依赖付费 TTS API
- 播放过程完全静默

## 链路

```text
Rust tts.rs
  -> node packages/audio/cli.js
  -> msedge-tts
  -> Edge Read Aloud 神经语音
  -> MP3 Buffer
  -> WindowsAudioPlayer
  -> winmm.dll MCI
  -> 默认音频设备
```

## 文件

| 文件 | 职责 |
| --- | --- |
| `crates/lcu/src/tts.rs` | Rust TTS 调度 |
| `packages/audio/cli.js` | Node 侧车入口 |
| `packages/audio/edge-tts.js` | Edge 神经语音合成 |
| `packages/audio/windows-audio-player.js` | MCI 静默播放 |
| `packages/audio/audio-player.js` | 跨平台播放器管理 |
| `packages/audio/tts-player.js` | 通用 HTTP TTS endpoint 播放 |

## 依赖安装

```powershell
corepack pnpm --dir packages/audio install
```

## 测试

```powershell
node packages/audio/cli.js --text="你好，世界" --voice="zh-CN-XiaoxiaoNeural"
```

预期输出：

```json
{"success":true,"text":"你好，世界","audioSize":13392,"engine":"edge-neural"}
```
