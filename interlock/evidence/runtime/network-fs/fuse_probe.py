"""A minimal FUSE filesystem for one manual check: mounts an empty, read-only
directory with a chosen subtype (so /proc/self/mountinfo says fuse.<subtype>)
and answers just enough of the protocol for stat, statfs and lookups.

Usage: python3 -I fuse_probe.py <mountpoint> <subtype> <seconds> [<source>]
The source is what mountinfo shows as the mount's source ("probe" if not given).
"""
import ctypes
import os
import struct
import sys
import threading
import time

LOOKUP, FORGET, GETATTR, STATFS, INIT, INTERRUPT, BATCH_FORGET = 1, 2, 3, 17, 26, 36, 42
ENOENT, ENOSYS = 2, 38

target, subtype, seconds = sys.argv[1], sys.argv[2], float(sys.argv[3])
source = sys.argv[4] if len(sys.argv) > 4 else "probe"
libc = ctypes.CDLL("libc.so.6", use_errno=True)
fd = os.open("/dev/fuse", os.O_RDWR)
opts = f"fd={fd},rootmode=40000,user_id=0,group_id=0,default_permissions"
if libc.mount(source.encode(), target.encode(), f"fuse.{subtype}".encode(), 1, opts.encode()) != 0:  # MS_RDONLY
    sys.exit(f"mount failed: {os.strerror(ctypes.get_errno())}")
print("mounted", flush=True)


def reply(unique, error=0, body=b""):
    os.write(fd, struct.pack("<IiQ", 16 + len(body), -error if error else 0, unique) + body)


def attr():
    now = int(time.time())
    # fuse_attr: ino size blocks atime mtime ctime, nsecs, mode nlink uid gid rdev blksize flags
    a = struct.pack("<QQQQQQIIIIIIIIII", 1, 0, 0, now, now, now, 0, 0, 0, 0o40755, 2, 0, 0, 0, 4096, 0)
    return struct.pack("<QII", 1, 0, 0) + a


def serve():
    while True:
        try:
            req = os.read(fd, 1 << 17)
        except OSError:
            return
        length, opcode, unique, nodeid = struct.unpack_from("<IIQQ", req)
        if opcode == INIT:
            major, minor = struct.unpack_from("<II", req, 40)
            body = struct.pack("<IIIIHHIIHHI", 7, min(minor, 31), 0, 0, 0, 0, 4096, 1, 0, 0, 0)
            reply(unique, body=body + b"\0" * (64 - len(body)))
        elif opcode == GETATTR:
            reply(unique, body=attr())
        elif opcode == STATFS:
            reply(unique, body=struct.pack("<QQQQQIIII", 0, 0, 0, 0, 0, 4096, 255, 4096, 0) + b"\0" * 24)
        elif opcode == LOOKUP:
            reply(unique, ENOENT)
        elif opcode in (FORGET, BATCH_FORGET, INTERRUPT):
            continue
        else:
            reply(unique, ENOSYS)


threading.Thread(target=serve, daemon=True).start()
time.sleep(seconds)
libc.umount2(target.encode(), 2)  # MNT_DETACH
print("unmounted", flush=True)
