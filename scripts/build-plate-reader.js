// Build the plate reader (plate-reader/, Rust) into resources/bin and fetch
// its two models into resources/plate-models, for packaging.
//
// The models are the open ones from the fast-alpr project, pinned by hash so
// a changed upload cannot slip into a build. On Windows the exe has ONNX
// Runtime linked in and needs nothing else; elsewhere it loads the runtime
// from ORT_DYLIB_PATH (see plate-reader/Cargo.toml). Needs Rust (cargo).
// Usage on the release runner: node scripts/build-plate-reader.js
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const https = require('https');
const { execFileSync } = require('child_process');

const MODELS = [
  {
    file: 'yolo-v9-t-384-license-plates-end2end.onnx',
    url: 'https://github.com/ankandrew/open-image-models/releases/download/assets/yolo-v9-t-384-license-plates-end2end.onnx',
    sha256: '888397b96d761c89db40bc9c305838e8652660f5e282c2cadebbe8d2951a77a8'
  },
  {
    file: 'cct_xs_v1_global.onnx',
    url: 'https://github.com/ankandrew/cnn-ocr-lp/releases/download/arg-plates/cct_xs_v1_global.onnx',
    sha256: 'c1ae2ae031b10f8ab7ed6e3cdaf8d9c753477e299dbad3579c8f705a7d882134'
  }
];

const root = path.join(__dirname, '..');
const crate = path.join(root, 'plate-reader');
const binDir = path.join(root, 'resources', 'bin');
const modelsDir = path.join(root, 'resources', 'plate-models');
const exe = process.platform === 'win32' ? 'plate-reader.exe' : 'plate-reader';

const download = (url, file, redirects = 0) => new Promise((resolve, reject) => {
  https.get(url, { headers: { 'User-Agent': 'fgr-scale-station' } }, (res) => {
    if ([301, 302, 303, 307, 308].includes(res.statusCode) && res.headers.location && redirects < 5) {
      res.resume();
      return resolve(download(new URL(res.headers.location, url).toString(), file, redirects + 1));
    }
    if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
    const out = fs.createWriteStream(file);
    res.pipe(out);
    out.on('finish', () => out.close(resolve));
    out.on('error', reject);
  }).on('error', reject);
});

// GitHub's release downloads time out now and then from the build runner;
// the v0.2.0 build failed on one (connect ETIMEDOUT). A few tries, spaced out.
const downloadWithRetries = async (url, file, tries = 4) => {
  for (let attempt = 1; ; attempt++) {
    try {
      return await download(url, file);
    } catch (err) {
      if (attempt >= tries) throw err;
      console.log(`${err.message}; trying again (${attempt + 1} of ${tries}) ...`);
      await new Promise((resolve) => setTimeout(resolve, attempt * 5000));
    }
  }
};

const sha256 = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

(async () => {
  fs.mkdirSync(modelsDir, { recursive: true });
  for (const model of MODELS) {
    const target = path.join(modelsDir, model.file);
    if (fs.existsSync(target) && sha256(target) === model.sha256) {
      console.log(`${model.file}: already here`);
      continue;
    }
    console.log(`Downloading ${model.url} ...`);
    await downloadWithRetries(model.url, `${target}.part`);
    const got = sha256(`${target}.part`);
    if (got !== model.sha256) {
      fs.rmSync(`${target}.part`, { force: true });
      throw new Error(`${model.file} has SHA-256 ${got}, expected ${model.sha256}`);
    }
    fs.renameSync(`${target}.part`, target);
  }

  execFileSync('cargo', ['build', '--release', '--locked', '--manifest-path', path.join(crate, 'Cargo.toml')], { stdio: 'inherit' });
  fs.mkdirSync(binDir, { recursive: true });
  fs.copyFileSync(path.join(crate, 'target', 'release', exe), path.join(binDir, exe));
  console.log(`Plate reader ready: ${path.join(binDir, exe)}, models in ${modelsDir}`);
})().catch((err) => {
  console.error(err.message);
  process.exit(1);
});
