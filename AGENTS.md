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
  - **主窗是无边框窗口**(`no-frame: true`) + 自绘 `TitleBar`: 图标/名字/构建戳 +
    最小化 `—` / 关闭 `×`, 整条可拖动, 双击最大化。原因: 用户 2026-10-05 指出原生
    标题栏是系统浅色、与深金主题冲突("很丑")。
    - 拖动靠 `TitleBar.drag(dx, dy)` 回调 → Rust `window.set_position`, Slint 没有内置拖动
    - 最大化状态下先 `set_maximized(false)` 再移动, 否则位置改不动
    - 字形用 Latin-1 区(`—` U+2014 / `×` U+00D7): `✕`(U+2715) 在默认字体里是空白
    - 代价: 没有系统边框 → 拖动边缘缩放失效; `main.rs::FRAME_W/FRAME_H` 因此改成 0
      (无边框时外框 == 客户区, 实测 outer=830x1343 == client=830x1343)
  - Settings 窗口仍用原生标题栏(未被要求改)
  - 无头预览: `cargo run -p champr --bin ui_preview [-- runes|settings|champselect]`
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
- 自动禁人 / 自动选人(lcu/autopick.rs, 用户 2026-10-05: "继续实现默认禁人和默认选人"):
  决策是**纯函数** `decide(session, prefs, names, sections) -> AutoAction`, 10 个单测覆盖
  真实 session 结构; 执行器在 app 的 WS 选人事件里(watch /lol-champ-select/v1/session),
  每次事件最多提交一个动作(LCU 不允许并发改同一动作), `auto_action_last` 去重,
  **失败会清空去重记录**以便重试, 失败只写日志不弹窗。
  规则: 只处理 `actorCellId == localPlayerCellId` 且 `isInProgress` 未完成的动作;
  禁人 = 优先禁用名单里第一个还没被禁的英雄(名单空则什么都不做, 不瞎禁);
  选人 = 首选名单 > 用户已悬停的英雄 > OP.GG 该分路胜率最高(样本 >= 300 场);
  **先悬停(completed=false), 剩余时间 <= auto_pick_lock_seconds 才锁定**(默认 3 秒,
  给用户反悔窗口)。两个开关默认**开**(用户要求「默认禁人/默认选人」, 与自动接受一致),
  护栏保证开着也安全: 禁用名单为空则不禁人、选人优先沿用你自己悬停的英雄; 动作会 TTS 播报
  ("已禁用X/已预选X/已锁定X")。
  LCU 接口: `lcu_api::patch_champ_select_action(auth, action_id, champion_id, completed)`
- 系统托盘常驻(金底深框图标): 左键召唤主窗, 右键菜单退出——独占全屏盖窗时唯一触达入口
- LLM 统一通道(lcu/maohou.rs): provider(deepseek/lmstudio/openai 任意兼容端点)
  默认经 houmao 引擎子进程(maohou exec --no-tools), key 走子进程 env(CHAMPR_ENGINE_KEY)不上命令行;
  二进制定位 = settings.maohou_bin → MAOHOU_BIN env → exe 祖先下 houmao-mac → PATH;
  缺二进制一次性 warn 并降级直连 reqwest; 引擎失败原样报错不双请求。差集: 思维链/
  推理强度/流式在引擎模式不生效(UI 已标注)。见 analysis_and_design/maohou-integration.md
- DeepSeek token 节流(prompt 与上轮完全一致时跳过请求)
- 主窗口「对局数据」**一张状态表格贯穿全程**(用户 2026-10-05 拍板: "用一个状态表格
  维护选人、游戏过程中的所有关键数据")。数据模型 lcu/advisor.rs::DataTable
  (列 = title/width/emphasis, 行 = cells/section/mine_team/mine/opponent),
  UI 是 app.slint 的 DataTable 组件; **列在选人与对局两个阶段完全相同**:
  `位 / 英雄 / 召唤师 / 段位 / 个人胜率 / 英雄胜率 / KDA / 补刀 / 等级 / 装备(弹性列)`
  - **个人胜率 vs 英雄胜率(用户 2026-10-05: "我要个人战绩")**:
    - `个人胜率` = **该玩家自己**本赛季单双排总胜率, 由 LCU
      `/lol-ranked/v1/ranked-stats/{puuid}` 的 wins/losses 算出(lcu::advisor::RankInfo,
      选人期按 summonerId 查一次并缓存)。与英雄无关 → 换英雄后仍然有效
    - `英雄胜率` = OP.GG 该英雄该分路的**全服**胜率
    - **个人·分英雄胜率拿不到**: LCU 只提供本地玩家的比赛记录, 别人的战绩不公开;
      OP.GG 也没有国服召唤师数据。**不要用全服胜率冒充个人值**, 也不要把两列混为一谈
    - 对局阶段胜率列取值: 先按**当前正在玩的英雄**查 OP.GG 分路数据, 查不到才退回
      选人档案 —— 选人后可以换英雄(交易), 用档案值会显示成"旧英雄的胜率"
  - 选人 `build_champ_select_table` → 返回 `(DataTable, Vec<RosterEntry>)`:
    段位/个人胜率、OP.GG 该分路胜率已填; KDA/补刀/等级显示 "-";
    最后一列表头改成"场次"(放 OP.GG 样本量)
  - 对局 `build_live_table(..., roster, sections)` → 用实时数据填满; 段位/个人胜率来自
    roster, **匹配必须带上队伍**(同队+分路 → 同队+英雄 → 召唤师名 → 英雄兜底),
    否则我和对位同分路会命中同一份档案, 显示成一模一样的段位/胜率(用户 2026-10-05 报障);
    召唤师名以 Live 数据为准, 缺失时用缓存
  - 磁盘缓存(crates/app/src/cache.rs, 用户 2026-10-05: "每次对局都要重新查一遍胜率"):
    段位/个人胜率按 summonerId 存 `%APPDATA%\champr\cache\ranked-stats.json`(TTL 24h),
    OP.GG 分路数据按英雄 id 存 `opgg-sections.json`(TTL 6h), 启动时读入, 拿到新数据即落盘。
    `ensure_opgg_sections` 另有 10 分钟冷却(内存 `opgg_attempt_at`): 对局任务每 2.5s 会为
    场上所有英雄调它, 没这层节流会每 2.5 秒白跑一次无效请求。坏缓存文件只记日志不崩。  - 缓存: `AppState.match_roster`, 选人每次 session 更新都刷新;
    **选人会话 Delete 事件不要清空**(那正是"选人结束、正在进游戏"的时刻, 清掉会让整局
    段位/胜率都是 "-"), 改为对局结束、阶段回 Idle 时清空(见 match_lifecycle_task)
  - 顶部: 选人=ban 行; 对局=比分 + 双方资源。表下金卡给对位对比(带正负号)+金币/加点/符文
  - 配色: 我方行蓝条 / 敌方红条 / 我 = 金条+金底 / 对位 = 红条; 行序按分路(上/野/中/下/辅)
  - **列宽 0 = 弹性列**, 由 main.rs::resolve_flex_columns 按窗口实际宽度折算成像素;
    不能在 Slint 里靠 horizontal-stretch(Slint 的 Text 写了 width: 0px 就钉死, 整列消失)
  - 表格自绘, 选中不了: 「复制」按钮用 advisor::render_table_text 生成的纯文本
    (中文按 2 格宽对齐, 超长单元格截断成 …), 文案存 AppState::live_table_text
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
