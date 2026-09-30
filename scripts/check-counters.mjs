// 快速验证 server 数据库里 counters 数据是否真实存在(counter 符文引擎的燃料)。
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { execSync } from 'node:child_process';
import path from 'node:path';

const candidates = ['data/champr.db', 'crates/server/data/champr.db'];
const dbFile = candidates.find(existsSync);
if (!dbFile) {
  console.log('NO_DB', candidates);
  process.exit(1);
}
console.log('db =', dbFile);
// 用 Python 的 sqlite3(node 侧干净避免 better-sqlite3 依赖)
const code = `
import sqlite3, json, sys
con = sqlite3.connect(${JSON.stringify(path.resolve(dbFile))})
rows = con.execute("select name from sqlite_master where type='table'").fetchall()
print('tables:', rows)
total = 0
with_counters = 0
for (champ,) in con.execute("select champion from builds"):
    try:
        sections = json.loads(con.execute("select payload from builds where champion=?", (champ,)).fetchone()[0])
    except Exception:
        continue
    total += 1
    if any(s.get('counters') for s in sections):
        with_counters += 1
print(f'champions_with_payload={total} with_counters={with_counters}')
`;
const script = path.resolve('scripts/_check_counters.py');
import { writeFileSync } from 'node:fs';
writeFileSync(script, code, 'utf8');
const py = process.env.DSH_PYTHON || 'python';
console.log(execSync(`${py} ${script}`).toString());
