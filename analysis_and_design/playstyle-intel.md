# 对位心理图谱(playstyle intel)

> 回答"对面这个英雄在想什么"——对应[对位心理四层模型](#四层模型)。
> 与 [counter-rune-system.md](counter-rune-system.md) 配系: counter 回答"符文怎么换",
> tips 回答"人怎么读"。

## 四层模型(机制理解)

| 层 | 问题 | 钉在数据上的证据 |
| --- | --- | --- |
| **意图** | 他想干什么 | ally_tips(他会本能性怎打) |
| **发力窗口** | 他何时敢于出手 | 等级质变(2/6)+CD窗(Phase2 技能CD采集) |
| **欺骗** | 他的假动作 | ally_tips 里的拉扯/位移话术 |
| **崩溃** | 什么打击他心态 | enemytips(官方逐条弱点清单) |

## Phases

### Phase 1(已实现): DDragon 官方心理字段采集

- 数据源: `championFull.json`(zh_CN, 单请求全英雄)拿 allytips/enemytips/blurb/title
- 模块: `crates/lcu/src/tips.rs` — `PlaystyleAtlas`(key = 数字 key)+两个消费口径:
  - `render_opponent` —— UI 卡(标题/正文两段, 条目数封顶, 防猾屏拥挤)
  - `render_for_prompt` —— LLM 注入(意图/软肋标签语)
- 管线: app 启动 task 一次拉取 → AppState.playbook → 失败回退空图(心理卡 / prompt 段落自动消失, 不阻塞)
- 不乱码路径: 复用现有 ddragon 版本检测; 解析走 `serde_json::from_str`, 测试数据全中文进断言

### 符文窗卡(选人阶段)

在深金 counter 卡下方立卡:
```
对位心理 · 影流之主 劫
他想 1 「劫依赖三级连招起手…」
打他 1 「劫的影分身后留有走位空当…」
```
- 触发条件跟 counter 符文卡同源(对位英雄已确定), 但**不依赖样本数**——
  样本不足的冷门对位至少还有心理可读。
- slate: `opponent-header` / `opponent-intel` 两个 prop, 空串自动隐卡

### Phase 2(记录未做): LLM 端口按英雄心理卡片

- 模板化 DeepSeek 生成每英雄: 发力期/连招习惯/明确心态崩点, 缓存 SQLite;
  入口跟 adivsor prompt 合并, UI 持"详细情报分流"进一步展开
- 和默认 atlas 区别: 语气更共用、含数值注入(技能CD), 做之前先在 tips.rs 加
  `render_for_prompt_detailed` 避免并发结构扭曲

### Phase 3(设想): 技能 CD 窗口感知

- champion/{Id}.json 拉 spells[].cooldownNum —— 互动期闪光信号:
  "亚索W盾墙 26s CD, 他刚交 → 有 20s 窗口"; 上色加粗进 live panel
