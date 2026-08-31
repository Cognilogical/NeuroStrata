#!/usr/bin/env python3
"""Regenerates icon.ico and icon.icns from icon.png.

Both files were copies of icon.png with a different extension, which the
Windows resource compiler rejects outright ("icon.ico is not in 3.00 format",
RC2175) and which macOS would not accept as an iconset either. This writes real
containers: BMP-encoded entries for the .ico, so any RC version takes it, and
PNG-encoded entries for the .icns, which is the modern Apple format.

Pure standard library on purpose -- Pillow is not a dependency of this repo, and
an icon regenerated once a year should not add one.

    python src-tauri/icons/make_icons.py
"""
import os
import struct
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
SOURCE = os.path.join(HERE, "icon.png")
ICO_SIZES = [16, 32, 48, 64, 128, 256]
# (icns type, pixel size). ic07/ic08/ic09 are the PNG-based entries Apple reads.
ICNS_ENTRIES = [(b"ic11", 32), (b"ic12", 64), (b"ic07", 128), (b"ic08", 256), (b"ic09", 512)]


def read_png(path):
    """Decodes an 8-bit RGBA, non-interlaced PNG into (width, height, bytes)."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("%s is not a PNG" % path)

    width = height = None
    idat = bytearray()
    pos = 8
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if (depth, colour, interlace) != (8, 6, 0):
                raise SystemExit(
                    "only 8-bit RGBA non-interlaced PNG is supported here; got depth=%d colour=%d interlace=%d"
                    % (depth, colour, interlace)
                )
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
        pos += 12 + length

    raw = zlib.decompress(bytes(idat))
    stride = width * 4
    out = bytearray(height * stride)
    previous = bytearray(stride)
    pos = 0
    for y in range(height):
        filter_type = raw[pos]
        pos += 1
        line = bytearray(raw[pos:pos + stride])
        pos += stride
        for x in range(stride):
            a = line[x - 4] if x >= 4 else 0
            b = previous[x]
            c = previous[x - 4] if x >= 4 else 0
            if filter_type == 0:
                value = line[x]
            elif filter_type == 1:
                value = line[x] + a
            elif filter_type == 2:
                value = line[x] + b
            elif filter_type == 3:
                value = line[x] + ((a + b) >> 1)
            elif filter_type == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                value = line[x] + (a if pa <= pb and pa <= pc else (b if pb <= pc else c))
            else:
                raise SystemExit("unknown PNG filter %d" % filter_type)
            line[x] = value & 0xFF
        out[y * stride:(y + 1) * stride] = line
        previous = line

    return width, height, bytes(out)


def resize(pixels, width, height, size):
    """Area-average downscale. Good enough for an icon, and dependency-free."""
    out = bytearray(size * size * 4)
    for y in range(size):
        y0, y1 = y * height // size, max(y * height // size + 1, (y + 1) * height // size)
        for x in range(size):
            x0, x1 = x * width // size, max(x * width // size + 1, (x + 1) * width // size)
            r = g = b = a = n = 0
            for sy in range(y0, y1):
                row = sy * width * 4
                for sx in range(x0, x1):
                    i = row + sx * 4
                    alpha = pixels[i + 3]
                    # Weight colour by alpha so transparent edges do not drag
                    # the visible pixels toward black.
                    r += pixels[i] * alpha
                    g += pixels[i + 1] * alpha
                    b += pixels[i + 2] * alpha
                    a += alpha
                    n += 1
            o = (y * size + x) * 4
            if a:
                out[o], out[o + 1], out[o + 2] = r // a, g // a, b // a
            out[o + 3] = a // n if n else 0
    return bytes(out)


def write_png(pixels, size):
    def chunk(kind, body):
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))

    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filter: none
        raw += pixels[y * size * 4:(y + 1) * size * 4]

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


def write_bmp_entry(pixels, size):
    """A single ICO image: BITMAPINFOHEADER, BGRA bottom-up, then an AND mask."""
    header = struct.pack(
        "<IiiHHIIiiII",
        40,            # header size
        size,          # width
        size * 2,      # height: colour data plus the mask
        1,             # planes
        32,            # bits per pixel
        0,             # BI_RGB
        size * size * 4,
        0, 0, 0, 0,
    )

    body = bytearray()
    for y in range(size - 1, -1, -1):
        row = y * size * 4
        for x in range(size):
            i = row + x * 4
            body += bytes((pixels[i + 2], pixels[i + 1], pixels[i], pixels[i + 3]))

    # The alpha channel carries transparency; the mask stays all-opaque, but it
    # has to be present and 4-byte aligned per row or RC rejects the icon.
    mask_row = ((size + 31) // 32) * 4
    body += bytes(mask_row * size)

    return header + bytes(body)


def main():
    width, height, pixels = read_png(SOURCE)
    print("source: %s (%dx%d RGBA)" % (os.path.basename(SOURCE), width, height))

    scaled = {}
    for size in sorted(set(ICO_SIZES + [s for _, s in ICNS_ENTRIES])):
        scaled[size] = pixels if size == width == height else resize(pixels, width, height, size)
        print("  scaled %d" % size)

    images = [(size, write_bmp_entry(scaled[size], size)) for size in ICO_SIZES]
    offset = 6 + 16 * len(images)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(images)))
    for size, blob in images:
        directory += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0, 0, 1, 32, len(blob), offset,
        )
        offset += len(blob)
    ico = bytes(directory) + b"".join(blob for _, blob in images)
    open(os.path.join(HERE, "icon.ico"), "wb").write(ico)
    print("wrote icon.ico (%d bytes, sizes %s)" % (len(ico), ICO_SIZES))

    payloads = [(kind, write_png(scaled[size], size)) for kind, size in ICNS_ENTRIES]
    total = 8 + sum(8 + len(p) for _, p in payloads)
    icns = bytearray(b"icns" + struct.pack(">I", total))
    for kind, payload in payloads:
        icns += kind + struct.pack(">I", 8 + len(payload)) + payload
    open(os.path.join(HERE, "icon.icns"), "wb").write(bytes(icns))
    print("wrote icon.icns (%d bytes, %d entries)" % (len(icns), len(payloads)))


if __name__ == "__main__":
    sys.exit(main())
