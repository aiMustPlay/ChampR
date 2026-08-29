# 分钟级对战播报设计

## 1. 目标

ChampR 在英雄选择阶段读取双方阵容，并在对局/选人阶段每分钟基于阵容、本局玩家位置和可用装备信息，通过 DeepSeek 生成中文战斗建议，最后通过 Windows TTS 语音播报。

建议内容重点：

- 对线战斗技巧
- 当前英雄装备思路
- 控龙策略
- 控虫策略

## 2. 核心数据流

```text
LCU champ-select session
        |
        v
读取双方阵容 / 本局玩家
        |
        v
补充当前英雄推荐装备标题
        |
        v
组装 DeepSeek prompt
        |
        v
DeepSeek chat/completions
        |
        v
主窗口显示建议
        |
        v
Windows SAPI TTS 语音播报
```

## 3. 模块划分

| 模块 | 文件 | 职责 |
| --- | --- | --- |
| DeepSeek 客户端 | `crates/lcu/src/deepseek.rs` | API Key/Base URL/Model 配置、chat 请求、错误处理 |
| 阵容解析与 prompt 组装 | `crates/lcu/src/advisor.rs` | 解析 `myTeam`、`theirTeam`、本局玩家，生成中文 prompt |
| LCU 数据读取 | `crates/lcu/src/lcu_api.rs` | 读取完整 champ-select session |
| Windows TTS | `crates/lcu/src/tts.rs` | 通过 PowerShell 调用 System.Speech 播报文本 |
| 分钟级调度 | `crates/app/src/main.rs` | `advice_loop` 每 60 秒调用一次完整链路 |
| UI 展示 | `crates/app/ui/app.slint` | 主窗口显示建议文本 |

## 4. 当前实现状态

### 4.1 已实现

- [x] DeepSeek 客户端
- [x] 读取 champ-select session
- [x] 解析我方阵容、敌方阵容
- [x] 识别本局玩家位置
- [x] 组装中文 prompt
- [x] 每 60 秒触发一次建议生成
- [x] champ-select 结束后保留最近一次阵容上下文继续播报
- [x] 优先读取游戏内 League Live Client Data 实时阵容、等级和装备 ID
- [x] Windows TTS 语速、音量、声音角色通过环境变量配置
- [x] 主窗口显示建议文本
- [x] DeepSeek 返回后自动 Windows TTS 播报
- [x] 通过环境变量配置 DeepSeek
- [x] 没有 API Key 时给出提示，不影响客户端启动

### 4.2 部分实现

- [x] 装备信息：已读取游戏内实时装备 ID，并通过 Data Dragon 中文装备名映射
- [ ] 阵容信息来源：游戏内已接入 Live Client Data，缺少本地缓存/英雄名兜底时可能退化
- [ ] 建议更新条件：已优先使用实时对局数据，但需要完善 API 不可用时的平滑降级
- [ ] 英雄名称映射：依赖 Data Dragon 的 `ChampionsMap`；若 Data Dragon 拉取失败，prompt 中会退化为数字 champion id
- [ ] 输出长度：prompt 要求 250 字以内，但没有硬性截断或后处理校验
- [ ] 首次播报：当前从启动后 60 秒开始，不会在进入选人瞬间立即播报
- [ ] DeepSeek 错误重试：请求失败只显示错误，没有自动重试

### 4.3 未实现

- [x] 游戏内实时阵容
- [x] 游戏内实时装备 ID / 等级
- [ ] 控龙/控虫计时器
- [ ] 手动重播最近一条建议
- [ ] 设置界面配置 API Key
- [x] TTS 参数设置界面（语速、音量、声音角色）
- [ ] DeepSeek streaming 输出
- [ ] 请求频率限制/冷却
- [ ] 建议历史记录
- [ ] 单元测试覆盖 DeepSeek mock、prompt 边界、TTS 错误处理

## 5. 待补齐优先级

### P0：让播报在实际对局中可用

1. 接入 LCU 实时对局数据接口，获取游戏内双方阵容、装备、等级。
2. 将 `advice_loop` 从只依赖 champ-select session 扩展到游戏阶段。
3. 增加首次进入选人/对局时立即生成建议，然后每分钟刷新。

### P1：提高建议质量

4. 使用真实装备信息，而不是推荐装备标题。
5. 增加英雄名映射的本地兜底，避免 Data Dragon 失败时只显示数字。
6. 对 DeepSeek 返回文本做长度和后处理校验。

### P2：产品化

7. 增加语音语速、音量、角色设置。
8. 增加“重播建议”按钮。
9. 在设置界面配置 DeepSeek API Key。
10. 增加请求失败重试和冷却。

## 6. 验证方式

- 设置 `DEEPSEEK_API_KEY` 后启动客户端。
- 进入英雄选择界面，等待 60 秒，检查主窗口是否出现建议。
- 确认建议包含双方阵容、本局玩家、对线和控龙/控虫策略。
- 确认建议内容通过 Windows TTS 播放。
- 断开 API Key，确认客户端仍能启动并显示配置提示。
