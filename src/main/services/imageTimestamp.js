// Burns our own time onto a camera snapshot.
//
// The cameras burn in their own clocks, and those clocks cannot be relied on:
// the Gate West Side camera reads 1970 because it never calls its time server
// unless someone does it by hand, and loses the setting again on every reboot;
// the Scale camera runs an hour out. Someone reviewing an access log snapshot
// later has no way to tell which of those to believe, and the camera's version
// is the one printed in large letters across the picture.
//
// So every snapshot we save carries the time the server recorded it, labelled,
// in a fixed corner. Where the two disagree, ours is the one to trust - and
// labelling it is the point, because an unlabelled second timestamp would just
// be a second thing to wonder about.

const pad = (n) => String(n).padStart(2, '0');

// Server local time, which is the same clock the access log entry is stamped
// with, so the picture and the row beside it agree.
const formatServerTime = (when = new Date()) => {
  const d = when instanceof Date && !Number.isNaN(when.getTime()) ? when : new Date();
  let zone = '';
  try {
    zone = new Intl.DateTimeFormat('en-US', { timeZoneName: 'short' })
      .formatToParts(d).find((p) => p.type === 'timeZoneName')?.value || '';
  } catch { /* fall back to no zone rather than fail a snapshot over it */ }
  const date = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  return `${date} ${time}${zone ? ` ${zone}` : ''}`;
};

const escapeXml = (s) => String(s)
  .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

// The chip on its own, so a caller already building an SVG overlay can add it
// to that one rather than compositing the image twice.
const timestampSvg = ({ width, height, when, label = 'server' }) => {
  const text = `${label} ${formatServerTime(when)}`;
  const fontSize = Math.max(11, Math.round(width / 70));
  const padX = Math.round(fontSize * 0.5);
  const padY = Math.round(fontSize * 0.35);
  // Rough width for a sans-serif face; the same 0.6 factor the box labels use.
  const boxWidth = Math.round(text.length * fontSize * 0.6) + padX * 2;
  const boxHeight = fontSize + padY * 2;
  const margin = Math.max(6, Math.round(width / 120));
  // Bottom right. The cameras put their own clock along the top and their name
  // bottom left, so this is the one corner none of them are already using.
  const x = Math.max(0, width - boxWidth - margin);
  const y = Math.max(0, height - boxHeight - margin);
  return `<rect x="${x}" y="${y}" width="${boxWidth}" height="${boxHeight}" rx="3" fill="rgba(17,17,17,0.72)"/>`
    + `<text x="${x + padX}" y="${y + padY + fontSize - Math.round(fontSize * 0.15)}"`
    + ` font-family="sans-serif" font-size="${fontSize}" fill="#f9fafb">${escapeXml(text)}</text>`;
};

// Stamp a bare JPEG that has no other overlay on it.
const stampImage = async (jpegBuffer, when = new Date()) => {
  const sharp = require('sharp');
  const image = sharp(jpegBuffer, { failOn: 'none' });
  const { width, height } = await image.metadata();
  if (!width || !height) return jpegBuffer;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}">`
    + timestampSvg({ width, height, when })
    + '</svg>';
  return image
    .composite([{ input: Buffer.from(svg), top: 0, left: 0 }])
    .jpeg({ quality: 85 })
    .toBuffer();
};

module.exports = { formatServerTime, timestampSvg, stampImage };
