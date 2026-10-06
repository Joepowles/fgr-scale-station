// Build the embedded Python that reads plates, into resources/python.
//
// Windows only (it is what the station runs on). Downloads the official
// "embeddable" CPython zip, enables site-packages in it, bootstraps pip,
// installs fast-alpr and the ONNX runtime, then runs the plate worker once so
// the two models land in resources/python/home/.cache - the folder the app
// points the worker's HOME at, so a station with no internet still reads
// plates. Usage on the release runner: node scripts/build-python.js
const fs = require('fs');
const path = require('path');
const https = require('https');
const { execFileSync } = require('child_process');

const PY_VERSION = process.env.STATION_PYTHON_VERSION || '3.11.9';
const PY_URL = `https://www.python.org/ftp/python/${PY_VERSION}/python-${PY_VERSION}-embed-amd64.zip`;
const GET_PIP_URL = 'https://bootstrap.pypa.io/get-pip.py';

const root = path.join(__dirname, '..');
const pyDir = path.join(root, 'resources', 'python');
const home = path.join(pyDir, 'home');
const worker = path.join(root, 'resources', 'python-worker', 'plate_reader_worker.py');

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

(async () => {
  if (process.platform !== 'win32') {
    console.error('build-python.js builds the Windows embedded Python and must run on Windows.');
    process.exit(1);
  }
  fs.rmSync(pyDir, { recursive: true, force: true });
  fs.mkdirSync(pyDir, { recursive: true });
  const zip = path.join(pyDir, 'python.zip');
  console.log(`Downloading ${PY_URL} ...`);
  await download(PY_URL, zip);
  execFileSync('powershell', ['-NoProfile', '-Command', `Expand-Archive -LiteralPath '${zip}' -DestinationPath '${pyDir}' -Force`], { stdio: 'inherit' });
  fs.rmSync(zip, { force: true });

  // The embeddable build ships with site-packages disabled: its ._pth file
  // lists what goes on sys.path and "import site" is commented out.
  const pth = fs.readdirSync(pyDir).find((f) => /^python\d+\._pth$/.test(f));
  if (!pth) throw new Error('python._pth not found in the embeddable zip');
  const pthPath = path.join(pyDir, pth);
  fs.writeFileSync(pthPath, fs.readFileSync(pthPath, 'utf8').replace(/^#\s*import site/m, 'import site') + '\nLib\\site-packages\n');

  const python = path.join(pyDir, 'python.exe');
  const getPip = path.join(pyDir, 'get-pip.py');
  await download(GET_PIP_URL, getPip);
  execFileSync(python, [getPip, '--no-warn-script-location'], { stdio: 'inherit' });
  fs.rmSync(getPip, { force: true });
  execFileSync(python, ['-m', 'pip', 'install', '--no-warn-script-location', '--disable-pip-version-check', 'fast-alpr>=0.4', 'onnxruntime'], { stdio: 'inherit' });

  // Fetch the models into the home the app will hand the worker.
  fs.mkdirSync(home, { recursive: true });
  console.log('Loading the plate models once so they are cached in the build...');
  const out = execFileSync(python, [worker], {
    input: '',
    env: { ...process.env, USERPROFILE: home, HOME: home, PYTHONUTF8: '1' },
    encoding: 'utf8',
    stdio: ['pipe', 'pipe', 'inherit']
  });
  if (!/"ready": true/.test(out)) throw new Error(`plate worker did not come ready: ${out.trim()}`);
  const cache = path.join(home, '.cache');
  if (!fs.existsSync(cache)) throw new Error(`models were not cached under ${cache}`);
  fs.rmSync(path.join(home, 'AppData'), { recursive: true, force: true });
  console.log(`Embedded Python ready at ${pyDir}; models cached under ${cache}`);
})().catch((err) => { console.error(err.message); process.exit(1); });
