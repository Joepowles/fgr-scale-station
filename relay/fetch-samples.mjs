// Fetches the sample pictures the yard stations have uploaded to the relay
// (src/samples.js), unpacks each into a folder of its own, and deletes it
// from the relay once it is safely on disk. Node 18 or later; tar is on the
// PATH on Linux, macOS and Windows 10 or later.
//
//   SAMPLES_KEY=... node relay/fetch-samples.mjs [folder] [--keep]
//
// The folder defaults to ./yard-samples. Each truck lands in
// <folder>/<station>/<visit>/: its frames as JPEGs, log.txt (the station's
// log over the visit, with the scale readings) and visit.json. --keep leaves
// them on the relay too. RELAY_URL overrides the relay's address.

import { mkdirSync, writeFileSync, existsSync, rmSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

const relay = (process.env.RELAY_URL || 'https://scale-station-reports.joepowles.workers.dev').replace(/\/+$/, '');
const key = process.env.SAMPLES_KEY;
const args = process.argv.slice(2);
const keep = args.includes('--keep');
const folder = args.find((a) => !a.startsWith('--')) || 'yard-samples';
if (!key) {
  console.error('Set SAMPLES_KEY to the key set on the relay (wrangler secret put SAMPLES_KEY).');
  process.exit(1);
}
const auth = { authorization: `Bearer ${key}` };

const res = await fetch(`${relay}/samples`, { headers: auth });
if (!res.ok) {
  console.error(`The relay answered ${res.status}: ${await res.text()}`);
  process.exit(1);
}
const { samples } = await res.json();
console.log(`${samples.length} sample${samples.length === 1 ? '' : 's'} waiting`);
let fetched = 0;
for (const { name, size } of samples) {
  const [station, file] = name.split('/');
  const target = join(folder, station, file.replace(/\.tar$/, ''));
  if (!existsSync(target)) {
    const one = await fetch(`${relay}/samples/${name}`, { headers: auth });
    if (!one.ok) {
      console.error(`${name}: the relay answered ${one.status}; left there`);
      continue;
    }
    const bytes = Buffer.from(await one.arrayBuffer());
    // Only a plain tar, which cannot unpack to more than its own size.
    if (bytes.length < 512 || bytes.subarray(257, 262).toString('latin1') !== 'ustar') {
      console.error(`${name}: not a plain tar; left on the relay`);
      continue;
    }
    // Unpacked beside the target and renamed into place, so a run cut short
    // leaves no folder that would make the next run skip it.
    const partial = `${target}.partial`;
    rmSync(partial, { recursive: true, force: true });
    mkdirSync(partial, { recursive: true });
    const tarFile = `${partial}.tar`;
    writeFileSync(tarFile, bytes);
    const untar = spawnSync('tar', ['-xf', tarFile, '-C', partial], { stdio: 'inherit' });
    rmSync(tarFile, { force: true });
    if (untar.status !== 0) {
      rmSync(partial, { recursive: true, force: true });
      console.error(`${name}: could not unpack it; left on the relay`);
      continue;
    }
    renameSync(partial, target);
    console.log(`${name} (${Math.round((size || 0) / 1024)} KB) -> ${target}`);
    fetched += 1;
  }
  if (!keep) await fetch(`${relay}/samples/${name}`, { method: 'DELETE', headers: auth });
}
console.log(`${fetched} new; ${keep ? 'kept on the relay' : 'removed from the relay'}`);
