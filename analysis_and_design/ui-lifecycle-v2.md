# UI v2 —— Token 体系与阶段化窗口编排

> 实施状态: 本文档每一项对应代码(rust/slint)中的实现位置, 可交叉核对。
> 上游约束: [ui-design-system.md](ui-design-system.md) 的颜色/字号语义不变,
> 本文档把它们落成可维护的 token 与窗口生命周期。

## 一、问题与目标

| 现象(v1) | 目标(v2) |
| --- | --- |
| 170+ 处魔法数字/颜色散落 850 行 slint | 全部收敛到单一 `Tokens` 全局 |
| 窗口尺寸不可缩, 无最小尺寸约束 | 每窗口 preferred + min, 按钮 stretch 自适应 |
| 符文窗 hover 就弹、关了还弹 | 阶段驱动 + 显式关闭记忆 |
| Counter 方案可用/不可用只靠按钮灰化 | 卡片永远在场, 解释为什么可用/不可用 |
| 中英混排 | 界面全中文(术语除外) |

## 二、Design Tokens(app.slint 顶部 `Tokens` global)

间距(4 起 8 进): `s-xs:4 s-sm:8 s-md:12 s-lg:16 s-xl:24`
字型: `f-title:22 f-section:15 f-body:14 f-aux:12 f-mini:11`
控件: `row-height:32(状态行/按钮最低高) row-field:40(表单行) btn-height:36 icon-btn:28 status-dot:8 radius:8`
颜色(**单一深金调色板, 用户 2026-10-03 拍板统一**):
- 语态 `c-ok #3fbf6f / c-warn #e6a817 / c-err #e2604f / c-muted #8a94a6`
- 表面 `c-page #0e1628 / c-card #16233a / c-card-border #2a3b55`
- 文字 `c-text #e8eef6 / c-text-secondary #9fb3c8 / c-text-faint #6f8299`
- 强调 `c-gold #c89b3c / c-accent-bg #241d10 / c-accent-border #6b5527 /
  c-accent-title·text #d9c78e`

### 2.1 全屋同色(历史: 曾按流程分派)

2026-10-03 前分两派: 游戏流程内窗口(符文/迷你)Hextech 深金, 管理态窗口(主窗/设置)
浅灰工具风。用户拍板统一为深金, 理由是半深半浅在切窗时观感断裂; `c-dark-*` 那套
别名随之并入通用名(c-page/c-card/...), 只剩一套 Tokens。

| 窗口 | 配色 |
| --- | --- |
| 主窗 / 设置窗 / 符文窗 / 迷你窗 | 同一套 Hextech 深金(Tokens) |

实现要点: Tokens 只管我们自绘的部分; Slint 的 `TextEdit`/`LineEdit`/`CheckBox`/
`ComboBox` 跟随操作系统主题, 靠 **Rust 端 `Palette.color-scheme = Dark`** 强制翻转
(app.slint 里 `export { Palette } from "std-widgets.slint";` 拿到句柄;
该属性与风格内部 `FluentPalette.color-scheme` 双向绑定, 赋值后会解掉与系统主题的绑定)。
写 `Palette { ... }` 元素或同名 `global Palette` 都无效(前者编译报错, 后者 widgets 不读)。
窗口尺寸: `win-main 520×1120 (min 440×860) / win-runes 620×1020 (min 520×640) / win-settings 480×830 (min 420×600)`

修改样式**只改 Tokens**。新需求需要新尺寸时先进 Tokens, 再给语义名。

## 三、阶段化窗口编排(与游戏进程同步)

| LCU/Live 阶段 | 主窗 (SourcesWindow) | 符文窗 (RunesWindow) | 设置窗 |
| --- | --- | --- | --- |
| Idle / 大厅 | 常驻可见(启动即 show) | 隐藏 | 手动开 |
| ChampSelect | 对局面板切到选人信息 + Ban/Counter 提示 | **自动弹出**: 我方 hover/选人变化即刷新; **用户手动关闭后本局不再自动弹**(runes_window_dismissed; session Delete 或断连时复位) | 手动开 |
| 锁定瞬间 | 触发自动符文(优先 Counter 方案)/自动出装 | 由"对位已确认"状态行提示可切换 | - |
| InProgress | 对局面板切实时数据(2.5s 刷新) + 教练对话; TTS 目标提醒按档位播 | 随选人 session Delete 自动隐藏, 数据清空 | - |
| Ended / 断连 | 状态灯复位 | 自动隐藏 + 复位标记 | 保持 |

规则:
1. **从不主动抢占焦点** —— 只在 handle 未 dismiss 时 show(), 不 `request_focus`。
2. **手动关闭优先于自动显示** —— 用户表达过意图, 本局尊重它。
3. **状态提示行承担过渡信号**(如"对位已确认: 可应用 Counter 符文"), 只用 None→Some 跃迁触发, 不刷屏。
4. **窗口内容即便隐藏也保持最新**(dismiss 时照常写 props)——重开即所见。

## 四、按钮编排(符文窗)

```text
[ 应用 Counter 符文 ] [ 应用主流最优符文 ]     ← 各 stretch:1, counter 可用时其为 primary
        ( ☑ 锁后自动符文 )  ( ☑ 锁后自动出装 )  ← 自治开关行, 与动作分离
┌ Counter 符文(本地规则, 按对位与敌方类型调整) ─┐
│ 可用时: 方案摘要 + 逐格差异 + 理由             │
│ 不可用时: 灰字解释为什么(等锁定/数据不足/与主流一致) │
└───────────────────────────────────────────────┘
...符文列表(按本局分路优先) + 逐页"应用"...
[ 关闭 ]
```

决策点: 两类"应用"|COUNTER 是条件推荐, 不可用不隐藏而是说明 —— 可发现性 > 极简。

## 五、弹窗体系(有意不做模态)

- 详情走 `ℹ️` → 主窗 coach log 详情区(append_info_log), 已在 v1 就好评
- 强提醒走跃迁状态行(见三世3)
- 无破坏性操作需要确认弹窗 —— 应用符文/出装都是幂等可覆盖, 无需 confirm
- DeepSeek web 登录窗是唯一外部模态(浏览器), 由用户在设置页手动触发

## 六、自适应清单(本次落地)

- 窗口: preferred + min 尺寸全配齐(大号 token)
- 动作按钮行: stretch 权重分配, 窗口加宽时按钮自动铺宽
- 页面背景统一 c-page; 卡片白底 + 1px 边 + radius 全 token 化
- 字号五色阶全 token 化; 150+ 处分发已清理
- RunesWindow 标题应用化: "Runes" → "符文"(此前唯一英文标题)

## 七、已知保留项(后续可选)

- 主窗 coach log / 对局面板 TextEdit 高度固定 —— stretch 分配留给后续有人抱怨再动
- `width: 110px` 的设置表单标签列 —— 有意对齐, 保持字面量(加注释)
- 深色模式: Tokens 已具备替换基础, 暂未做

## 八、交叉引用

- Token 定义: `crates/app/ui/app.slint` 顶部 `Tokens`
- 窗口生命周期: `crates/app/src/main.rs`(show_champion_runes / on_close_requested / 三处 reset)
- 状态跃迁提示: main.rs WS handler 的 `plan_newly_available` 分支
- 迷你窗生命周期: main.rs `match_lifecycle_task` 的 mini_wek 分支
- 显示器固定: `crates/app/src/monitors.rs`(Win32 枚举) + main.rs `pin_window_to_monitor`

### 8.1 显示器固定(手动配置)

**决策: 手动选屏, 不做自动跟游戏屏。** 自动检测 LoL 窗口所在屏(无边框/独占全屏切换
会翻转检测结果)比手动配置一次更不可靠; 用户只要"我的游戏屏别被占"这一件事的确定性。

- 设置页 "显示器定位" 卡片: 下拉 = 启动时 `EnumDisplayMonitors` 枚举 (
  `0 - 主显示器 2560x1440 @(0,0)`...), 选 0=不固定
- 持久化 settings.pinned_monitor(-1/0/1..), 越界(之前选过的屏拔了)自动静默回退
- 固定生效点: 主窗/设置窗 = 工作区居中; 迷你窗 = 工作区右上角; 符文窗 = 工作区居中
  (show() 之后用 window.size() 精锚定)
- 插拔显示器需重启(一次枚举, 不监听热插拔——稳定优先)
- 坐标全部走**物理像素**(`WindowPosition::Physical`), 多 DPI 混排不出布局错乱

## 九、外部基线与取舍(调研结论)

调研原文: [ui-redesign-research-notes.md](ui-redesign-research-notes.md)。
原始抓取材料: `.cache/research/`(不入库)。

### 平台通行惯例(Overwolf 系: Porofessor/Blitz/OP.GG/Facecheck)

五阶段值班表: idle 托盘常驻 → 选人自动弹面板(锁定时自动写符文) → loading 选人面板收 →
对局中主窗隐藏+overlay 折叠 → 结算自动弹赛后页。我们与其能力边界不同(Slint 独立窗、
无透明 overlay), 取舍如下:

| 惯例 | 我们的取舍 |
| --- | --- |
| 选人自动弹面板 + 每会话一次 + 尊重手动关闭 | ✅ 已实现(`runes_window_dismissed`)并与之对齐 |
| 锁定瞬间自动写符文 | ✅ 已实现(优先 Counter 方案) |
| 对局中"少即是多" | ✅ 340×220 置顶迷你窗(本文件新增), 每局进场弹一次, X 关掉不重弹, 离局自动收 |
| 主窗对局中自动隐藏 | ❌ 不做 — 主窗还有教练对话用途; 让迷你窗承担"少即是多"即可 |
| 结算自动弹赛后页 | ⏳ backlog — 需要 end-of-game 数据聚合设计 |
| 托盘常驻/随游戏进程自启 | ⏳ backlog — 涉及 tray icon + autostart, 本轮不扩 |
| overlay 计分板大窗 | ❌ 非 overlay 应用场景不允许(纯遮挡) |

### Slint 官方惯用法核验摘要

- `global` 是**每窗一份**实例(官方文档原话) — 本项目的 `Tokens` 全是常量所以无碍;
  若日后做主题切换需要 Rust 侧统一广播或重启窗口
- `*stretch` 是权重(float), 内置控件只有 0/1; 网格只用于严格表格, 流式用 H/V 嵌套
- 断点惯用法: `states + when: root.width < bp` (states 不能放 global)
- Window 的 `min/max/preferred-width` 会交给窗口管理器执行; `always-on-top` 官方即迷你窗用法
- 点击高度: Fluent 惯例 28~32px 即可(我们是桌面应用, 不追触屏 44px)

### 尺寸"标准"的定标

现状(距官方 4px 网格留有少量差异, 保留有意为之):
- 字号: 11/12/14/15/22 (官方建议还含 13/16/18/24, 我们压缩了梯度)
- spacing: 4/8/12/16/24 (官方至 48, 我们窗口密度用不上 48)
- 断点 640/860/1120 记录备查, 现三窗口均窄于 640, 暂不启用 states+when 断点

