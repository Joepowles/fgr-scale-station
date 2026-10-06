// Draws the app icon (a scale on a dark green tile) as build/icon.png;
// electron-builder makes the .ico from it. Usage: node scripts/make-icon.js
const fs = require('fs');
const path = require('path');

(async () => {
  const sharp = require('sharp');
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
    <rect width="512" height="512" rx="96" fill="#064e3b"/>
    <rect x="96" y="332" width="320" height="40" rx="8" fill="#a7f3d0"/>
    <rect x="236" y="140" width="40" height="200" rx="8" fill="#a7f3d0"/>
    <rect x="136" y="128" width="240" height="28" rx="8" fill="#a7f3d0"/>
    <path d="M136 156 L96 252 Q136 300 176 252 Z" fill="#34d399"/>
    <path d="M376 156 L336 252 Q376 300 416 252 Z" fill="#34d399"/>
    <rect x="156" y="380" width="200" height="24" rx="6" fill="#6ee7b7"/>
  </svg>`;
  const out = path.join(__dirname, '..', 'build', 'icon.png');
  fs.mkdirSync(path.dirname(out), { recursive: true });
  await sharp(Buffer.from(svg)).png().toFile(out);
  console.log(`icon written: ${out}`);
})().catch((err) => { console.error(err.message); process.exit(1); });
