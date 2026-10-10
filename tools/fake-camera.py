#!/usr/bin/env python3
"""A stand-in for the yard's scale camera: an RTSP server sending HEVC, as
the real cameras do, with a moving test picture and the time burned in.

    python3 tools/fake-camera.py [port]       # default 8554

Then rtsp://admin:secret@127.0.0.1:8554/sub (960x480) and /main (1920x1080).
Needs GStreamer's RTSP server (Debian: gir1.2-gst-rtsp-server-1.0,
python3-gi, gstreamer1.0-plugins-{good,bad,ugly}).
"""
import sys

import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstRtspServer", "1.0")
from gi.repository import GLib, Gst, GstRtspServer  # noqa: E402

Gst.init(None)
port = sys.argv[1] if len(sys.argv) > 1 else "8554"


def factory(width, height):
    f = GstRtspServer.RTSPMediaFactory()
    f.set_launch(
        f"( videotestsrc is-live=true pattern=ball ! video/x-raw,width={width},height={height},framerate=15/1 "
        "! clockoverlay font-desc=\"Sans 36\" ! videoconvert "
        "! x265enc tune=zerolatency speed-preset=ultrafast key-int-max=30 ! rtph265pay name=pay0 pt=96 )"
    )
    f.set_shared(True)
    perms = GstRtspServer.RTSPPermissions()
    perms.add_permission_for_role("admin", GstRtspServer.RTSP_PERM_MEDIA_FACTORY_ACCESS, True)
    perms.add_permission_for_role("admin", GstRtspServer.RTSP_PERM_MEDIA_FACTORY_CONSTRUCT, True)
    f.set_permissions(perms)
    return f


server = GstRtspServer.RTSPServer()
server.set_service(port)
mounts = server.get_mount_points()
mounts.add_factory("/sub", factory(960, 480))
mounts.add_factory("/main", factory(1920, 1080))

# The cameras want a login; so does this one: admin / secret.
auth = GstRtspServer.RTSPAuth()
token = GstRtspServer.RTSPToken()
token.set_string(GstRtspServer.RTSP_TOKEN_MEDIA_FACTORY_ROLE, "admin")
auth.add_basic(GstRtspServer.RTSPAuth.make_basic("admin", "secret"), token)
server.set_auth(auth)

server.attach(None)
print(f"camera on rtsp://admin:secret@127.0.0.1:{port}/sub and /main", flush=True)
GLib.MainLoop().run()
