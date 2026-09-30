# Counter 符文体系设计

> 对应实现: `crates/lcu/src/counter.rs`、`crates/lcu/data/counter_runes.default.toml`、
> `crates/lcu/data/rune_lattice.json`, 消费侧在 `crates/lcu/src/advisor.rs` 与
> `crates/app/src/main.rs`(状态/按钮/自动应用)。

## 目标

把"对位针对性符文"从 LLM 推理中剥离, 成为一份**本地确定性规则引擎**:

- 每一次已知对位(对手已锁定 + 对位样本充足)都能确定性地产出一个符文方案
- 方案与英雄主流两页(LOL 侧记录的最优页)完全一样时不重复推荐
- 不一样时, 每处改动都有"原→新"的逐格差异与取舍理由, 用户可以复核
- 规则是**用户可编辑的配置**(TOML), 不是写死在代码里

## 为什么不靠 LLM

| 维度 | LLM 方案 | 本地规则(本设计) |
| --- | --- | --- |
| 一致性 | 每次可能不一样 | 同输入必同输出 |
| 延迟/成本 | 需一次请求 | 零请求, 微秒级 |
| 合法性 | 可能编出不存在的符文/树组合 | 写盘前已由格子图校验 |
| 可审计 | 黑盒 | TOML + tests, 每条规则可 review |
| 可信度 | 取决于模型对局的记忆 | 取决于你写规则的人理解多深 |

LLM 在这里退化为"解释者": prompt 里注入规则引擎的结论与理由, 由它向用户讲清楚,
而不是让它做决策。

## 输入

| 输入 | 来源 | 字段 |
| --- | --- | --- |
| 对位胜率/场次 | OP.GG counters 爬取 import 到 server, `BuildSection.counters` | `win_rate`, `play` |
| 敌方英雄画像 | Data Dragon champion.json | `tags`(Assassin/Tank/...), `info.attack/magic` |
| 基页 | 本地服务器 OP.GG 最优符文页 (`section.runes`, 本路第一页) | 9 perk + 双树 id |

压力分级(由胜率与场次推出):

| 档 | 条件 | key |
| --- | --- | --- |
| 大劣势 | win_rate < 45% | `hard_lose` |
| 劣势 | 45% ~ 48.5% | `lose` |
| 均势 | 48.5% ~ 51.5% | `even` |
| 优势 | > 51.5% | `win` |
| 未知 | 对位场次 < 25 (`SAMPLE_MIN`) / 无对位记录 | `unknown`(不触发有档规则) |

伤害类型启发式: `magic >= attack + 3` → AP; `attack >= magic + 3` → AD; 其余 → 混合。

## 规则 schema(TOML)

```toml
[[rule]]
name = "抗压刺客爆发"                     # 仅用于日志 / 排错
priority = 90                              # 越大越优先命中
pressure = ["hard_lose", "lose"]           # any-of; 省略 = 不限
opponent_tags = ["Assassin"]               # any-of; 省略 = 不限
opponent_damage = ["ad"]                   # any-of; 省略 = 不限
explain = "敌方刺客爆发高: ..."            # 写到 UI/prompt 里的取舍理由

[rule.set]
sub_tree = 8400                            # 副系改坚决(可选)
sub_runes = [8473, 8451]                   # 两枚不同行 副系符文(可选, 与 sub_tree 成对)
[rule.set.shard]                           # 防御碎片随敌方伤害类型(可选)
ad = 5002                                  # 敌方 AD → 护甲
ap = 5003                                  # 敌方 AP → 魔抗
default = 5001                             # 混合/未知 → 成长生命
```

### 匹配与生效语义

1. 所有字段均为 any-of; 不写的要求不生效。四条全部单独满足才算命中。
2. 命中规则按 `priority` 从大到小排序。
3. **首条带 `sub_tree` 的规则**决定副系两位; 后续规则的 `sub_*` 不再应用。
4. **首条带 `shard` 的规则**决定防御碎片。
5. `explain` 最多保留前 2 条含文本的命中规则, 合并写入 UI。

### 合法性校验(装载时)

任何一条不满足会被跳过并以 warning 记录, 不会拖垮其他规则:

- `sub_tree` 必须是 8000/8100/8200/8300/8400 之一
- `sub_runes` 恰好 2 枚, 均属于该树, 均不是基石(row 0), 且不在同一行
- `shard` 三枚 id ∈ {5001, 5002, 5003, 5011, 5013}(防御槽可选范围)
- `pressure` / `opponent_damage` 在已知枚举里

校验基于内置 `crates/lcu/data/rune_lattice.json`(DDragon 16.19.1 快照:
id → tree/row/中文名 62 枚)。

### 规则文件装载顺序

```text
%APPDATA%\champr\counter-runes.toml  --存在--> 用户版
       | 不存在
       v
crates/lcu/data/counter_runes.default.toml (include_str 编译进二进制)

解析失败(TOML 语法错误) → 自动回退内置默认 + 记 warning,
不会把系统搞成"零规则"。
```

## 方案生成

```rust
CounterSystem::plan(base, opponent_profile, pressure) -> Option<RunePlan>
```

- 保留基页主系基石 + 主系三格 + 前两个碎片位(这些是英雄命理, 不是 counter 件)
- 只重写: 副系树/两枚(索引 4/5)、防御碎片(索引 8)
- 完全无改动(传入 base 已等于目标) → None

### RunePlan 内含

| 字段 | 说明 |
| --- | --- |
| `selected_perk_ids` | 9 枚 perk id, LCU 写入格式 |
| `line` | 一句话摘要: "副系坚决: 骸骨镀层+过度生长, 防御碎片: 护甲" |
| `diffs` | 逐格差异: ["副系第1格: 饼干配送→骸骨镀层", "防御碎片: 成长生命→护甲"] |
| `reasons` | 最多 2 条命中规则的 `explain` |
| `base` | 原始基页, 用于构造 apply 用 Rune(复用 alias/position 元数据) |

### 去重 (`plan_for_matchup`)

生成后与**主流两页**逐一比对(9 perk + 主副树全等); 完全相同 → None。
这就是"重复不显示"的实现位置 —— 不进 UI, 不进 prompt, 不出自动应用。

## 系统的四条出口

| 场景 | 消费方式 |
| --- | --- |
| 选人 prompt (advisor::build_lineup_prompt) | 注入 `符文针对(...): {line} [{diffs}] — {reason}` |
| 选人面板 (advisor::build_champ_select_panel_text) | 同上, 与 Ban 推荐同区 |
| 符文窗口按钮 "应用Counter符文" | `plan.to_rune_page()` → `lcu_api::apply_rune` 写入 |
| 锁后自动符文 | 有 plan 时用 counter 页; 无 plan 回退 `apply_best_rune_for_position` |

## 一套典型输出

对位 Aatrox 上(我方) vs Zed 上(敌方, 刺客/物理, 大数据 46.47% / 340 场 → 劣势):

```text
符文针对(counter规则, 敌方刺客·物理,对位劣势): 副系坚决: 骸骨镀层+过度生长, 防御碎片: 护甲
[副系第1格: 饼干配送→骸骨镀层; 副系第2格: 星界洞悉→过度生长; 防御碎片: 成长生命→护甲]
— 敌方刺客爆发高: 副系换坚决, 骸骨镀层顶第一波, 过度生长叠血量
```

(如果 Aatrox 的 OP.GG 第二页本来就是这一套, 上面这段不会出现 —— 与主流页
重合就不额外推荐。)

## 用户怎么自定义规则

1. 复制 `crates/lcu/data/counter_runes.default.toml` 到 `%APPDATA%\champr\counter-runes.toml`
2. 直接编辑, 无需重启开发服务器 —— 重启应用即可生效(装载是进程生命周期一次)
3. 引用不存在的符文 id、同树同行的组合、未知 pressure 值都会被静默跳过,
   warning 记到日志 —— 出错不会崩, 但拿不准就先跑 `cargo test -p lcu counter` 校验思路

## 测试矩阵(`lcu/src/counter.rs` 10 测 + `advisor.rs` 2 个端到端)

| 用例 | 覆盖 |
| --- | --- |
| `lattice_covers_default_rules` | 默认 TOML 解析 + 全部 id 合法 + 无 warning |
| `assassin_pressure_loses_swap_to_resolve` | 劣势 + 刺客 → 副系坚决 骸骨+过度生长, 防御碎片护甲 |
| `mage_pressure_prefers_second_wind` | 大劣 + 法师 → 复苏之风, 防御碎片魔抗 |
| `even_matchup_tunes_shard_only` | 均势 → 副系保持, 只调防御碎片 |
| (同上)Unknown | 样本不足不推 |
| `win_keeps_sub_and_tunes_shard` | 优势 → 副系保持 |
| `plan_diffs_describe_each_slot_tradeoff` | diffs 的槽位名/旧→新顺序 |
| `invalid_rule_is_dropped_with_warning` | 同行副系规则被校验拒绝 |
| `override_text_replaces_embedded_rules` | 覆盖文件生效 + 坏文件回退内置 |
| `pressure_thresholds` | 分级边界(+样本下限进 Unknown) |
| `damage_classification_via_info` | AD/AP/混合分类 |
| `counter_note_describes_rule_engine_plan` (advisor) | tags/伤害/压力/diffs 全部进 note 文本 |
| `counter_note_skips_when_mainstream_page_already_matches` | 去重路径 |

## 边界与已知取舍

- **样本下限 25 局写死在 Rust 端**(`SAMPLE_MIN`), 不进规则可配 —— 这是防噪,
  不是规则作者该反复改的东西; 要改就要改代码。
- **shard id 白名单写死**(`5001/5002/5003/5011/5013`): 防御槽位是客户端固定 5 选,
  不是树结构, 用格子图校验不了 —— 干脆用 curated 列表。
- **副系两枚**: 与客户端限制一致, 规则不许塞进三枚 —— 这本身就是强制你聚焦的方式。
- **均势规则只 shard 不动副系**: 均势没有足够强的信号值得动副系。若你说"均势也要换",
  自己加一条 `pressure = ["even"]` 且带 `sub_tree` 的高优先级规则即可, 内置并不拦。
- **主系基石从不动**: 英雄主流符文/版天梯答案已经很成熟; counter 关心"怎么活过对线"
  和"怎么利用已经拿到的优势", 都在副系和防御碎片解决。
- **Unknown 档不推**: 盲选、对位数据缺失、样本不足 —— 这些场景迷信 adjusts 比不调更危险。

## 后续可以扩的方向

| 方向 | 判断 |
| --- | --- |
| 规则里允许改主系某一行(如劣势换主系"过度生长") | 等真有一个 chamber 场景跑不出效果再加 |
| 规则条件加"己方 tags"(`self_tags`) | 字段/schema 均未实现, 当前规则只看对手; 需要时再加 |
| Ban 位也由规则引擎统一管理(现是 advisor 里简单过滤) | 不在本模块里, 但思路相通 |
| 胜率阈值外移到 TOML 顶层 `[defaults]` | 若用户反馈硬 |
