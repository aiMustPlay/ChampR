# maohou(houmao 引擎)集成

> 决策: ChampR(houmao win)的 LLM 调用统一交给 `houmao-mac/engine` 的
> `maohou exec` 一次性命令, 两仓共享同一引擎实现。直连 reqwest 降为备用通道。

## 选型(为什么不是路径依赖/常驻服务)

| 方案 | 弃/取 | 理由 |
| --- | --- | --- |
| git 路径依赖(engine 作为 crate) | 弃 | 跨仓库构建耦合, 任一仓 mutex 摇动全崩; 引擎"二进制名/契约外不进组合档"明确 |
| 常驻 web 服务 + HTTP | 弃 | 9765 引擎自己是简易HTTPS+口令的单机用户件, ChampR 没加密钥轮换/会话持久这个层面的需求 |
| **`maohou exec` 子进程**(采纳) | 取 | 引擎配置模型即"一次性命令=启动参数即全部配置"; 进程即隔离, key 不走 cmdline 不落盘; 超时/退出码清晰 |

## 架构

```
advisor prompt → chat_with_selected_provider(main.rs)
  ├ deepseek_web(网页边角)       → BrowserSidecar(不动)
  ├ backend=maohou && bin 探测 ✓ → lcu::maohou::chat(bin, target, messages)
  │     spawn: maohou exec <prompt> --no-tools --base-url … --model … --api-key-env CHAMPR_ENGINE_KEY
  │     env: CHAMPR_ENGINE_KEY=<settings 里的 key>(不进命令行)
  │     超时 150s / kill_on_drop; 非零退出 bail(不双请求不双扣)
  └ 其他(bin 缺失)              → DeepSeekClient 直连(一次性警告进 ui_log)
```

二进制定位顺序: `settings.maohou_bin` → 环境变量 `MAOHOU_BIN` →
当前 exe 祖先下的 `houmao-mac/engine/target/release/maohou.exe` → PATH。

## 差集(引擎 OpenAI 方言暂不转发的旋钮)

DeepSeek 设置页里以下项在引擎模式不生效(UI 页有标注, 选择 direct 通道恢复):
- 思维链(DEEPSEEK_THINKING)
- 推理强度(DEEPSEEK_REASONING_EFFORT)
- 流式输出(stream 差异: 引擎 exec 无 --nonstream 但也不流式输出, 聚合后返回)

⚠ 需要这些参数时: 在 houmao-mac/engine 的 ai.rs openai_body 加转发
(它是协议职责, 按跨仓边界规则应在 Rust 侧补), 而不是在 ChampR 绕回直连。

## 引擎升级/版本对齐

两仓独立演进, 靠 `maohou --version` 探测避免意外复用; 文档中对齐检查项:
- [ ] houmao 侧 openai 方言加了参数转发后, 同步移除本仓差集说明
- [ ] houmao 侧 system prompt 若加长(现 ~2 行 agent persona), ChampR 注入的
      advisor system 调用一文 (compose_prompt) 不变, 引擎前置 system 总会在一起

## 测试

- `lcu/maohou.rs` 6 单测: 参数拼序/key 不下命令行/提示语拼接/空 prompt
- 端到端不做单元测试(要真 API key): 手动 `maohou exec 你好 --no-tools …` 烟验证
- `champr` 2 测(settings 默认 + monitors)随 ai_backend 字段验证 normalize 合法值域
