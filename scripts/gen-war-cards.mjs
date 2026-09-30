// =============================================================================
// 兵法心战卡批量生成器(LLM 起草 + 校验 + 人工 review 报告)
//
// 流程:
//   1. 读 .env 的 DEEPSEEK_API_KEY(仓库 .gitignore 已排除).
//   2. 拉 DDragon 最新 championFull.json(zh_CN), 得到全英雄 id/tags/tips.
//   3. 排除 crates/lcu/data/war_strategy.default.toml 已收录的人工卡(以 id= 匹配).
//   4. 每 6 个英雄一批交给 DeepSeek, 要求严格 JSON 数组输出:
//      { id, stratagem, combo, psych, usage, reason }
//   5. 校验: stratagem 必须在三十六计全表; 4 个文本字段非空; JSON 严格解析.
//      失败的卡进入 rejected.json, 不写入草稿.
//   6. 产物(output/war-draft/):
//      - war-strategy.generated.toml  — 合并草稿
//      - report.md                    — 人工 review 用(全卡列表 + 失败原因 + 随机样例)
//      - generated.jsonl / rejected.json — 审计线索
//   7. `node scripts/gen-war-cards.mjs --merge` 在 review 后把草稿追加进内置 TOML
//      (重复 id 仍然跳过), 随后 `cargo test -p lcu --lib war::` 由其单测二次把关.
//
// 用法:
//   node scripts/gen-war-cards.mjs            # 全量生成(不合并)
//   node scripts/gen-war-cards.mjs --limit 6  # 试跑 6 个英雄
//   node scripts/gen-war-cards.mjs --merge    # review 后合并进内置文件
//   node scripts/gen-war-cards.mjs --mock     # 无 API 时验证管线(假卡)
//   node scripts/gen-war-cards.mjs --import <cards.json>
//      # 会话内 LLM 起草模式: 给一份已成形 JSON(数组或 {cards:[...]}),
//      # 脚本仍走同一批校验, 产出同样的草稿/报告; 再 --merge 合并。
// =============================================================================

import { readFileSync, writeFileSync, mkdirSync, existsSync, appendFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT_DIR = resolve(ROOT, 'output/war-draft');
const DEFAULT_TOML = resolve(ROOT, 'crates/lcu/data/war_strategy.default.toml');
const BATCH_SIZE = 6;
const MAX_RETRIES = 3;
const PACE_MS = 400;

// 三十六计正名表: 与 crates/lcu/src/war.rs 保持同步的唯一合法值。
const STRATAGEMS = [
  '瞒天过海', '围魏救赵', '借刀杀人', '以逸待劳', '趁火打劫', '声东击西',
  '无中生有', '暗渡陈仓', '隔岸观火', '笑里藏刀', '李代桃僵', '顺手牵羊',
  '打草惊蛇', '借尸还魂', '调虎离山', '欲擒故纵', '抛砖引玉', '擒贼擒王',
  '釜底抽薪', '混水摸鱼', '金蝉脱壳', '关门捉贼', '远交近攻', '假道伐虢',
  '偷梁换柱', '指桑骂槐', '假痴不癫', '上屋抽梯', '树上开花', '反客为主',
  '美人计', '空城计', '反间计', '苦肉计', '连环计', '走为上计',
];

function parseArgs(argv) {
  const args = { limit: Infinity, merge: false, mock: false, import: null };
  for (let i = 2; i < argv.length; i++) {
    if (argv[i] === '--merge') args.merge = true;
    else if (argv[i] === '--mock') args.mock = true;
    else if (argv[i] === '--limit') args.limit = Number(argv[++i]);
    else if (argv[i] === '--import') args.import = resolve(argv[++i]);
  }
  return args;
}

function loadEnv() {
  const envPath = resolve(ROOT, '.env');
  const env = { ...process.env };
  if (existsSync(envPath)) {
    for (const line of readFileSync(envPath, 'utf8').split(/\r?\n/)) {
      const m = line.match(/^\s*([A-Z0-9_]+)\s*=\s*(.*)\s*$/);
      if (m && !env[m[1]]) env[m[1]] = m[2].replace(/^["']|["']$/g, '');
    }
  }
  return env;
}

async function fetchJson(url, retries = 3) {
  let lastErr;
  for (let i = 0; i < retries; i++) {
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error(`HTTP ${res.status} ${url}`);
      return await res.json();
    } catch (err) {
      lastErr = err;
      await new Promise((r) => setTimeout(r, 1000 * (i + 1)));
    }
  }
  throw lastErr;
}

async function loadChampions() {
  mkdirSync(OUT_DIR, { recursive: true });
  const cache = resolve(OUT_DIR, 'championFull.json');
  let data;
  if (existsSync(cache)) {
    data = JSON.parse(readFileSync(cache, 'utf8'));
  } else {
    const versions = await fetchJson('https://ddragon.leagueoflegends.com/api/versions.json');
    const ver = versions[0];
    console.log(`[fetch] Data Dragon ${ver}`);
    data = await fetchJson(
      `https://ddragon.leagueoflegends.com/cdn/${ver}/data/zh_CN/championFull.json`
    );
    writeFileSync(cache, JSON.stringify(data));
  }
  return Object.values(data.data).map((c) => ({
    id: c.id.toLowerCase(),
    zh: c.name,
    title: c.title,
    tags: c.tags,
    allytips: (c.allytips || []).slice(0, 3),
    enemytips: (c.enemytips || []).slice(0, 3),
  }));
}

function existingIds() {
  const text = readFileSync(DEFAULT_TOML, 'utf8');
  return new Set([...text.matchAll(/^id\s*=\s*"([^"]+)"/gm)].map((m) => m[1].toLowerCase()));
}

const SYSTEM = `你是英雄联盟对线战术顾问兼三十六计编纂人。给定英雄元数据(id/中文名/定位tags/官方allytips与enemytips), 为每个英雄写一张"心战卡"。

每张卡的字段(全部简体中文, 每个字段一句话, 不堆词):
- id: 原样回填小写 id
- stratagem: 主计, 必须精确等于下列三十六计之一: ${STRATAGEMS.join('、')}
- combo: 连招签名。按"起手→铺陈→爆发/收割"顺序, 用技能键位+功效名, 间隔用→
- psych: 心理特点。操刀这名英雄的人在想什么、怕什么、什么时候急
- usage: 实战用法。对位它时, 用这句能直接操作
- reason: 收录理由。为什么是这一计, 衔接到该英雄的技能机制, 一两句话

硬规则:
- 只输出 JSON 数组, 不要 markdown 围栏, 不要任何额外文字
- stratagem 逐条核对在白名单内, 禁止自造
- 不同英雄尽量选贴合各自机制的计, 不要全部用"声东击西"
- 字段内不要出现英文双引号, 会坏掉 TOML`;

function mockCard(c) {
  return {
    id: c.id,
    stratagem: STRATAGEMS[Math.floor(Math.random() * STRATAGEMS.length)],
    combo: '__MOCK__ combo',
    psych: '__MOCK__ psych',
    usage: '__MOCK__ usage',
    reason: '__MOCK__ reason',
  };
}

function extractJsonArray(text) {
  const start = text.indexOf('[');
  const end = text.lastIndexOf(']');
  if (start < 0 || end <= start) throw new Error('响应中找不到 JSON 数组');
  return JSON.parse(text.slice(start, end + 1));
}

async function callDeepSeek(env, batch) {
  const apiKey = env.DEEPSEEK_API_KEY;
  if (!apiKey) throw new Error('DEEPSEEK_API_KEY 未设置(写进 .env 或环境变量)');
  const baseUrl = (env.DEEPSEEK_BASE_URL || 'https://api.deepseek.com/v1').replace(/\/$/, '');
  const model = env.DEEPSEEK_MODEL || 'deepseek-v4-flash';
  const user = batch
    .map(
      (c) =>
        `${JSON.stringify({
          id: c.id,
          zh: c.zh,
          title: c.title,
          tags: c.tags,
          allytips: c.allytips,
          enemytips: c.enemytips,
        })}`
    )
    .join('\n');
  const res = await fetch(`${baseUrl}/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${apiKey}` },
    body: JSON.stringify({
      model,
      temperature: 0.6,
      max_tokens: 4000,
      messages: [
        { role: 'system', content: SYSTEM },
        { role: 'user', content: `为以下${batch.length}个英雄各写一张心战卡:\n${user}` },
      ],
    }),
  });
  if (!res.ok) {
    const body = await res.text().catch(() => '');
    throw new Error(`DeepSeek HTTP ${res.status}: ${body.slice(0, 200)}`);
  }
  const data = await res.json();
  return extractJsonArray(data.choices?.[0]?.message?.content ?? '');
}

function validateCard(card, knownIds) {
  const errs = [];
  if (!knownIds.has(String(card.id || '').toLowerCase())) errs.push('id 不在批内清单');
  if (!STRATAGEMS.includes(card.stratagem)) errs.push(`stratagem 非法: ${card.stratagem}`);
  for (const f of ['combo', 'psych', 'usage', 'reason']) {
    if (typeof card[f] !== 'string' || card[f].trim().length < 8) errs.push(`${f} 过短/缺`);
  }
  return errs;
}

function tomlEscape(s) {
  return s.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
}

function toToml(card) {
  return `[[unit]]
id = "${card.id}"
stratagem = "${tomlEscape(card.stratagem)}"
combo = "${tomlEscape(card.combo)}"
psych = "${tomlEscape(card.psych)}"
usage = "${tomlEscape(card.usage)}"
reason = "${tomlEscape(card.reason)}"
`;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function emitOutputs({ accepted, rejected, batches, all, note }) {
  mkdirSync(OUT_DIR, { recursive: true });
  writeFileSync(
    resolve(OUT_DIR, 'generated.jsonl'),
    accepted.map((c) => JSON.stringify(c)).join('\n')
  );
  writeFileSync(resolve(OUT_DIR, 'rejected.json'), JSON.stringify(rejected, null, 2));
  writeFileSync(
    resolve(OUT_DIR, 'war-strategy.generated.toml'),
    `# 由 scripts/gen-war-cards.mjs 生成(${note}), review 后用 --merge 合并进内置文件\n\n` +
      accepted.map(toToml).join('\n')
  );

  const sample = [...accepted].sort(() => 0.5 - Math.random()).slice(0, 8);
  const zhOf = Object.fromEntries(all.map((c) => [c.id, c.zh]));
  const render = (c) =>
    `### ${zhOf[c.id] || c.id}(${c.id}) — ${c.stratagem}\n- 连招: ${c.combo}\n- 心理: ${c.psych}\n- 用法: ${c.usage}\n- 理由: ${c.reason}\n`;
  const report =
    `# 心战卡生成报告(${note})\n\n- 批次: ${batches}, 通过: ${accepted.length}, 驳回: ${rejected.length}\n\n` +
    `## 抽查样例(请重点核对)\n\n${sample.map(render).join('\n')}\n\n` +
    `## 全部通过卡\n\n${accepted.map(render).join('\n')}\n\n` +
    (rejected.length
      ? `## 驳回名单\n\n${rejected.map((r) => `- ${r.id}: ${r.reason}`).join('\n')}\n`
      : '');
  writeFileSync(resolve(OUT_DIR, 'report.md'), report);
  console.log(`[done] 草稿与报告 → ${OUT_DIR}`);
  console.log('review 完没问题再执行:  node scripts/gen-war-cards.mjs --merge');
}

async function main() {
  const args = parseArgs(process.argv);
  const env = loadEnv();

  if (args.merge) {
    const draftPath = resolve(OUT_DIR, 'war-strategy.generated.toml');
    if (!existsSync(draftPath)) throw new Error('草稿不存在, 先生成再合并');
    const taken = existingIds();
    const draft = readFileSync(draftPath, 'utf8');
    const blocks = draft.split(/\n(?=\[\[unit\]\])/).filter((b) => b.trim().startsWith('[[unit]]'));
    const cards = [];
    for (const block of blocks) {
      const id = (block.match(/^id\s*=\s*"([^"]+)"/m) || [])[1];
      if (id && !taken.has(id.toLowerCase())) cards.push(block.trim());
    }
    if (cards.length === 0) {
      console.log('没有可合并的新卡(全部已存在)');
      return;
    }
    appendFileSync(
      DEFAULT_TOML,
      `\n# --- LLM 起草区(由 scripts/gen-war-cards.mjs 生成并入, 涉嫌可疑条目删除即可) ---\n\n${cards.join('\n\n')}\n`
    );
    console.log(`已合并 ${cards.length} 张新卡 → ${DEFAULT_TOML}`);
    console.log('下一步: cargo test -p lcu --lib war::  由单测二次校验整表');
    return;
  }

  // —— 会话内 LLM 起草导入: 跳过 API, 只走同一批校验 ——
  if (args.import) {
    const all = await loadChampions();
    const taken = existingIds();
    const knownIds = new Set(all.map((c) => c.id));
    const raw = JSON.parse(readFileSync(args.import, 'utf8'));
    const importCards = Array.isArray(raw) ? raw : raw.cards;
    if (!Array.isArray(importCards)) throw new Error('--import 文件必须是数组或 {cards:[...]}');
    const accepted = [];
    const rejected = [];
    const seen = new Set();
    for (const card of importCards) {
      const id = String(card.id || '').toLowerCase();
      card.id = id;
      if (seen.has(id)) {
        rejected.push({ id, reason: '导入文件内重复 id' });
        continue;
      }
      seen.add(id);
      if (taken.has(id)) {
        rejected.push({ id, reason: '已有手写卡, 跳过(合并保护)' });
        continue;
      }
      const errs = validateCard(card, knownIds);
      if (errs.length) rejected.push({ id, reason: errs.join('; ') });
      else accepted.push(card);
    }
    console.log(`[import] 收到 ${importCards.length}, 通过 ${accepted.length}, 驳回 ${rejected.length}`);
    emitOutputs({ accepted, rejected, batches: '(import)', all, note: 'import 会话起草' });
    return;
  }

  const all = await loadChampions();
  const taken = existingIds();
  const todo = all.filter((c) => !taken.has(c.id)).slice(0, args.limit);
  console.log(`[plan] 全英雄 ${all.length}, 已有人工卡 ${taken.size}, 本次生成 ${todo.length}(批大小 ${BATCH_SIZE})`);
  if (todo.length === 0) return;

  const knownIds = new Set(todo.map((c) => c.id));
  const accepted = [];
  const rejected = [];
  let batches = 0;

  for (let i = 0; i < todo.length; i += BATCH_SIZE) {
    const batch = todo.slice(i, i + BATCH_SIZE);
    let cards = null;
    for (let attempt = 1; attempt <= MAX_RETRIES && !cards; attempt++) {
      try {
        cards = args.mock ? batch.map(mockCard) : await callDeepSeek(env, batch);
        if (!Array.isArray(cards) || cards.length === 0) {
          throw new Error('响应不是非空 JSON 数组');
        }
      } catch (err) {
        cards = null;
        console.warn(`[retry ${attempt}/${MAX_RETRIES}] batch ${batch.map((b) => b.id).join(',')}: ${err.message}`);
        await sleep(800 * attempt);
      }
    }
    batches++;
    if (!cards) {
      batch.forEach((c) => rejected.push({ id: c.id, reason: '批次三次重试仍失败' }));
      continue;
    }
    const gotIds = new Set();
    for (const card of cards) {
      const id = String(card.id || '').toLowerCase();
      if (gotIds.has(id)) continue;
      gotIds.add(id);
      const errs = validateCard(card, knownIds);
      if (errs.length) rejected.push({ id, reason: errs.join('; ') });
      else accepted.push(card);
    }
    for (const c of batch) {
      if (!gotIds.has(c.id)) rejected.push({ id: c.id, reason: '模型未返回该英雄' });
    }
    console.log(`[ok] ${i + batch.length}/${todo.length}  通过 ${accepted.length}  驳回 ${rejected.length}`);
    await sleep(PACE_MS);
  }

  emitOutputs({ accepted, rejected, batches, all, note: 'api 起草' });
}

main().catch((err) => {
  console.error(`失败: ${err.message}`);
  process.exit(1);
});
