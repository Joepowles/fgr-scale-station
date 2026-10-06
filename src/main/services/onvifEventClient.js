const http = require('http');
const https = require('https');
const crypto = require('crypto');

// Minimal ONVIF event client: enough of the Events service to pull motion
// events from a camera, and nothing else.
//
// Written by hand rather than pulling in an ONVIF library because the cameras
// on the gates are budget Xiongmai-platform units whose SOAP is only loosely
// standard: PullMessages returns immediately instead of waiting out its
// Timeout, the clock may be stuck in 1970, and Renew is unreliable. Those
// quirks are handled here (poll on a timer, sign requests with the camera's
// own clock, re-subscribe instead of renewing) and would fight a library
// that expects a compliant device.

const SOAP_TIMEOUT_MS = 10000;

const escapeXml = (value) => String(value)
  .replace(/&/g, '&amp;')
  .replace(/</g, '&lt;')
  .replace(/>/g, '&gt;')
  .replace(/"/g, '&quot;');

const NS = {
  s: 'http://www.w3.org/2003/05/soap-envelope',
  a: 'http://www.w3.org/2005/08/addressing',
  tds: 'http://www.onvif.org/ver10/device/wsdl',
  tev: 'http://www.onvif.org/ver10/events/wsdl',
  wsnt: 'http://docs.oasis-open.org/wsn/b-2',
  wsse: 'http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd',
  wsu: 'http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd',
  trt: 'http://www.onvif.org/ver10/media/wsdl',
  tt: 'http://www.onvif.org/ver10/schema'
};

// Grab the text of the first element whose local name matches, ignoring any
// namespace prefix. Regex is enough for the handful of fields we read.
const textOf = (xml, localName) => {
  const match = xml.match(new RegExp(`<(?:[A-Za-z0-9_]+:)?${localName}(?:\\s[^>]*)?>([^<]*)<`, 'i'));
  return match ? match[1].trim() : '';
};

const parseFault = (xml) => {
  if (!/Fault>/i.test(xml)) return null;
  const reason = textOf(xml, 'Text') || textOf(xml, 'Reason') || textOf(xml, 'Subcode') || 'SOAP fault';
  return reason.replace(/\s+/g, ' ').trim();
};

class OnvifEventClient {
  /**
   * @param {object} options
   * @param {string} options.deviceUrl  e.g. http://192.168.1.10/onvif/device_service
   * @param {string} options.username
   * @param {string} options.password
   */
  constructor({ deviceUrl, username, password }) {
    this.deviceUrl = deviceUrl;
    this.username = username || '';
    this.password = password || '';
    // Camera clock minus our clock. WS-Security timestamps are signed with the
    // camera's idea of "now" so a camera with a wrong clock still accepts us.
    this.clockOffsetMs = 0;
    this.eventsUrl = null;
    this.subscriptionUrl = null;
    this.subscribedAt = 0;
  }

  _securityHeader() {
    if (!this.username) return '';
    const nonce = crypto.randomBytes(16);
    const created = new Date(Date.now() + this.clockOffsetMs).toISOString();
    const digest = crypto.createHash('sha1')
      .update(Buffer.concat([nonce, Buffer.from(created), Buffer.from(this.password)]))
      .digest('base64');
    return `<wsse:Security s:mustUnderstand="1" xmlns:wsse="${NS.wsse}" xmlns:wsu="${NS.wsu}">` +
      `<wsse:UsernameToken><wsse:Username>${escapeXml(this.username)}</wsse:Username>` +
      `<wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">${digest}</wsse:Password>` +
      `<wsse:Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">${nonce.toString('base64')}</wsse:Nonce>` +
      `<wsu:Created>${created}</wsu:Created></wsse:UsernameToken></wsse:Security>`;
  }

  _request(url, body, action) {
    return new Promise((resolve, reject) => {
      let target;
      try { target = new URL(url); } catch { return reject(new Error(`Bad ONVIF URL: ${url}`)); }
      const client = target.protocol === 'https:' ? https : http;
      const addressing = action
        ? `<a:To s:mustUnderstand="1">${escapeXml(url)}</a:To><a:Action s:mustUnderstand="1">${action}</a:Action>`
        : '';
      const xml = `<?xml version="1.0" encoding="UTF-8"?>` +
        `<s:Envelope xmlns:s="${NS.s}" xmlns:a="${NS.a}"><s:Header>${this._securityHeader()}${addressing}</s:Header>` +
        `<s:Body>${body}</s:Body></s:Envelope>`;
      const req = client.request({
        hostname: target.hostname,
        port: target.port || (target.protocol === 'https:' ? 443 : 80),
        path: `${target.pathname}${target.search || ''}`,
        method: 'POST',
        headers: {
          'Content-Type': 'application/soap+xml; charset=utf-8',
          'Content-Length': Buffer.byteLength(xml)
        },
        rejectUnauthorized: false,
        timeout: SOAP_TIMEOUT_MS
      }, (res) => {
        let data = '';
        res.setEncoding('utf8');
        res.on('data', (chunk) => { data += chunk; });
        res.on('end', () => {
          const fault = parseFault(data);
          if (fault) return reject(new Error(`ONVIF fault: ${fault}`));
          if (res.statusCode >= 400) return reject(new Error(`ONVIF HTTP ${res.statusCode}`));
          resolve(data);
        });
      });
      req.on('timeout', () => req.destroy(new Error('ONVIF request timed out')));
      req.on('error', reject);
      req.end(xml);
    });
  }

  // Learn the camera's clock and where its Events service lives.
  async connect() {
    const timeXml = await this._request(this.deviceUrl, `<tds:GetSystemDateAndTime xmlns:tds="${NS.tds}"/>`);
    const utc = timeXml.match(/<(?:[A-Za-z0-9_]+:)?UTCDateTime>(.*?)<\/(?:[A-Za-z0-9_]+:)?UTCDateTime>/s);
    if (utc) {
      const n = (name) => Number(textOf(utc[1], name));
      const cameraNow = Date.UTC(n('Year'), n('Month') - 1, n('Day'), n('Hour'), n('Minute'), n('Second'));
      if (Number.isFinite(cameraNow)) this.clockOffsetMs = cameraNow - Date.now();
    }

    const capsXml = await this._request(
      this.deviceUrl,
      `<tds:GetCapabilities xmlns:tds="${NS.tds}"><tds:Category>Events</tds:Category></tds:GetCapabilities>`
    );
    const events = capsXml.match(/<(?:[A-Za-z0-9_]+:)?Events>.*?<(?:[A-Za-z0-9_]+:)?XAddr>([^<]+)</s);
    this.eventsUrl = events ? events[1].trim() : this.deviceUrl.replace(/\/onvif\/.*$/, '/onvif/Events');
    // Some firmware advertises a hostname we cannot resolve; keep the host we
    // were given and only take the path from the camera.
    try {
      const advertised = new URL(this.eventsUrl);
      const given = new URL(this.deviceUrl);
      advertised.hostname = given.hostname;
      advertised.port = given.port;
      advertised.protocol = given.protocol;
      this.eventsUrl = advertised.toString();
    } catch { /* keep as advertised */ }
    return this;
  }

  // Which event topics the camera can raise (e.g. RuleEngine/CellMotionDetector/Motion).
  async getTopics() {
    if (!this.eventsUrl) await this.connect();
    const xml = await this._request(this.eventsUrl, `<tev:GetEventProperties xmlns:tev="${NS.tev}"/>`);
    const topicSet = (xml.match(/<(?:[A-Za-z0-9_]+:)?TopicSet.*?<\/(?:[A-Za-z0-9_]+:)?TopicSet>/s) || [''])[0];
    const topics = [];
    const stack = [];
    const tagRe = /<\/?([A-Za-z0-9:_]+)([^>]*)>/g;
    let match;
    while ((match = tagRe.exec(topicSet))) {
      const [full, rawName, attrs] = match;
      const name = rawName.replace(/^[A-Za-z0-9_]+:/, '');
      if (full.startsWith('</')) { stack.pop(); continue; }
      stack.push(name);
      if (/topic="true"/.test(attrs)) topics.push(stack.slice(1).join('/'));
      if (full.endsWith('/>')) stack.pop();
    }
    return topics;
  }

  async subscribe(initialTermination = 'PT10M') {
    if (!this.eventsUrl) await this.connect();
    const xml = await this._request(
      this.eventsUrl,
      `<tev:CreatePullPointSubscription xmlns:tev="${NS.tev}"><tev:InitialTerminationTime>${initialTermination}</tev:InitialTerminationTime></tev:CreatePullPointSubscription>`
    );
    const address = xml.match(/<(?:[A-Za-z0-9_]+:)?Address>([^<]+)</);
    if (!address) throw new Error('Camera did not return a subscription address');
    const url = new URL(address[1].trim());
    const given = new URL(this.deviceUrl);
    url.hostname = given.hostname;
    url.port = given.port;
    url.protocol = given.protocol;
    this.subscriptionUrl = url.toString();
    this.subscribedAt = Date.now();
    return this.subscriptionUrl;
  }

  /**
   * Pull pending notifications. Returns
   *   [{ topic, operation, state, items: {Name: Value}, time }]
   * where state is the boolean-ish value of the first State/IsMotion item.
   */
  async pull(timeout = 'PT5S', limit = 50) {
    if (!this.subscriptionUrl) await this.subscribe();
    const xml = await this._request(
      this.subscriptionUrl,
      `<tev:PullMessages xmlns:tev="${NS.tev}"><tev:Timeout>${timeout}</tev:Timeout><tev:MessageLimit>${limit}</tev:MessageLimit></tev:PullMessages>`,
      'http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/PullMessagesRequest'
    );
    const messages = [];
    const msgRe = /<(?:[A-Za-z0-9_]+:)?NotificationMessage>(.*?)<\/(?:[A-Za-z0-9_]+:)?NotificationMessage>/gs;
    let match;
    while ((match = msgRe.exec(xml))) {
      const body = match[1];
      const topic = (textOf(body, 'Topic') || '').replace(/^tns1:/, '');
      const operation = (body.match(/PropertyOperation="([^"]+)"/) || [])[1] || '';
      const time = (body.match(/UtcTime="([^"]+)"/) || [])[1] || '';
      const items = {};
      const itemRe = /<(?:[A-Za-z0-9_]+:)?SimpleItem\s+([^>]*)\/?>/g;
      let item;
      while ((item = itemRe.exec(body))) {
        const name = (item[1].match(/Name="([^"]*)"/) || [])[1];
        const value = (item[1].match(/Value="([^"]*)"/) || [])[1];
        if (name) items[name] = value;
      }
      const rawState = items.State ?? items.IsMotion ?? items.IsInside ?? items.LogicalState;
      const state = rawState === undefined ? null : /^(true|1|on)$/i.test(String(rawState));
      messages.push({ topic, operation, state, items, time });
    }
    return messages;
  }

  // ── Which video source is which channel ─────────────────────────────────
  //
  // A recorder delivers every channel's events on one subscription: the XVR in
  // the yard carries twelve cameras and sends all twelve to anyone who asks.
  // Each notification names its source, but as an opaque token - this device
  // uses "00000" through "01100" - and the mapping to the channel number in an
  // RTSP url is the device's business, not something to be guessed at. On this
  // firmware channel 3 is token 00200, so reading the token as a channel
  // number would have pointed a camera at someone else's picture and looked
  // like it was working.
  //
  // So ask. The device names its media profiles after their channel, which
  // answers it in one request; failing that, each profile's stream url carries
  // channel=N and settles it definitively.
  async _mediaUrl() {
    if (this._media) return this._media;
    if (!this.eventsUrl) await this.connect();
    const caps = await this._request(this.deviceUrl, `<tds:GetCapabilities xmlns:tds="${NS.tds}"><tds:Category>Media</tds:Category></tds:GetCapabilities>`);
    const found = caps.match(/<(?:[A-Za-z0-9_]+:)?Media>.*?<(?:[A-Za-z0-9_]+:)?XAddr>([^<]+)</s);
    let url = found ? found[1].trim() : this.deviceUrl.replace(/\/onvif\/.*$/, '/onvif/Media');
    try {
      const advertised = new URL(url);
      const given = new URL(this.deviceUrl);
      advertised.hostname = given.hostname;
      advertised.port = given.port;
      advertised.protocol = given.protocol;
      url = advertised.toString();
    } catch { /* keep as advertised */ }
    this._media = url;
    return url;
  }

  async _profiles() {
    if (this._profileList) return this._profileList;
    const xml = await this._request(await this._mediaUrl(), `<trt:GetProfiles xmlns:trt="${NS.trt}"/>`);
    const re = /<(?:[A-Za-z0-9_]+:)?Profiles\b([^>]*)>(.*?)<\/(?:[A-Za-z0-9_]+:)?Profiles>/gs;
    const out = [];
    let m;
    while ((m = re.exec(xml))) {
      const token = (m[1].match(/token="([^"]+)"/) || [])[1] || '';
      const name = (m[2].match(/<(?:[A-Za-z0-9_]+:)?Name>([^<]*)</) || [])[1] || '';
      const vsc = m[2].match(/<(?:[A-Za-z0-9_]+:)?VideoSourceConfiguration\b([^>]*)>/);
      const vscToken = vsc ? ((vsc[1].match(/token="([^"]+)"/) || [])[1] || '') : '';
      // The encoder's resolution, so a stream list can say which is the big one.
      const enc = m[2].match(/<(?:[A-Za-z0-9_]+:)?VideoEncoderConfiguration\b[^>]*>(.*?)<\/(?:[A-Za-z0-9_]+:)?VideoEncoderConfiguration>/s);
      const width = enc ? Number((enc[1].match(/<(?:[A-Za-z0-9_]+:)?Width>(\d+)</) || [])[1]) || null : null;
      const height = enc ? Number((enc[1].match(/<(?:[A-Za-z0-9_]+:)?Height>(\d+)</) || [])[1]) || null : null;
      if (token) out.push({ token, name, vscToken, width, height });
    }
    this._profileList = out;
    return out;
  }

  async _streamChannel(profileToken) {
    const xml = await this._request(await this._mediaUrl(),
      `<trt:GetStreamUri xmlns:trt="${NS.trt}" xmlns:tt="${NS.tt}">`
      + '<trt:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></trt:StreamSetup>'
      + `<trt:ProfileToken>${escapeXml(profileToken)}</trt:ProfileToken></trt:GetStreamUri>`);
    const uri = (xml.match(/<(?:[A-Za-z0-9_]+:)?Uri>([^<]*)</) || [])[1] || '';
    return Number((uri.match(/channel=(\d+)/i) || [])[1]) || null;
  }

  // The source token this device uses for the given RTSP channel, or null if
  // it cannot be established - in which case the caller must not filter.
  async sourceTokenForChannel(channel) {
    const wanted = Number(channel);
    if (!Number.isFinite(wanted)) return null;
    const profiles = await this._profiles();
    for (const p of profiles) {
      const named = Number((p.name.match(/Channel\s*(\d+)/i) || [])[1]);
      if (named === wanted && p.vscToken) return p.vscToken;
    }
    // Names did not carry it; ask each profile for its stream url, stopping as
    // soon as the channel matches rather than walking all of them.
    for (const p of profiles) {
      if (!p.vscToken) continue;
      let chan = null;
      try { chan = await this._streamChannel(p.token); } catch { continue; }
      if (chan === wanted) return p.vscToken;
    }
    return null;
  }

  async unsubscribe() {
    if (!this.subscriptionUrl) return;
    const url = this.subscriptionUrl;
    this.subscriptionUrl = null;
    try {
      await this._request(
        url,
        `<wsnt:Unsubscribe xmlns:wsnt="${NS.wsnt}"/>`,
        'http://docs.oasis-open.org/wsn/bw-2/SubscriptionManager/UnsubscribeRequest'
      );
    } catch { /* best effort - the subscription expires on its own */ }
  }
}

module.exports = { OnvifEventClient };
