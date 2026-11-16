"""Adds iTunes freeform atoms (`----`, with a `mean` of com.apple.iTunes, a
`name` and a UTF-8 `data`) to the end of an MP4 file's ilst, as Picard
writes MusicBrainz identifiers into an M4A. ffmpeg cannot write them: it
drops a key it does not know, or writes a QuickTime `mdta` key instead.

The MP4 must have its moov after its mdat, so that no chunk offset moves,
and must already have a moov/udta/meta/ilst.

Usage: python3 freeform.py <in.m4a> <out.m4a> <name=value>...
"""

import struct
import sys

CONTAINERS = {"moov": 8, "udta": 8, "meta": 12, "ilst": 8}


def box(kind: bytes, body: bytes) -> bytes:
    return struct.pack(">I", 8 + len(body)) + kind + body


def freeform(name: str, value: str) -> bytes:
    mean = box(b"mean", b"\0\0\0\0" + b"com.apple.iTunes")
    named = box(b"name", b"\0\0\0\0" + name.encode())
    data = box(b"data", struct.pack(">II", 1, 0) + value.encode())
    return box(b"----", mean + named + data)


def find(data: bytes, start: int, end: int, kind: str) -> int:
    at = start
    while at < end:
        size, found = struct.unpack(">I4s", data[at : at + 8])
        if found.decode("latin-1") == kind:
            return at
        at += size
    raise SystemExit(f"no {kind} box")


def main() -> None:
    source, target, *pairs = sys.argv[1:]
    data = bytearray(open(source, "rb").read())
    path, start, end = [], 0, len(data)
    for kind in ["moov", "udta", "meta", "ilst"]:
        at = find(data, start, end, kind)
        size = struct.unpack(">I", data[at : at + 4])[0]
        path.append(at)
        start, end = at + CONTAINERS[kind], at + size
    if find(data, 0, len(data), "mdat") > path[0]:
        raise SystemExit("the moov comes before the mdat")
    added = b"".join(freeform(*pair.split("=", 1)) for pair in pairs)
    data[end:end] = added
    for at in path:
        size = struct.unpack(">I", data[at : at + 4])[0]
        data[at : at + 4] = struct.pack(">I", size + len(added))
    open(target, "wb").write(bytes(data))


main()
