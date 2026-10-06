/**
 * Strip passwords out of any URL credentials embedded in a string.
 *
 * Camera stream URLs carry the login inline (rtsp://user:pass@host/path), and
 * both ffmpeg and execFile hand that URL straight back on failure — ffmpeg
 * quotes the input URL in its errors, and execFile puts the whole command line
 * into err.message. Passing either through untouched puts the camera password
 * into an API response the browser displays, or into the PM2 logs.
 *
 * Run anything derived from a stream URL or an ffmpeg failure through here.
 *
 *   redactCredentials('rtsp://admin:Egasi%23687@10.0.0.5:554/live')
 *     => 'rtsp://admin:***@10.0.0.5:554/live'
 */
const redactCredentials = (text) =>
  String(text ?? '').replace(/([a-z][a-z0-9+.-]*:\/\/)([^\s:/@]+):([^\s/]*)@(?=[^\s/@]+)/gi, '$1$2:***@');

module.exports = { redactCredentials };
