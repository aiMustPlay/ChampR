import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, '..');

const outputDir = process.env.CHAMPR_OUTPUT_DIR
  ? path.resolve(process.env.CHAMPR_OUTPUT_DIR)
  : path.join(repoRoot, 'packages', 'opgg', 'output', 'latest');
const serverUrl = process.env.CHAMPR_SERVER_URL || 'http://127.0.0.1:3030';
const adminEmail = process.env.CHAMPR_ADMIN_EMAIL || 'admin@example.com';
const adminPassword = process.env.CHAMPR_ADMIN_PASSWORD || 'change-me-now';
const sourceKey = process.env.CHAMPR_SOURCE_KEY || 'op.gg';
const mode = process.env.CHAMPR_MODE || 'ranked';
const batchSize = Number(process.env.CHAMPR_BATCH_SIZE || 50);

const basicAuth = `Basic ${Buffer.from(`${adminEmail}:${adminPassword}`).toString('base64')}`;

function loadLegacyChampionIds() {
  const indexPath = path.join(repoRoot, '.cache', 'opgg-pkg', 'package', 'index.json');
  if (!fs.existsSync(indexPath)) return new Map();

  const index = JSON.parse(fs.readFileSync(indexPath, 'utf8'));
  const map = new Map();
  for (const entry of Object.values(index)) {
    const key = entry.key;
    const name = String(entry.name || entry.id || '')
      .trim()
      .toLowerCase()
      .replace(/\s+/g, '');
    const id = String(entry.id || '')
      .trim()
      .toLowerCase()
      .replace(/\s+/g, '');
    if (key && /^\d+$/.test(String(key))) {
      map.set(name, Number(key));
      map.set(id, Number(key));
    }
  }
  return map;
}

async function postBatch(items) {
  const response = await fetch(`${serverUrl}/api/admin/champion-data/batch`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: basicAuth,
    },
    body: JSON.stringify({ items }),
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`batch upload failed (${response.status}): ${text}`);
  }
  return response.json();
}

async function main() {
  const allPath = path.join(outputDir, '_all.json');
  if (!fs.existsSync(allPath)) {
    throw new Error(`crawl output not found: ${allPath}`);
  }

  const sections = JSON.parse(fs.readFileSync(allPath, 'utf8'));
  if (!Array.isArray(sections)) {
    throw new Error(`${allPath} must contain a JSON array`);
  }

  const legacyIds = loadLegacyChampionIds();
  const items = [];
  const skipped = [];

  for (const section of sections) {
    const alias = String(section.alias || '');
    const numericId = Number(section.id);
    const championId = Number.isInteger(numericId) && numericId > 0
      ? numericId
      : legacyIds.get(alias.trim().toLowerCase()) || 0;
    const version = String(section.officialVersion || section.version || '');

    if (!alias || championId <= 0 || !version) {
      skipped.push(`${alias || section.id || 'unknown'} (invalid id/version)`);
      continue;
    }

    items.push({
      source_key: sourceKey,
      champion_id: championId,
      champion_alias: alias,
      mode,
      version,
      content: [section],
    });
  }

  console.log(`[import] Found ${items.length} champion payloads (${skipped.length} skipped)`);
  if (skipped.length > 0) {
    console.log(`[import] Skipped: ${skipped.join(', ')}`);
  }

  let uploaded = 0;
  for (let i = 0; i < items.length; i += batchSize) {
    const chunk = items.slice(i, i + batchSize);
    await postBatch(chunk);
    uploaded += chunk.length;
    console.log(`[import] Uploaded ${uploaded}/${items.length}`);
  }

  console.log('[import] Done.');
}

main().catch((err) => {
  console.error('[import] Fatal error:', err);
  process.exit(1);
});
