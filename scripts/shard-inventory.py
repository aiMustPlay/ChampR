# ============================================
# 校准验证: counter 引擎防御碎片白名单 vs 真实爬取数据
# 核实运行时 slot8/整体 plan 的 selectable perks 分布
# ============================================
import json
import sqlite3
import sys
from collections import Counter

conn = sqlite3.connect("data/champr.db")
cur = conn.cursor()
rows = cur.execute(
    "SELECT champion_id, mode, payload FROM champion_data"
).fetchall()
slot7 = Counter()
slot8 = Counter()
pages = 0
champs = set()
versions = set()
for cid, mode, payload in rows:
    doc = json.loads(payload)
    sections = doc if isinstance(doc, list) else doc.get("sections", [])
    for section in sections:
        for rune in section.get("runes", []):
            ids = rune.get("selectedPerkIds", [])
            if len(ids) >= 9:
                slot7[ids[7]] += 1
                slot8[ids[8]] += 1
                pages += 1
        if section.get("runes"):
            champs.add(cid)
print("pages:", pages, "champions:", len(champs), "versions:", sorted(versions))
print("slot7(灵活碎片):", dict(slot7.most_common()))
print("slot8(防御碎片):", dict(slot8.most_common()))
conn.close()
