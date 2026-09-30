// 提取待起草英雄的紧凑上下文(gen-war-cards 的辅助件, 与 --import 路径配套)
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const data = JSON.parse(readFileSync(resolve(ROOT, 'output/war-draft/championFull.json'), 'utf8')).data;
const toml = readFileSync(resolve(ROOT, 'crates/lcu/data/war_strategy.default.toml'), 'utf8');
const taken = new Set([...toml.matchAll(/id\s*=\s*"([^"]+)"/g)].map((m) => m[1].toLowerCase()));
const list = Object.values(data)
  .filter((c) => !taken.has(c.id.toLowerCase()))
  .map((c) => ({
    id: c.id.toLowerCase(),
    zh: c.name,
    title: c.title,
    tags: c.tags,
    allytips: (c.allytips || []).slice(0, 3),
    enemytips: (c.enemytips || []).slice(0, 3),
  }));
writeFileSync(resolve(ROOT, 'output/war-draft/champs-todo.json'), JSON.stringify(list));

// 会话起草模式: 拆批落盘, 每批一个 pretty JSON(一行一英雄),
// 供并行子任务直接读文件起草, 主链路不经过巨型上下文。
import { mkdirSync } from 'node:fs';
const PART_DIR = resolve(ROOT, 'output/war-draft/batches');
mkdirSync(PART_DIR, { recursive: true });
const PER = 9;
const parts = [];
for (let i = 0; i < list.length; i += PER) {
  const name = `batch-${String(parts.length + 1).padStart(2, '0')}.json`;
  writeFileSync(
    resolve(PART_DIR, name),
    list.slice(i, i + PER).map((c) => JSON.stringify(c, null, 2)).join('\n')
  );
  parts.push(name);
}
console.log(`champs to draft: ${list.length}, batches: ${parts.length} (${PER}/批) -> ${PART_DIR}`);
console.log(parts.join('\n'));
