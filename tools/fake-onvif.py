#!/usr/bin/env python3
"""A stand-in for an ONVIF camera, for trying "Find cameras" without a yard.

    python3 tools/fake-onvif.py [--ip 192.168.1.50] [--port 8080] [--no-discovery]

Answers WS-Discovery probes on UDP 3702 (multicast and direct) and the few
device and media calls the station makes, and points at tools/fake-camera.py
for the streams. Like the real cameras it wants a login (admin / secret,
WS-Security password digest) for everything but the time and the probe.
--no-discovery leaves the probe unanswered, so only the sweep can find it.
"""
import argparse
import base64
import hashlib
import re
import socket
import struct
import threading
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

parser = argparse.ArgumentParser()
parser.add_argument("--ip", help="the address to advertise (default: this PC's LAN address)")
parser.add_argument("--port", type=int, default=8080)
parser.add_argument("--rtsp", default=None, help="RTSP base (default rtsp://IP:8554)")
parser.add_argument("--no-discovery", action="store_true")
options = parser.parse_args()


def lan_address():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("192.0.2.1", 9))  # no packet is sent; this just picks the route
        return s.getsockname()[0]
    finally:
        s.close()


IP = options.ip or lan_address()
RTSP = options.rtsp or f"rtsp://{IP}:8554"
USER, PASSWORD = "admin", "secret"
DEVICE_URL = f"http://{IP}:{options.port}/onvif/device_service"
MEDIA_URL = f"http://camera.invalid:{options.port}/onvif/Media"  # unresolvable, as some firmware does


def envelope(body):
    return ('<?xml version="1.0" encoding="UTF-8"?><SOAP-ENV:Envelope xmlns:SOAP-ENV="http://www.w3.org/2003/05/soap-envelope" '
            'xmlns:tds="http://www.onvif.org/ver10/device/wsdl" xmlns:trt="http://www.onvif.org/ver10/media/wsdl" '
            f'xmlns:tt="http://www.onvif.org/ver10/schema"><SOAP-ENV:Body>{body}</SOAP-ENV:Body></SOAP-ENV:Envelope>')


def fault(reason):
    return envelope(f'<SOAP-ENV:Fault><SOAP-ENV:Code><SOAP-ENV:Value>SOAP-ENV:Sender</SOAP-ENV:Value></SOAP-ENV:Code>'
                    f'<SOAP-ENV:Reason><SOAP-ENV:Text xml:lang="en">{reason}</SOAP-ENV:Text></SOAP-ENV:Reason></SOAP-ENV:Fault>')


def field(xml, name):
    m = re.search(rf"<(?:\w+:)?{name}\b[^>]*>([^<]*)<", xml)
    return m.group(1).strip() if m else ""


def authorised(xml):
    """The WS-Security UsernameToken digest, checked as a camera would."""
    if field(xml, "Username") != USER:
        return False
    nonce = base64.b64decode(field(xml, "Nonce"))
    expected = base64.b64encode(hashlib.sha1(nonce + field(xml, "Created").encode() + PASSWORD.encode()).digest()).decode()
    return field(xml, "Password") == expected


PROFILES = [("Profile_1", "MainStream", 1920, 1080, "/main"), ("Profile_2", "SubStream", 960, 480, "/sub")]


def answer(xml):
    now = datetime.now(timezone.utc)
    if "GetSystemDateAndTime" in xml:
        return 200, envelope(
            "<tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime><tt:UTCDateTime>"
            f"<tt:Time><tt:Hour>{now.hour}</tt:Hour><tt:Minute>{now.minute}</tt:Minute><tt:Second>{now.second}</tt:Second></tt:Time>"
            f"<tt:Date><tt:Year>{now.year}</tt:Year><tt:Month>{now.month}</tt:Month><tt:Day>{now.day}</tt:Day></tt:Date>"
            "</tt:UTCDateTime></tds:SystemDateAndTime></tds:GetSystemDateAndTimeResponse>")
    if not authorised(xml):
        return 400, fault("Sender not authorized")
    if "GetDeviceInformation" in xml:
        return 200, envelope("<tds:GetDeviceInformationResponse><tds:Manufacturer>Fakecam</tds:Manufacturer>"
                             "<tds:Model>Scale Cam 2000</tds:Model><tds:HardwareId>FC-1</tds:HardwareId></tds:GetDeviceInformationResponse>")
    if "GetCapabilities" in xml:
        return 200, envelope(f"<tds:GetCapabilitiesResponse><tds:Capabilities><tt:Media><tt:XAddr>{MEDIA_URL}</tt:XAddr></tt:Media>"
                             "</tds:Capabilities></tds:GetCapabilitiesResponse>")
    if "GetProfiles" in xml:
        profiles = "".join(
            f'<trt:Profiles token="{token}" fixed="true"><tt:Name>{name}</tt:Name>'
            f'<tt:VideoEncoderConfiguration token="Enc_{token}"><tt:Name>enc</tt:Name><tt:Encoding>H265</tt:Encoding>'
            f'<tt:Resolution><tt:Width>{w}</tt:Width><tt:Height>{h}</tt:Height></tt:Resolution></tt:VideoEncoderConfiguration></trt:Profiles>'
            for token, name, w, h, _ in PROFILES)
        return 200, envelope(f"<trt:GetProfilesResponse>{profiles}</trt:GetProfilesResponse>")
    for tag in ("GetStreamUri", "GetSnapshotUri"):
        if tag in xml:
            token = field(xml, "ProfileToken")
            path = next((p for t, *_, p in PROFILES if t == token), None)
            if path is None:
                return 400, fault("No such profile")
            uri = f"{RTSP}{path}" if tag == "GetStreamUri" else f"http://{IP}:{options.port}/snapshot{path}.jpg"
            return 200, envelope(f"<trt:{tag}Response><trt:MediaUri><tt:Uri>{uri}</tt:Uri></trt:MediaUri></trt:{tag}Response>")
    return 400, fault("Action not supported")


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        if not self.path.startswith("/onvif/"):
            self.send_error(404)
            return
        xml = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode("utf-8", "replace")
        status, body = answer(xml)
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/soap+xml; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, fmt, *args):
        print(f"onvif: {self.command} {self.path} -> {args[1] if len(args) > 1 else ''}", flush=True)


def discovery():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("", 3702))
    s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, struct.pack("4s4s", socket.inet_aton("239.255.255.250"), socket.inet_aton(IP)))
    while True:
        data, sender = s.recvfrom(65536)
        text = data.decode("utf-8", "replace")
        if "Probe" not in text or "ProbeMatch" in text:
            continue
        print(f"probe from {sender[0]}", flush=True)
        relates = field(text, "MessageID")
        reply = ('<?xml version="1.0" encoding="UTF-8"?><SOAP-ENV:Envelope xmlns:SOAP-ENV="http://www.w3.org/2003/05/soap-envelope" '
                 'xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery">'
                 f'<SOAP-ENV:Header><wsa:RelatesTo>{relates}</wsa:RelatesTo></SOAP-ENV:Header><SOAP-ENV:Body><d:ProbeMatches><d:ProbeMatch>'
                 '<d:Scopes>onvif://www.onvif.org/name/Scale+Camera onvif://www.onvif.org/hardware/FC-1 onvif://www.onvif.org/location/weighbridge</d:Scopes>'
                 f'<d:XAddrs>http://[fe80::1]/onvif/device_service {DEVICE_URL}</d:XAddrs></d:ProbeMatch></d:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>')
        s.sendto(reply.encode(), sender)


if not options.no_discovery:
    threading.Thread(target=discovery, daemon=True).start()
print(f"onvif camera at {DEVICE_URL} (discovery {'off' if options.no_discovery else 'on'})", flush=True)
ThreadingHTTPServer(("0.0.0.0", options.port), Handler).serve_forever()
