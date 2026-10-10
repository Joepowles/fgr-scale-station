// Two update channels on the one GitHub release feed. Stable installs
// follow the newest release that is not marked pre-release (latest.yml).
// Beta installs follow the newest release of either kind: a pre-release
// tagged v0.2.0-beta.1 carries beta.yml; a stable release is taken too, so
// a beta PC is never behind. Going back to stable from a beta version is
// allowed to be a downgrade, so the switch actually happens.
const CHANNELS = ['stable', 'beta'];

const normalize = (channel) => (CHANNELS.includes(channel) ? channel : 'stable');

const isPrerelease = (version) => /-/.test(String(version || ''));

// What to set on electron-updater for the chosen channel.
const updaterOptionsFor = (channel, currentVersion) => {
  const c = normalize(channel);
  if (c === 'beta') return { allowPrerelease: true, channel: 'beta', allowDowngrade: false };
  // 'latest' is electron-updater's own name for the stable channel file; it
  // is set by name because the updater refuses to unset a channel once set.
  return { allowPrerelease: false, channel: 'latest', allowDowngrade: isPrerelease(currentVersion) };
};

module.exports = { CHANNELS, normalize, updaterOptionsFor, isPrerelease };
