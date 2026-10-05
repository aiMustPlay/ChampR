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
  - 主窗口(内容区三 Tab: 符文 / 对局数据 / 大师对话)
  - Settings 窗口
  - 无头预览: `cargo run -p champr --bin ui_preview [-- runes|settings]`
    把窗口用软件渲染器画进 `.cache/ui-preview-*.png`, 布局改动先自检再交付
    (不带参数 = 主窗, 已灌对局表格演示数据, 默认停在「对局数据」Tab)

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
CHAMPR_DUMP_SNAPSHOTS  # =1 时把 Live/LCU 原始快照写入 .cache/*.json 调试
MAOHOU_BIN           # maohou 引擎二进制路径(留空时自动定位: 兄弟仓 → PATH)
```

## 数据流

### 数据抓取

```text
OP.GG -> Playwright(Edge) -> JSON -> scripts/import-opgg-crawl.mjs -> server SQLite
```

爬虫默认同时抓 counters 页面的整条对位表(胜率从本英雄视角, 按场次排序)
和 skills 页面的技能加点优先级, 可用 `--no-counters` / `--no-skills` 关闭;
非 ranked 模式自动跳过。对位数据随后写入 BuildSection.counters,
客户端用它给 DeepSeek 报告"你对敌方X胜率Y%(场次), 优/劣势对位"。

### 对局辅助

```text
LCU(champ-select/bans/段位/当前符文页)
+ Live Client Data 全量快照(KDA/补刀/符文/技能/装备/面板/资源事件/杀人流水)
+ Data Dragon zh_CN 静态名表
+ OP.GG 分路数据(梯度/胜率/推荐符文/出装)
  -> match_context 结构化解析
  -> counter 规则引擎(确定性符文调整方案)
  -> advisor prompt(对位分析 + 符文对比 + 资源差)
  -> DeepSeek
  -> UI advice
  -> Windows TTS
```

关键模块: `crates/lcu/src/match_context.rs` 负责把 `allgamedata` 与 champ-select session
解析为 `LiveSnapshot` / `ChampSelectSnapshot`(含对位配对), `advisor.rs` 基于这些结构
生成中文 prompt。

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
- Live Client Data 全量解析(KDA/补刀/符文/召唤师技能/金币/加点/面板/资源事件)
- 选人阶段增强(ban/段位/当前符文页/对位英雄/OP.GG 分路统计)
- 对位分析与针对性符文对比 prompt(advisor)
- OP.GG 克制/对位数据抓取(counters 页 RSC, 45+ 条对位胜率)并注入选人与实时 prompt
- 本地 counter 符文规则引擎(lcu/counter.rs): 对位压力(counters 胜率)×敌方类型(DDragon tags+伤害类型)
  经可编辑 TOML 规则输出副系/防御碎片替换方案(主系基石保持英雄主流), 一键/锁后自动应用;
  规则文件内置默认 + %APPDATA%\champr\counter-runes.toml 用户覆盖, 解析出错自动回退内置,
  符文树/行合法性由内置 rune_lattice.json 校验; 与两页主流页任一页完全相同的方案不再另行推荐,
  不相同则附逐格差异(原→新)与取舍理由说明。设计全文见 analysis_and_design/counter-rune-system.md
- Ban位参考(按 counters 表低胜率+高场次筛 top3)显示在选人道具面板并注入选人 prompt
- 对位心理图谱(lcu/tips.rs): DDragon championFull.json(zh_CN 单请求)拿每英雄
  allytips/enemytips/blurb; 选人阶段符文窗展示"对位心理"卡(意图/弱点编号条目),
  并注入选人/实时 LLM prompt 作为意图软肋标签; 失败回退空图功能自闭合。设计见
  analysis_and_design/playstyle-intel.md(含心理四层模型与 Phase2/3 规划)
- 兵法心战系统(lcu/war.rs): 战略(分型固定 doctrine + 双主计)×战术(压力档驱动),
  三十六计全表为唯一合法计名集; 覆盖卡 data/war_strategy.default.toml 全英雄覆盖:
  11 张手写卡 + 162 张 LLM 起草(会话模型并行起草,脚本校验/报告/--merge,合并区有
  标记注释可整段删除); 用户级 %APPDATA%\champr\war-strategy.toml 拼错即丢弃,
  拼错整个文件回退内置; 选人窗深金心战卡 + 选人/实时 prompt 双注入。设计见
  analysis_and_design/war-of-psychology.md
- 符文面板(2026-10-04 起并入主窗「符文」Tab, 不再单独弹窗)按本局分路排序 +
  一键/锁定自动应用 OP.GG 最优符文页
- 符文对比卡(lcu/runetraits.rs, 确定性不依赖 LLM): 对局内拿 Live Client Data 的
  我方完整页 vs 对方基石+主副系 → 特性(消耗/爆发/续航/耐久/功能)配对 →
  1~3 句可执行提醒(距离/换血/等关键符文冷却)。对方符文选人期不公开, 故开局才出现
- 锁定后自动写入推荐出装文件(auto_apply_builds)
- 事件驱动目标提醒 TTS(一血/小龙/巢虫/先锋/男爵事件 + 刷新前 30s 倒计时), 档位: 全部/仅关键事件/静音
- 排队就绪自动接受对局(WS 监听 lol-matchmaking/v1/ready-check, InProgress 翻转瞬间 POST
  accept 一次; 默认开(用户拍板), 设置页"对局"卡可关)
- 系统托盘常驻(金底深框图标): 左键召唤主窗, 右键菜单退出——独占全屏盖窗时唯一触达入口
- LLM 统一通道(lcu/maohou.rs): provider(deepseek/lmstudio/openai 任意兼容端点)
  默认经 houmao 引擎子进程(maohou exec --no-tools), key 走子进程 env(CHAMPR_ENGINE_KEY)不上命令行;
  二进制定位 = settings.maohou_bin → MAOHOU_BIN env → exe 祖先下 houmao-mac → PATH;
  缺二进制一次性 warn 并降级直连 reqwest; 引擎失败原样报错不双请求。差集: 思维链/
  推理强度/流式在引擎模式不生效(UI 已标注)。见 analysis_and_design/maohou-integration.md
- DeepSeek token 节流(prompt 与上轮完全一致时跳过请求)
- 主窗口「对局数据」实时表格(lcu/advisor.rs::build_live_table → Slint LiveRow 模型,
  2.5s 刷新, 不走 LLM): 列 = 位/英雄/召唤师/KDA/补刀/等级/基石/技能/装备, 我方(蓝条)
  敌方(红条)分区, 我 = 金条+金底、对位 = 红条、阵亡行显示倒计时; 顶部概览(时间/比分/
  双方资源), 表下金卡给对位对比(补刀/等级/净击杀差值带正负号)与我方金币/加点/符文、
  近期击杀; 行序按分路(上/野/中/下/辅)。选人阶段(无 Live 数据)该 Tab 回退原文字面板。
  表头与数据行共用 Tokens.tw-* 列宽, 改列宽必须同时改这两处
- zh_CN 静态名表(英雄/符文/装备中文名)
- 原始快照落盘调试(CHAMPR_DUMP_SNAPSHOTS=1)

## 关键设计原则

- 状态只显示圆点 / 图标，详情按需展开
- 状态区和快捷操作区分开
- 所有长文本可复制
- 默认值始终要有兜底
- 空字符串不应覆盖代码默认值
- UI 尺寸/颜色只用 `app.slint` 顶部的 `Tokens` 全局定义，不在组件里写魔法数字
- 游戏时间隐形原则(第一性, 用户 2026-09-30 拍板): 对局期间零窗口零焦点事件,
  辅助信息只走 TTS 声音; 任何新功能默认不得在 InProgress 阶段显示窗口
- 窗口生命周期跟随 LCU 阶段: 选人开始把主窗内容区**自动切到「符文」Tab**(2026-10-04
  起符文面板并入主窗, 不再单独弹窗, 也就没有"关掉别重弹"的状态了); 用户手点过 Tab 后
  45 秒内不自动抢台。**不做任何置顶/悬浮小窗**(用户 2026-10-04 拍板去掉迷你窗):
  对局信息统一在主窗「对局数据」Tab 输出, 目标提醒走 TTS;
  也不做 overlay 大窗/自动赛后页(见 analysis_and_design/ui-lifecycle-v2.md 九节)
- 显示器固定走设置页手动选屏(settings.pinned_monitor, monitors.rs Win32 枚举),
  不做自动跟游戏屏——可解释性优于自动化; 主窗/设置窗落在指定屏
- 对局期间零窗口(用户 2026-09-30 拍板, 2026-10-04 进一步确认不需要悬浮小窗):
  对局信息只在主窗「对局数据」Tab 里滚动更新, 不弹任何置顶窗口;
  游戏必须完整独占一块屏, 因此不做也不允许出现覆盖游戏的小窗。

## 开发注意

1. `.cache/`、`packages/opgg/.cache/` 不应提交。
2. `app.slint` 中的中文曾经出现过编码问题，修改时尽量用 ASCII 或确保 UTF-8。
   **`ChampR.bat` 必须纯 ASCII**: cmd.exe 按 GBK 解析 .bat, UTF-8 中文注释行会被拆成
   垃圾命令("澶?90 不是内部命令"), 启动链路当场崩。批处理一行 CJK 都不许。
3. Windows 路径 `C:\WeGameApps\...` 作为腾讯客户端默认路径。
4. TTS 优先使用 Node.js `packages/audio` sidecar，依赖 `msedge-tts`；安装依赖：`corepack pnpm --dir packages/audio install`。
5. LoL 启动器需要管理员权限时，会通过 `Start-Process -Verb RunAs` 处理。
6. 修改设置后应调用 `settings.save()`。
7. DeepSeek 默认模型为 `deepseek-v4-flash`。
8. Edge 神经语音默认使用 `zh-CN-XiaoxiaoNeural`，不要改回旧版 SAPI 语音。
9. `scripts/gen-war-cards.mjs` 用心战系统给全英雄起草卡片: 需要 `.env` 内的
   `DEEPSEEK_API_KEY`(gitignore 已排除); 产物在 `output/war-draft/`(草稿+review报告),
   人工核对后 `--merge` 追加内置 TOML, `cargo test -p lcu --lib war::` 负责二次校验。
10. `crates/app` 有**两个 bin**(`champr` 与 `ui_preview`): `cargo run -p champr` 必须带
   `--bin champr`(Cargo.toml 已写 `default-run`)。2026-10-04 因为漏了这一步, 启动器
   里的 `cargo run` 直接 exit 101, 表现是"双击后什么都没有"。
11. `run.ps1 app` 会先构建 DeepSeek Web sidecar(pnpm), 首次启动较慢属正常; 服务端与
   客户端由 ChampR.bat 并发拉起, 客户端拉不到冠军表时会每 3s 重试(最多 2 分钟)。

## 提交规范

推荐格式：

```text
feat: <功能>
fix: <修复>
docs: <文档>
chore: <杂项>
```

不要提交缓存、编译产物和本地设置。
