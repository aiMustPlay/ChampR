import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, '..');

const packageDir = process.env.CHAMPR_PACKAGE_DIR
  ? path.resolve(process.env.CHAMPR_PACKAGE_DIR)
  : path.join(repoRoot, '.cache', 'opgg-pkg', 'package');
const serverUrl = process.env.CHAMPR_SERVER_URL || 'http://127.0.0.1:3030';
const adminEmail = process.env.CHAMPR_ADMIN_EMAIL || 'admin@example.com';
const adminPassword = process.env.CHAMPR_ADMIN_PASSWORD || 'change-me-now';
const sourceKey = process.env.CHAMPR_SOURCE_KEY || 'op.gg';
const mode = process.env.CHAMPR_MODE || 'ranked';
const batchSize = Number(process.env.CHAMPR_BATCH_SIZE || 50);

const basicAuth = `Basic ${Buffer.from(`${adminEmail}:${adminPassword}`).toString('base64')}`;

async function postBatch(items) {
  const payload = { items };
  const response = await fetch(`${serverUrl}/api/admin/champion-data/batch`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: basicAuth,
    },
    body: JSON.stringify(payload),
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`batch upload failed (${response.status}): ${text}`);
  }
  return response.json();
}

async function main() {
  if (!fs.existsSync(packageDir)) {
    throw new Error(`package directory not found: ${packageDir}`);
  }

  const files = fs
    .readdirSync(packageDir)
    .filter((name) => name.endsWith('.json') && name !== 'index.json')
    .sort();

  const items = [];
  const skipped = [];

  for (const file of files) {
    const raw = fs.readFileSync(path.join(packageDir, file), 'utf8');
    let sections;
    try {
      sections = JSON.parse(raw);
    } catch (err) {
      skipped.push(`${file}: invalid JSON`);
      continue;
    }

    if (!Array.isArray(sections)) {
      sections = [sections];
    }
    if (sections.length === 0) {
      skipped.push(`${file}: empty`);
      continue;
    }

    const first = sections[0];
    const championId = Number(first.id);
    const alias = String(first.alias || file.replace(/\.json$/i, ''));
    const version = String(first.officialVersion || first.version || '');

    if (!Number.isFinite(championId) || championId <= 0) {
      skipped.push(`${file}: invalid champion id`);
      continue;
    }

    items.push({
      source_key: sourceKey,
      champion_id: championId,
      champion_alias: alias,
      mode,
      version,
      content: sections,
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
