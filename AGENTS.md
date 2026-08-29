# ChampR Agent 开发参考

## 项目定位

ChampR 是一个 Windows 英雄联盟助手：

- 连接 League Client / LCU
- 读取英雄选择和对局状态
- 应用 OP.GG 符文和装备
- 通过 DeepSeek 生成中文对局建议
- 使用 Windows TTS 语音播报

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| `crates/app` | Rust + Slint 桌面客户端 |
| `crates/lcu` | LCU、Live Client Data、DeepSeek、TTS 等核心库 |
| `crates/server` | 本地 SQLite + Axum 后端 |
| `packages/opgg` | OP.GG Playwright 爬虫 |
| `packages/audio` | Node.js TTS sidecar，使用 msedge-tts 与 Windows MCI 播放 |
| `scripts` | 数据导入脚本 |
| `analysis_and_design` | 设计与架构文档 |
| `run.ps1` | 一键启动脚本 |
| `ChampR.bat` | 桌面启动器 |

## 主要模块

### 客户端 `crates/app`

- `src/main.rs`
  - LCU 监控
  - Apply Builds
  - 自动启动 LoL
  - DeepSeek advice loop
  - TTS 测试与设置
- `src/settings.rs`
  - 本地设置持久化
- `ui/app.slint`
  - 主窗口
  - Settings 窗口
  - Runes 窗口

### LCU 核心 `crates/lcu`

- `cmd.rs`
  - LCU 命令读取
  - LoL 客户端定位与启动
- `lcu_api.rs`
  - LCU REST 接口
- `live_client.rs`
  - 游戏内 `127.0.0.1:2999` Live Client Data
- `advisor.rs`
  - 阵容 / 实时数据 prompt 组装
- `deepseek.rs`
  - DeepSeek OpenAI 兼容客户端
- `tts.rs`
  - Windows TTS 调度：优先 Node sidecar，再回退旧实现
- `web.rs`
  - Data Dragon、后端数据源

### 后端 `crates/server`

- `src/db.rs` SQLite 数据访问
- `src/handlers.rs` API
- `src/models.rs` 请求 / 响应模型

## 启动命令

```powershell
.\run.ps1 doctor
.\run.ps1 server
.\run.ps1 app
.\run.ps1 crawler --all --output=./output/latest --concurrency=3
```

管理员运行客户端：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\run.ps1 app
```

## 环境变量

见 `.env.example`。

核心变量：

```text
CHAMPR_SERVER_URL
SERVER_ADDR
DATABASE_URL
DEEPSEEK_API_KEY
DEEPSEEK_MODEL
DEEPSEEK_THINKING
DEEPSEEK_REASONING_EFFORT
DEEPSEEK_STREAM
CHAMPR_TTS_RATE
CHAMPR_TTS_VOLUME
CHAMPR_TTS_VOICE
```

## 数据流

### 数据抓取

```text
OP.GG -> Playwright(Edge) -> JSON -> scripts/import-opgg-crawl.mjs -> server SQLite
```

### 对局辅助

```text
LCU / Live Client Data
  -> advisor prompt
  -> DeepSeek
  -> UI advice
  -> Windows TTS
```

### TTS 播报

```text
Rust TTS
  -> node packages/audio/cli.js
  -> msedge-tts
  -> zh-CN-XiaoxiaoNeural
  -> MP3 Buffer
  -> winmm.dll MCI
  -> 静默播放
```

## 当前已实现功能

- OP.GG 最新数据抓取
- 单英雄 Apply Builds
- LoL 客户端定位与启动
- LCU 状态监控
- Settings 面板
- TTS 参数与测试
- DeepSeek 配置
- 大师对局辅助开关
- 游戏内实时阵容 / 装备 / 等级读取
- 装备中文名映射

## 关键设计原则

- 状态只显示圆点 / 图标，详情按需展开
- 状态区和快捷操作区分开
- 所有长文本可复制
- 默认值始终要有兜底
- 空字符串不应覆盖代码默认值

## 开发注意

1. `.cache/`、`packages/opgg/.cache/` 不应提交。
2. `app.slint` 中的中文曾经出现过编码问题，修改时尽量用 ASCII 或确保 UTF-8。
3. Windows 路径 `C:\WeGameApps\...` 作为腾讯客户端默认路径。
4. TTS 优先使用 Node.js `packages/audio` sidecar，依赖 `msedge-tts`；安装依赖：`corepack pnpm --dir packages/audio install`。
5. LoL 启动器需要管理员权限时，会通过 `Start-Process -Verb RunAs` 处理。
6. 修改设置后应调用 `settings.save()`。
7. DeepSeek 默认模型为 `deepseek-v4-flash`。
8. Edge 神经语音默认使用 `zh-CN-XiaoxiaoNeural`，不要改回旧版 SAPI 语音。

## 提交规范

推荐格式：

```text
feat: <功能>
fix: <修复>
docs: <文档>
chore: <杂项>
```

不要提交缓存、编译产物和本地设置。
