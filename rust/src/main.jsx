// The window is the Electron app's renderer (src/renderer in the repository),
// built from there rather than copied, so the two apps cannot drift apart.
// station.js goes first: it puts window.station in place before the app
// starts calling it.
import './station';
import '../../src/renderer/src/main.jsx';
