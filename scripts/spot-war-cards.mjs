// 抽查导入卡的渲染(VS pwsh 转义地狱的一次性工位)
import { readFileSync } from 'node:fs';
const cards = JSON.parse(readFileSync('output/war-draft/llm-cards.json', 'utf8'));
const want = ['viego', 'yuumi', 'zeri', 'twistedfate', 'masteryi', 'sylas', 'gwen', 'briar'];
for (const id of want) {
  const c = cards.find((x) => x.id === id);
  if (!c) {
    console.log(`=== ${id} MISSING ===\n`);
    continue;
  }
  console.log(`=== ${id} || ${c.stratagem} ===\n  combo: ${c.combo}\n  psych: ${c.psych}\n  usage: ${c.usage}\n  reason: ${c.reason}\n`);
}
