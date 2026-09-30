# ChampR UI 系统性重设计 · 调研纪要

> 目的：为 ChampR（Rust + Slint，独立窗口、非 overlay 注入）的 UI 重设计提供
> (A) 主流 LoL 桌面助手的窗口生命周期/编排惯例，
> (B) Slint 1.x 响应式/尺寸系统的官方最佳实践。
> 出处标注约定：**[已核验]** = 本次实际抓取到页面内容；**[平台惯例]** = 多源一致的行业通用行为描述，未逐条抓原文；**[需复核]** = 建议落地前人工确认。
> 调研日期：2026-09-30。Wayback 镜像链接用于绕过反爬的官网。

---

## 一、课题 A：主流 LoL 助手的窗口生命周期/编排惯例

### A.0 大前提：它们大多是 overlay 应用，我们不是

- Porofessor、Blitz、OP.GG for Desktop、Facecheck 均作为 **Overwolf 平台应用** 分发（Overwolf 商店页面可直查）：[Porofessor](https://www.overwolf.com/app/Porofessor.gg-Porofessor)、[Blitz](https://www.overwolf.com/app/Blitz-Blitz)、[OP.GG for Desktop](https://www.overwolf.com/app/OP.GG-OP.GG_for_Desktop)、[Facecheck](https://www.overwolf.com/app/ProGuides-Facecheck) **[已核验：页面 HTTP 200]**。
- Overwolf 应用的架构惯例是「**桌面窗口(desktop windows) + 游戏内透明 overlay 窗口(in-game windows) + 游戏事件驱动（GPU 渲染层注入）**」，随游戏启动自动拉起（launch triggers），对局内信息画在游戏画面上。**[平台惯例 / 文档入口：[Overwolf Developers](https://dev.overwolf.com/)]**
- U.GG 桌面应用已停止维护（官方引导至网页版）**[需复核 — https://u.gg/]**，移动端/网页继续；Mobalytics 半弃更。结论：独立桌面辅助的活化石不多了，行为惯例主要看 Overwolf 四家。
- Porofessor 官网（Wayback 2026-09 快照）首页分栏为「Get the in-game app / Desktop app support / Featured Games」，并注明网页端与 app 端数据在 Riot 接口故障时互不影响，说明其 app 有独立的游戏数据通道 **[已核验 — [porofessor.gg 快照](https://web.archive.org/web/20260918145224/https://porofessor.gg/)]**。

### A.1 各游戏阶段的窗口编排（Overwolf 系通行模式）

| 阶段 | 桌面窗口（companion） | 游戏内窗口 | 关键行为 |
| --- | --- | --- | --- |
| 大厅 idle | 主面板可开可不开；多数常驻托盘 | 无 | 随 LoL 客户端/游戏进程启动自动拉起应用本体；设置入口在桌面主窗口（当然也可以进设置关掉"随游戏启动"）|
| 选人 champ-select | **值班窗口**：自动前置/出现，显示队友与对手统计、对位、ban 建议 | （LoL 客户端阶段，overlay 不可见）| Facecheck/Porofessor 在选人时给队友打标签显示熟练度；Blitz **锁定英雄瞬间自动导入推荐符文页+召唤师技能**（可关）；OP.GG Desktop 提供一键 perk 自动设置 **[平台惯例；官网职能描述交叉印证]** |
| 加载 loading | 选人面板**自动收起**（阶段结束即消失）| 准备载入 | 桌面窗口退到后台/最小化 |
| 对局中 in-game | 桌面窗口隐藏/最小化，默认**不可见** | **主战场**：透明 overlay；对局计分板多挂到 Tab 长按才显示；计时器/刷怪提示用小块 overlay；默认折叠、热键唤起 **[平台惯例]** |
| 结算 end | **自动弹出赛后总结面板**（KDA/经济/评分），随后恢复 idle | 关闭 | Blitz/Porofessor 均有赛后自动页 **[平台惯例]** |

要点提炼（阶段编排的底层规律）：
1. **窗口有"值班表"**：同一信息不会在两个阶段重复展示；选人面板属于选人阶段，结束即自动关；赛后面板属于结算阶段，自动弹+一键关。
2. **自动弹 = 每会话一次**：自动出现的窗口遵守"本次会话内尊重用户主动关闭"，不会反复弹出。
3. **对局中少即是多**：对局内默认只留最小信息量（且在我们无法 overlay 的前提下⇒应主要靠 TTS）。
4. **设置/主面板常驻**：主窗口+托盘是常态入口，阶段窗是临时窗。主窗口记住上次位置尺寸。

### A.2 对 ChampR（非 overlay、独立窗口）的适用性

**建议采纳：**
- 沿用"阶段值班"状态机，与我们现状一致（`runes_window_dismissed` 每选人会话一次、选人结束重置）。扩展到完整体：大厅=主面板按需；选人=主面板前置 + 符文面板自动弹（锁定英雄时触发自动应用）；loading=符文面板自动关、主面板最小化/托盘化；对局中=主面板默认隐藏，提供**可选的 340×220 置顶迷你窗**（比分/资源/下一目标计时），大面板用热键或任务栏还原；结算=自动还原主面板到"赛后"页签。
- 对局中的信息主通道用 **TTS + Windows toast**，窗口信息只做"顺手可看"。这与 AGENTS.md"状态只显示圆点/图标、详情按需展开"一致。
- 自动弹出的所有行为都进设置页开关（弹窗时机/是否前置/迷你窗开关）。

**不建议采纳（Overwolf 特性不适配我们）：**
- 计分板式大 overlay：我们做不了透明穿透，任何常开置顶大窗都会遮挡游戏画面，弊大于利。
- 热键驱动 overlay 调起：Slint 全局热键能力有限，跨进程快捷键要靠 Rust 侧额外 crate，收益低；TTS 已覆盖提醒诉求。

**出处链接：**
- [porofessor.gg（Wayback 2026-09 快照）](https://web.archive.org/web/20260918145224/https://porofessor.gg/) **[已核验]**
- [OP.GG 公司页（Wayback 2025-04 快照，列出 OP.GG for Desktop / Streamer Overlay 产品线）](https://web.archive.org/web/20250402091841/https://op.gg/about) **[已核验]**
- [blitz.gg/lol（官网，实时抓取）](https://blitz.gg/lol) **[已核验]**
- Overwolf 商店：Porofessor / Blitz / OP.GG for Desktop / Facecheck（链接见 A.0）**[已核验存在]**
- [Overwolf Developers（窗口模型/游戏事件文档入口）](https://dev.overwolf.com/) **[已核验可达]**

---

## 二、课题 B：Slint 1.x 尺寸/响应式系统最佳实践（官方文档核验版）

### B.1 Global singleton 作为设计 token：官方机制与坑

Slint 的 `global` 是为全局共享数据设计的语言级单例 **[已核验 — [language/globals](https://docs.slint.dev/latest/docs/slint/reference/language/globals/)]**：
- 语法：`global Name { /* property / callback / function */ }`，可放 `in/out/in-out/private` 属性、回调、函数。
- 引用：`Name.property`，任意绑定/回调/函数可用。
- **坑 1：每个窗口一个实例**。文档原文：*"Separate windows — and a window and a SystemTrayIcon exported alongside it — each hold their own instance"**。⇒ 我们的主窗/符文窗/设置窗若都要用 `Tokens`，token 应是**编译期常量绑定（字面量默认值）或在 Rust 侧统一注入**，不要假设改一处全局实例跨窗生效（改字体缩放这类运行时 token 时尤其注意）。
- **坑 2：global 内没有 scale factor / rem 上下文**：*"A global exists independently of any window, so it has no scale factor... cannot convert between logical (px) and physical (phx) lengths"**。⇒ token 一律用 `px`（逻辑像素），不要在 token 文件里做 `phx/rem` 换算。
- 导出规则：从导出根组件的文件再 export 的 global 会暴露给 Rust 侧业务代码读写——适合"主题/Rust 侧控制"用例。
- 官方自己就这么干：`std-widgets` 提供 [Palette 单例](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/globals/palette/)（`background/foreground/alternate-* / control-* / accent-* / selection-* / border` 等命名画刷 + `color-scheme` 强制明暗）和 [StyleMetrics 单例](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/globals/stylemetrics/)（**`layout-spacing`、`layout-padding`** 即官方布局 token）。**[均已核验]**
- 结论：**"所有尺寸/颜色走全局 Tokens"（AGENTS.md 已定原则）就是 Slint 官方惯用法**；结构上建议拆两个全局：`Tokens`（原始常量：spacing/radius/font/hit-size/window-size）与 `AppPalette`（语义色：bg/surface/on-surface/accent/success/danger，参照 Palette 命名习惯）。

### B.2 布局三件套与约束体系的官方边界

全部出自 [layouts 概览](https://docs.slint.dev/latest/docs/slint/reference/layouts/overview/)（**已核验**，含原文引述）：

- 每个元素都可声明约束：`min-width/min-height/max-width/max-height/preferred-width/preferred-height`（length）。
- **stretch 权重**：`horizontal-stretch / vertical-stretch` 是 float，*"当为 0 时元素不伸展，除非所有元素都是 0；内置控件取值只有 0 或 1"*。⇒ 侧栏固定（0）+ 内容区 1 是两栏自适应的标准写法；多列等比分配用相同权重。
- **`layout-order`**：只改视觉顺序不改焦点序——可做"窄屏时交换块顺序"的廉价响应式手段。
- `cross-axis-self-alignment`（auto/stretch/start/end/center）：单个子元素覆盖容器交叉轴对齐。
- **GridLayout 的边界**：用于**行列严格对齐**的表单/属性表（子元素挂 `col/row/colspan/rowspan`）；文档把 [GridLayout](https://docs.slint.dev/latest/docs/slint/reference/layouts/gridlayout/)、[HorizontalLayout](https://docs.slint.dev/latest/docs/slint/reference/layouts/horizontallayout/)、[VerticalLayout](https://docs.slint.dev/latest/docs/slint/reference/layouts/verticallayout/) 并列——流式堆叠（卡片列表、标签流）不要用 GridLayout 模拟，用 H/V 布局嵌套或 FlexboxLayout。
- **Flickable（滚动）**：[Flickable](https://docs.slint.dev/latest/docs/slint/reference/gestures/flickable/) 是底层元素，*"当 content-width/height 大于父级宽高时变得可滚动"*；`interactive:false` 时事件穿透下发；`mouse-drag-pan-enabled` 只影响鼠标拖动平移。桌面端建议直接用 [ScrollView](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/scrollview/)（样式一致的滚动条），底层 Flickable 只在做自定义行为时用。**[已核验]**
- **窗口级约束**：[Window 元素](https://docs.slint.dev/latest/docs/slint/reference/window/window/) 文档明确：*"窗口尺寸受布局约束限制……窗口管理器会尊重 min-width/max-width 使窗口不能缩到比它更小/更大；初始尺寸用 preferred-width 控制"*，另有 `always-on-top / no-frame / minimized / maximized / default-font-family / default-font-size`。**[已核验]** ⇒ 阶段窗口尺寸组合直接声明在各自 Window 的 preferred + min/max；迷你窗用 `always-on-top:true`；`default-font-size` 绑 token 即可全局调字号。

### B.3 窗口尺寸改变时的内容自适应（断点）惯用模式

Slint 没有 CSS 媒体查询，惯用法是**"根宽度比较 + states/条件绑定"**：
- 在根组件声明 `in property <length> bp-md: 720px;`（或直接引用 `Tokens.bp-md`），用 `root.width > bp-md` 驱动 `states [compact when ... : ...]` 变换布局（收起侧栏、卡片网格从 3 列变 2 列/1 列）。
- 多断点时用多组 states 或一个 computed enum。logs 注意：states 只能放在组件里，不能放 global（见 B.1 坑2，global 禁止 states）。
- 字号随宽度变化不建议线性插值（保持档位制，如 12/14 两档）；密度（spacing 12↔8）同理档位制。
- 文字溢出：桌面工具类 UI 用 `overflow: elide` + tooltip，而非强制换行拉伸行高。

### B.4 标准控件尺寸约定（8pt grid 的来源与换算）

- Slint 自带样式族（fluent / material / cupertino / qt…见 [Widget Styles](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/style/)）本身就是"8pt/4pt 网格"的落地：material 族遵循 Material Design 8dp 网格，fluent 族遵循 Fluent 4px 基准。**[已核验：样式枚举存在；网格细节为样式族事实]**
- Slint `length` 用**逻辑像素 px**（高分屏自动换算物理像素，另有 phx、pt、rem 单位，见 [numeric types #length](https://docs.slint.dev/latest/docs/slint/reference/property-types/numeric-types/#length)）。⇒ token 直接按"逻辑 px、4 的倍数"命名即可，等于 8pt/4pt 网格。
- 桌面指针精度 ≠ 触屏：不必按触屏 44/44px 旋钮；参照 Fluent 桌面惯例取 **28–32px 行高/按钮高** 作为最小点击目标，`Tokens` 里单列一项 `hit-min`。字号基线：Windows 桌面应用近年主流 13–14px（Segoe UI），LoL 类深色工具建议正文 13px。
- 颜色语义命名照抄官方 Palette 的粒度（surface/foreground/accent/selection/border + 明暗自适应），避免"研发色"（red500）直上界面。

---

## 三、结论与建议

### 3.1 直接可用的结论

1. **窗口生命周期**（课题 A）：四件套 = 常驻主窗口(托盘兜底) + 选人自动弹符文面板(每会话一次、锁定自动应用、选人结束自动关) + 对局中默认隐藏主窗口(可选 340×220 置顶迷你窗) + 结算自动弹赛后页。**不要**模仿 overlay 计分板。
2. **Token 化**（课题 B）：继续执行 AGENTS.md 既定原则（`app.slint` 顶部 `Tokens` 全局、无魔法数字），并升级为"Tokens(原始) + AppPalette(语义色)"双层；注意 global **每窗口一份实例**，跨窗 token 改动要么重启各窗、要么 Rust 侧统一广播。
3. **响应式**：`min/max/preferred` + `horizontal-stretch` 权重为主体，断点用 states by root width；GridLayout 只用于真表格；滚动一律 ScrollView。

### 3.2 建议 token 列表（默认值，单位均为逻辑 px；4px 网格）

| 类别 | 名称与取值 |
| --- | --- |
| spacing | `space-1:4, space-2:8, space-3:12, space-4:16, space-5:24, space-6:32, space-8:48`；布局默认：`layout-gap:8`、`panel-pad:12`、`section-pad:16`（对应官方 StyleMetrics 的 layout-spacing/layout-padding 概念）|
| radius | `radius-xs:2, radius-sm:4, radius-md:6, radius-lg:10, radius-pill:999`；卡片/面板用 `radius-md`，按钮 `radius-sm`，状态点用 `radius-pill` |
| font-size | `fs-caption:11, fs-body:13(基准), fs-body-lg:14, fs-sub:16, fs-title:18, fs-display:24`；字重 `fw-regular:400, fw-medium:500, fw-bold:700`；`fs-mono:12`（数据列）|
| 点击目标/行高 | `hit-min:28`（按钮/菜单项最小高）、`hit-comfort:32`（主按钮）、`row-dense:24`（数据表行）、`row-regular:32`、`icon-btn:28`、`icon-16/20/24/32` |
| 描边/焦点 | `border-hair:1`、`focus-ring:2`、`outline-offset:1` |
| 暗色语义色(AppPalette) | `bg:#0F1116, surface:#171A21, surface-2:#1E222B, on-surface:#E6E8EE, on-dim:#9AA3B2, accent:#4C8DFF, success:#3FB950, warn:#D29922, danger:#F85149, border:#2A2F3A`（示例值，按现有视觉调）|
| 断点 | `bp-sm:640, bp-md:860, bp-lg:1120`（主面板内部用）|
| 窗口尺寸组合 | 主窗口 `preferred 1100×720, min 860×560`；符文面板(选人阶段) `preferred 420×640, min 380×520`；设置窗 `preferred 720×560, min 640×520`；对局迷你窗 `固定 340×220, always-on-top=true, 可 no-frame`；赛后页复用主窗口 |
| z/层级 | Slint 无 elevation token：弹层用 [PopupWindow](https://docs.slint.dev/latest/docs/slint/reference/window/popupwindow/)，tooltip 用内置 [ToolTip](https://docs.slint.dev/latest/docs/slint/reference/window/tooltip/) |

### 3.3 不适合我们的（避免过度设计）

- overlay 式透明大窗/Tab 计分板（技术不可行+体验负分）。
- 触屏式 44px+ 大按钮、字号线性插值缩放。
- 为 token 单建"运行时主题引擎"：Slint 主题切换直接绑 `AppPalette` 属性即可，主题数 ≤2 时别上做状态机。

---

## 附：抓取与核验记录

- 直连可抓取并解析：docs.slint.dev（9 个页面）、blitz.gg/lol、Wayback 下的 porofessor.gg 首页与 FAQ、op.gg/about。
- 抗爬无法直连抓取（已用 Wayback 快照替代或仅核验存在性）：overwolf.com 商店页（Next.js shell，HTTP 200）、porofessor.gg 真站（Cloudflare challenge）、facecheck.gg（官网 404/redirect）、u.gg（Cloudflare）、support.blitz.gg（Intercom SPA shell）。
- dev.overwolf.com 旧 deep path 已失效（SPA 回退到根），仅核验根域可用；窗口模型细节以平台通行惯例表述。
- 原始页面与抽取文本留档于 `.cache/research/`（不入库）。
