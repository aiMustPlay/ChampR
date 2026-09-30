# 对战分析引擎 · 系统性架构评审

> 评审对象: match_context(解析) → counter(符文规则) / tips(心理图谱) / war(心战)
> → advisor(prompt 组装) → main.rs(编排) → UI(三窗) / TTS 全链路。
> 方法: 6 路模块并行审计 + 主链路对 3 个 blocker 原始核验(代码证据 + data/champr.db
> 173 英雄 344 符文页真实统计)。结论依据均可复现。

## 一、总判定

**简要性: 6.5/10 —— 引擎层干净, 编排层在腐化。**
lcu 侧四引擎(match_context/counter/tips/war/advisor)都是纯函数模块, 职责单一可测,
这部分是好的。腐化集中在两个地方:
1. **main.rs 编排层**: 同一 session 事件最多解析 4 次 ChampSelectSnapshot;
   compute_counter_plan / compute_opponent_intel / compute_war_text 三个 helper
   各重复一遍 local→lane_opponent 解析, 加上 advisor::build_counter_note 内一处,
   "对位"这个概念没有单一计算点——改 lane 判定要动 4 处。
2. **双渲染器分叉**: advisor 里 LLM prompt 与 UI panel 两套同源渲染器(objectives/
   玩家行/击杀流水), 已经出现用词分叉("小龙" vs "龙")。

**充分性: 4/10 —— 核心链路在默认配置下不闭合。**
架构图纸是充分的(数据→引擎→prompt→UI 的层序没问题), 但三个 blocker 让
"我对位怎么打"在真实局内回答不了或回答错误。

## 二、Blocker(核实过证据)

| # | 位置 | 问题 | 证据 |
| --- | --- | --- | --- |
| B1 | counter.rs:210-212 + counter-runes.default.toml | **防御碎片 id 5002(护甲)/5003(魔抗)已被 Riot 移除, 但每条规则仍在引用**; LCU 应用时大概率拒收 | data/champr.db 中 344 页真实符文 slot8 分布: {5001×248, 5011×93, 5013×3}; 5002/5003 = 0 次。旁证: rune_name 表 5011/5013 名称疑似标反(5013 才是韧性减速抗性) |
| B2 | advisor.rs:1514-1516 build_gameflow_prompt | gameflow 阶段 teamOne 无条件标"我方"——约一半对局敌我标反, 且错误事实会被 TTS 播报 | 代码直读; LCU gameflow session 不保证 teamOne = 本机队 |
| B3 | tts.rs:107-120 | sidecar spawn 即 Ok: msedge-tts 未装/失败静默全哑, 0 字节也不回退; 锁也只持有到 spawn, 多段语音实际并发叠放 | 代码直读 |

次严重 but 已核实:
- **默认配置链路断裂(should-fix 但它最像 blocker):** opgg_sections_cache 的唯一写入点
  ensure_opgg_sections(main.rs:1661)只被 LLM prompt 路径(1770/1816)调用;
  LLM 辅助默认关 → counter 卡永不出现、war 战术恒"未知"档、自动应用静默退主流页,
  UI 占位文案还在说"数据充足后自动给出方案"。即: 本地引擎能力被错误地后置依赖在
  LLM 开关上。
- war.rs `tactics_for` 用"走为上", STRATAGEM_NAMES 收的是"走为上计"——自家校验表
  绕过了自己, 测试还把错名钉成了期望值(我把这个留给自己负责, 本会话引入)。

## 三、模块评分总表

| 模块 | 简要 | 充分 | 主要缺口 |
| --- | --- | --- | --- |
| match_context.rs | ★★★★ | ★★☆ | 龙/turret 未知队伍归属策略不一致(四种事件三套语义); position="NONE" 会配成随机敌人; local_player 前缀碰撞会全链敌我对调; ARAM/盲选对位静默消失 |
| counter.rs | ★★★★☆ | ★★☆ | B1 碎片已死; win_rate 坏字符串回落 50.0 触发 Even 规则(违背"数据不足不出方案"); pressure_of/plan_for 重复 lookup; 分路段与 counters 段不核对 |
| tips.rs | ★★★★★ | ★★★★ | 最干净的模块; blurb 死字段; 版本拉取第三次重写 |
| war.rs | ★★★★ | ★★★☆ | mains[1] 死数据(承诺双主计只渲染一个); 用户覆盖是整体替换, 空文件静默吞 173 张卡; id 无真伪校验; 与 tips 在 prompt 中心理解读语义交叠 |
| advisor.rs | ★★★☆ | ★★★ | B2; system prompt 尾部包尾协议自相矛盾且零执行; live 不注入 OP.GG 推荐符文(与 system 第 2 步要求断层); "按英文名找英雄"谓词单函数写 3 遍 |
| main.rs 编排 | ★★☆ | ★★★☆ | 4× 快照解析; 三处会话重置清单不一致; extract_* 与 snapshot 双套"当前英雄"语义; EIP 级: 锁定 counter 后对手换人就静默不同步 |
| UI/TTS | ★★★★ | ★★★ | B3; 三卡无高度上限挤压符文列表; 对线期 UI 完全没有"怎么打"答案(只有 LLM 腿) |

## 四、推荐的修复次序(按影响/成本)

**Wave 1 — 正确性(当天):**
1. 修 B1: 重写碎片白名单与默认 TOML shard(slot8 ∈ {5001,5011,5013}), 对换 5011/5013 名称, 加真实数据回归测试; 同步 counter-rune-system.md
2. 修 B2: gameflow 用 current-summoner 定边, 匹配不到就中性"阵容A/B"
3. 修 B3: sidecar 等退出码 + 失败回退链; 或至少失败时 warn + 回退
4. 修链路断裂: fetch_and_show_runes 成功时回写 opgg_sections_cache(一处 insert)
5. 修 war: "走为上"→"走为上计"; mains 双计都渲染; 用户覆盖改按-id 合并语义
6. war TOML 头部注释与测试语义对齐(reason 必填的表述)

**Wave 2 — 数据完整性:**
7. match_context 四种事件未知队伍归属统一(龙不再默认记 ORDER)
8. position "NONE"/空统一判空; local_player 前缀匹配加 tag 一致性门槛
9. counter: win_rate 坏值 → Pressure::Unknown; find_matchup 抽公共; section 与 counters 分路核对
10. counter 对手换人重应用提示; plan 摘要格式化收进 RunePlan::describe()

**Wave 3 — 架构收敛(降腐化):**
11. main.rs 每个 session 事件只解析一次 ChampSelectSnapshot, compute_* 收 &Snapshot
12. prompt 渲染与 UI panel 渲染合一(带 compact/label 参数)
13. system prompt 包尾协议改"中文自然行文+禁 markdown", 在拼接/TTS 单点加 sanitize
14. live prompt 注入 OP.GG 推荐符文(或 system 按阶段条件化步骤)

**Wave 4 — 排障可见性(所有"静默"类):**
15. ensure_opgg_sections 失败 warn; counter_plan 空值成因枚举化进 UI 文案
16. live 期 UI 补"战术一行"(war tactics 沉淀到 state 随 phase 拼进 live panel)
17. test-gap 补清单(见各模块 findings, main.rs 五个纯函数 + 三个引擎边界用例)

## 五、设计事实记录(本次新校准出来的真相, 写进知识库)

- 现行符文第三行(防御碎片)合法 id: **5001(成长生命)/5011(+65生命)/5013(韧性及减速抗性)**;
  护甲/魔抗碎片 5002/5003 已于 2023 年中版本被 Riot 移除。来源: champr.db 344 页统计。
- LCU gameflow session 的 teamOne 不承诺是本机队伍, 必须用 /lol-summoner/v1/current-summoner 定边。
- 本仓所有"默认关闭的 LLM 辅助"会连带饿死本地确定性引擎(counter/war 依赖的 sections 缓存),
  这是当前最大的产品级架构缺陷: 本地引擎不应该被网络服务开关控制。
