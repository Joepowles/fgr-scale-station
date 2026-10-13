#!/usr/bin/env python3
"""A stand-in for the yard's scale camera: an RTSP server sending HEVC, as
the real cameras do, with a moving test picture and the time burned in.

    python3 tools/fake-camera.py [port] [--image photo.jpg]   # default 8554
    python3 tools/fake-camera.py --frames 'dir/frame%03d.jpg' [--fps 4]

With --image the picture is that photo (a truck with a plate, say) instead
of a test pattern, for trying the plate reader.

Then rtsp://admin:secret@127.0.0.1:8554/sub (960x480) and /main (1920x1080).
Needs GStreamer's RTSP server (Debian: gir1.2-gst-rtsp-server-1.0,
python3-gi, gstreamer1.0-plugins-{good,bad,ugly}).
"""
import argparse

import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstRtspServer", "1.0")
from gi.repository import GLib, Gst, GstRtspServer  # noqa: E402

Gst.init(None)
parser = argparse.ArgumentParser()
parser.add_argument("port", nargs="?", default="8554")
parser.add_argument("--image", help="a JPEG to show instead of the test pattern")
parser.add_argument("--frames", help="numbered JPEGs (frame%%03d.jpg) played on a loop: a truck pulling up, say")
parser.add_argument("--fps", default="4", help="frames a second for --frames")
options = parser.parse_args()
port = options.port


def factory(width, height):
    if options.frames:
        source = (
            f"multifilesrc location=\"{options.frames}\" index=0 loop=true caps=\"image/jpeg,framerate={options.fps}/1\" ! jpegdec "
            f"! videoscale add-borders=true ! videoconvert ! videorate ! video/x-raw,width={width},height={height},framerate=15/1"
        )
    elif options.image:
        source = (
            f"filesrc location=\"{options.image}\" ! jpegdec ! imagefreeze is-live=true "
            f"! videoscale add-borders=true ! videoconvert ! video/x-raw,width={width},height={height},framerate=15/1"
        )
    else:
        source = f"videotestsrc is-live=true pattern=ball ! video/x-raw,width={width},height={height},framerate=15/1"
    f = GstRtspServer.RTSPMediaFactory()
    f.set_launch(
        f"( {source} ! clockoverlay font-desc=\"Sans 36\" ! videoconvert "
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
