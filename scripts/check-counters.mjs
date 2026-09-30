// 快速验证 server 数据库里 counters 数据是否真实存在(counter 符文引擎的燃料)。
// 用法: node scripts/check-counters.mjs  (python 走 DSH_PYTHON 或系统 python)
import { writeFileSync, rmSync, existsSync } from 'node:fs';
import { execSync } from 'node:child_process';
import path from 'node:path';

const dbFile = path.resolve('data/champr.db');
if (!existsSync(dbFile)) {
  console.error('NO_DB', dbFile);
  process.exit(1);
}
console.log('db =', dbFile);

const code = `
import sqlite3, json
con = sqlite3.connect(${JSON.stringify(dbFile)})
n = con.execute("select count(*) from champion_data").fetchone()[0]
have, total_matchups, worst, best = 0, 0, None, None
for (alias, p) in con.execute("select champion_alias, payload from champion_data"):
    try:
        sections = json.loads(p)
    except Exception:
        continue
    cnt = 0
    for s in sections:
        cc = s.get("counters")
        if isinstance(cc, dict):
            cnt += len(cc.get("matchups", []))
        elif isinstance(cc, list):
            cnt += sum(len(x.get("matchups", [])) for x in cc)
    total_matchups += cnt
    if cnt > 0:
        have += 1
        if worst is None or cnt < worst[1]: worst = (alias, cnt)
        if best is None or cnt > best[1]: best = (alias, cnt)
print(f"champions={n} with_counters={have} total_matchups={total_matchups}")
if worst: print("min:", worst, " max:", best)
`;

const script = path.resolve('scripts/_check_counters.py');
writeFileSync(script, code, 'utf8');
try {
  const py = process.env.DSH_PYTHON || 'python';
  process.stdout.write(execSync(`"${py}" "${script}"`, { encoding: 'utf8' }));
} finally {
  rmSync(script, { force: true });
}
