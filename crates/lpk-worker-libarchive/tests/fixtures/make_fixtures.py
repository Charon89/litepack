"""Make the libarchive worker's fixtures (E4-4). Run from anywhere:

    python -I make_fixtures.py <out-dir> <scratch-dir>

The source tree is generated deterministically into <scratch-dir>/src. RAR fixtures come from
WinRAR's `rar` (found on PATH, or the RAR environment variable), CAB fixtures from Windows'
`makecab` (on PATH), and the ISO 9660, ar, cpio, uuencode and `.Z` fixtures from the generators
below (written for this project; no tool needed). See README.md for the tools and versions used.
"""

import os
import shutil
import struct
import subprocess
import sys
import tarfile

MTIME = 1704164645  # 2024-01-02 03:04:05 UTC


def lcg_bytes(n, seed):
    out = bytearray(n)
    x = seed
    for i in range(n):
        x = (x * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        out[i] = x >> 56
    return bytes(out)


TREE = {
    "hello.txt": b"Hello from LitePack's libarchive fixtures.\n",
    "Mixed Case Name.txt": b"A long, mixed-case name with spaces.\n" * 3,
    "sub/nested.txt": b"nested file\n" * 40,
    "data.bin": lcg_bytes(48_000, 7),
}


def write_tree(root):
    if os.path.exists(root):
        shutil.rmtree(root)
    for rel, data in TREE.items():
        p = os.path.join(root, *rel.split("/"))
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "wb") as f:
            f.write(data)
        os.utime(p, (MTIME, MTIME))
    os.utime(os.path.join(root, "sub"), (MTIME, MTIME))


def tool(name, env):
    p = os.environ.get(env) or shutil.which(name)
    if not p:
        sys.exit(f"{name} not found (PATH or {env})")
    return p


def run(cmd, cwd):
    subprocess.run(cmd, cwd=cwd, check=True, stdout=subprocess.DEVNULL)


def make_rar(out, src):
    rar = tool("rar", "RAR")
    # -idq quiet, -r recurse, -m3 normal compression, -ep1 names relative to the tree, -ma5 RAR 5
    # (RAR 7.00 no longer writes RAR 4: see make_rar4), -v volume size, -p/-hp passwords.
    base = ["a", "-idq", "-r", "-m3", "-ep1"]
    jobs = {
        "rar5.rar": ["-ma5"],
        "rar5-vol.rar": ["-ma5", "-v20k"],
        "gap-rar5-encrypted.rar": ["-ma5", "-pLitePack"],
        "gap-rar5-encrypted-headers.rar": ["-ma5", "-hpLitePack"],
    }
    for name, opts in jobs.items():
        stem = name[: -len(".rar")]
        for f in os.listdir(out):
            if f.startswith(stem + ".") or f == name:
                os.remove(os.path.join(out, f))
        run([rar] + base + opts + [os.path.join(out, name)] + sorted(os.listdir(src)), src)
    # A self-extracting layout without WinRAR's SFX module (not redistributable): a minimal PE
    # stub of our own followed by the RAR 5 archive.
    stub = bytearray(4096)
    stub[0:2] = b"MZ"
    struct.pack_into("<I", stub, 0x3C, 0x80)
    stub[0x80:0x84] = b"PE\0\0"
    with open(os.path.join(out, "rar5.rar"), "rb") as f:
        body = f.read()
    with open(os.path.join(out, "rar5-sfx.sfx"), "wb") as f:
        f.write(bytes(stub) + body)


# --- RAR 4, stored (RAR 7.00 cannot write RAR 4) -------------------------------------------------
# From RARLAB's RAR 4.x format description (technote.txt): a marker block, a main header, one
# file header per member (stored, METHOD 0x30, UNP_VER 29) with its data, an end block. Each
# header's HEAD_CRC is the low 16 bits of the CRC-32 of the header from HEAD_TYPE on.


def rar4_block(ty, flags, body):
    import zlib

    size = 7 + len(body)
    head = struct.pack("<BHH", ty, flags, size) + body
    return struct.pack("<H", zlib.crc32(head) & 0xFFFF) + head


def dos_time(t):
    import time as _t

    tm = _t.gmtime(t)
    return (
        ((tm.tm_year - 1980) << 25)
        | (tm.tm_mon << 21)
        | (tm.tm_mday << 16)
        | (tm.tm_hour << 11)
        | (tm.tm_min << 5)
        | (tm.tm_sec // 2)
    )


def rar4_file(name, data, flags=0, unp_ver=29, method=0x30, part=None):
    """A file header and its data. For a split member, `part` is this volume's piece: its
    header carries the piece's CRC-32 unless it is the last piece (flag 0x0002 clear), which
    carries the whole file's."""
    import zlib

    nb = name.replace("/", "\\").encode()
    is_dir = flags & 0x00E0 == 0x00E0
    piece = data if part is None else part
    crc = zlib.crc32(piece) if flags & 0x0002 else zlib.crc32(data)
    body = (
        struct.pack(
            "<IIBIIBBHI",
            len(piece),
            len(data),
            2,
            crc,
            dos_time(MTIME),
            unp_ver,
            method,
            len(nb),
            0x10 if is_dir else 0x20,
        )
        + nb
    )
    return rar4_block(0x74, 0x8000 | flags, body) + piece


def rar4_archive(members, main_flags=0, end_flags=0):
    out = b"Rar!\x1a\x07\x00" + rar4_block(0x73, main_flags, b"\0" * 6)
    for m in members:
        out += m
    return out + rar4_block(0x7B, 0x4000 | end_flags, b"")


def make_rar4(out):
    members = [rar4_file("sub", b"", flags=0x00E0)]
    for rel in sorted(TREE):
        members.append(rar4_file(rel, TREE[rel]))
    with open(os.path.join(out, "rar4.rar"), "wb") as f:
        f.write(rar4_archive(members))
    # Volumes, old naming (.rar, .r00, .r01): data.bin split in three, hello.txt whole in the
    # first.
    data = TREE["data.bin"]
    cuts = [0, 20_000, 40_000, len(data)]
    for i in range(3):
        part = data[cuts[i] : cuts[i + 1]]
        flags = (0x0001 if i > 0 else 0) | (0x0002 if i < 2 else 0)
        ms = []
        if i == 0:
            ms.append(rar4_file("hello.txt", TREE["hello.txt"]))
        ms.append(rar4_file("data.bin", data, flags=flags, part=part))
        main = 0x0001 | (0x0100 if i == 0 else 0)
        end = 0x0001 if i < 2 else 0
        name = "rar4-vol.rar" if i == 0 else f"rar4-vol.r{i - 1:02d}"
        with open(os.path.join(out, name), "wb") as f:
            f.write(rar4_archive(ms, main_flags=main, end_flags=end))
    hello = TREE["hello.txt"]
    gaps = {
        "gap-rar4-solid.rar": rar4_archive([rar4_file("hello.txt", hello)], main_flags=0x0008),
        "gap-rar4-encrypted-headers.rar": rar4_archive(
            [rar4_file("hello.txt", hello)], main_flags=0x0080
        ),
        "gap-rar4-encrypted.rar": rar4_archive([rar4_file("hello.txt", hello, flags=0x0004)]),
        "gap-rar20.rar": rar4_archive([rar4_file("hello.txt", hello, unp_ver=20, method=0x33)]),
    }
    for name, blob in gaps.items():
        with open(os.path.join(out, name), "wb") as f:
            f.write(blob)


def make_cab(out, src, scratch):
    makecab = tool("makecab", "MAKECAB")
    for name, comp in (("cab-stored.cab", "OFF"), ("cab-mszip.cab", "MSZIP"), ("cab-lzx.cab", "LZX")):
        ddf = os.path.join(scratch, name + ".ddf")
        lines = [
            ".Option Explicit",
            f".Set CabinetNameTemplate={name}",
            f".Set DiskDirectoryTemplate={out}",
            ".Set Cabinet=on",
            ".Set MaxDiskSize=0",
            f".Set Compress={'off' if comp == 'OFF' else 'on'}",
            ".Set CompressionType=" + ("MSZIP" if comp == "OFF" else comp),
            ".Set CompressionMemory=21" if comp == "LZX" else "",
            f".Set RptFileName={os.path.join(scratch, 'setup.rpt')}",
            f".Set InfFileName={os.path.join(scratch, 'setup.inf')}",
            ".Set UniqueFiles=on",
        ]
        for rel in sorted(TREE):
            lines.append(f'"{os.path.join(src, *rel.split("/"))}" "{rel.replace("/", chr(92))}"')
        with open(ddf, "w", newline="\r\n") as f:
            f.write("\n".join(l for l in lines if l) + "\n")
        run([makecab, "/F", ddf], scratch)


# --- ISO 9660 (with Rock Ridge or Joliet) -------------------------------------------------------

SECTOR = 2048


def both16(v):
    return struct.pack("<H", v) + struct.pack(">H", v)


def both32(v):
    return struct.pack("<I", v) + struct.pack(">I", v)


def rec_date():
    # 2024-01-02 03:04:05, GMT offset 0.
    return bytes([124, 1, 2, 3, 4, 5, 0])


def dec_date():
    return b"2024010203040500" + b"\0"


def dir_record(name_bytes, lba, size, is_dir, su=b""):
    n = len(name_bytes)
    pad = b"\0" if n % 2 == 0 else b""
    body = (
        both32(lba)
        + both32(size)
        + rec_date()
        + bytes([2 if is_dir else 0, 0, 0])
        + both16(1)
        + bytes([n])
        + name_bytes
        + pad
        + su
    )
    rec = bytes([len(body) + 2, 0]) + body
    assert len(rec) <= 255
    return rec


def rr_entries(name, mode, links, root=False):
    su = b""
    if root:
        su += b"SP" + bytes([7, 1, 0xBE, 0xEF, 0])
        su += b"RR" + bytes([5, 1, 0x89])
    if name is not None:
        nb = name.encode()
        su += b"NM" + bytes([5 + len(nb), 1, 0]) + nb
    su += b"PX" + bytes([36, 1]) + both32(mode) + both32(links) + both32(0) + both32(0)
    return su


def short_name(rel_name, is_dir):
    stem, dot, ext = rel_name.upper().partition(".")
    stem = "".join(c if c.isalnum() else "_" for c in stem)[:8]
    if is_dir:
        return stem.encode()
    ext = "".join(c if c.isalnum() else "_" for c in ext)[:3]
    return f"{stem}.{ext};1".encode()


def make_iso(path, joliet):
    files = sorted(TREE)
    dirs = ["", "sub"]
    # Layout: 16 system sectors, PVD, [SVD], terminator, path tables (L, M) per tree, one sector
    # per directory per tree, then file data.
    trees = 2 if joliet else 1
    next_lba = 16 + 1 + (1 if joliet else 0) + 1
    pt_lba = []
    for _ in range(trees):
        pt_lba.append((next_lba, next_lba + 1))
        next_lba += 2
    dir_lba = []
    for _ in range(trees):
        dir_lba.append({d: next_lba + i for i, d in enumerate(dirs)})
        next_lba += len(dirs)
    file_lba = {}
    for rel in files:
        file_lba[rel] = next_lba
        next_lba += max(1, -(-len(TREE[rel]) // SECTOR))
    total = next_lba
    img = bytearray(total * SECTOR)

    def put(lba, data):
        img[lba * SECTOR : lba * SECTOR + len(data)] = data

    for rel in files:
        put(file_lba[rel], TREE[rel])

    for t in range(trees):
        jol = t == 1
        rr = not joliet

        def ident(name, is_dir):
            if jol:
                return name.encode("utf-16-be")
            return short_name(name, is_dir)

        for d in dirs:
            parent = "" if d == "" else ""
            entries = [(b"\0", dir_lba[t][d], True, "."), (b"\1", dir_lba[t][parent], True, "..")]
            kids = []
            for rel in files:
                head, _, base = rel.rpartition("/")
                if head == d:
                    kids.append((ident(base, False), file_lba[rel], False, base, len(TREE[rel])))
            if d == "":
                kids.append((ident("sub", True), dir_lba[t]["sub"], True, "sub", SECTOR))
            kids.sort(key=lambda k: k[0])
            out = b""
            for name_b, lba, is_dir, special in entries:
                su = b""
                if rr:
                    su = rr_entries(None, 0o40755, 2, root=(d == "" and special == "."))
                out += dir_record(name_b, lba, SECTOR, True, su)
            for name_b, lba, is_dir, real, size in kids:
                su = b""
                if rr:
                    su = rr_entries(real, 0o40755 if is_dir else 0o100644, 2 if is_dir else 1)
                out += dir_record(name_b, lba, size, is_dir, su)
            assert len(out) <= SECTOR
            put(dir_lba[t][d], out)

        # Path tables: root (number 1), then sub (parent 1).
        def pt(big):
            o = b""
            for i, d in enumerate(dirs):
                name_b = b"\0" if d == "" else ident(d, True)
                lba = dir_lba[t][d]
                o += bytes([len(name_b), 0])
                o += struct.pack(">I" if big else "<I", lba)
                o += struct.pack(">H" if big else "<H", 1)
                o += name_b + (b"\0" if len(name_b) % 2 else b"")
            return o

        lpt, mpt = pt(False), pt(True)
        put(pt_lba[t][0], lpt)
        put(pt_lba[t][1], mpt)

        vd = bytearray(SECTOR)
        vd[0] = 2 if jol else 1
        vd[1:6] = b"CD001"
        vd[6] = 1

        def text(at, n, s):
            if jol:
                b = s.encode("utf-16-be")[:n].ljust(n, b"\0")
                # pad with UCS-2 spaces
                b = s.encode("utf-16-be") + b"\0 " * ((n - 2 * len(s)) // 2)
                b = b[:n]
            else:
                b = s.encode()[:n].ljust(n, b" ")
            vd[at : at + n] = b

        text(8, 32, "")
        text(40, 32, "LPK_FIXTURE")
        vd[80:88] = both32(total)
        if jol:
            vd[88:91] = b"%/E"
        vd[120:124] = both16(1)
        vd[124:128] = both16(1)
        vd[128:132] = both16(SECTOR)
        vd[132:140] = both32(len(lpt))
        vd[140:144] = struct.pack("<I", pt_lba[t][0])
        vd[148:152] = struct.pack(">I", pt_lba[t][1])
        root_su = b""
        vd[156:190] = dir_record(b"\0", dir_lba[t][""], SECTOR, True, root_su)
        for at, n in ((190, 128), (318, 128), (446, 128), (574, 128), (702, 37), (739, 37), (776, 37)):
            text(at, n, "")
        vd[813:830] = dec_date()
        vd[830:847] = dec_date()
        vd[847:864] = b"0" * 16 + b"\0"
        vd[864:881] = b"0" * 16 + b"\0"
        vd[881] = 1
        put(16 + t, bytes(vd))

    term = bytearray(SECTOR)
    term[0] = 255
    term[1:6] = b"CD001"
    term[6] = 1
    put(16 + trees, bytes(term))
    with open(path, "wb") as f:
        f.write(bytes(img))


# --- ar, cpio, uuencode, compress ---------------------------------------------------------------


def make_ar(path):
    out = b"!<arch>\n"
    for rel in ("hello.txt", "data.bin"):
        data = TREE[rel]
        hdr = f"{rel + '/':<16}{MTIME:<12}{0:<6}{0:<6}{'100644':<8}{len(data):<10}`\n"
        assert len(hdr) == 60
        out += hdr.encode() + data + (b"\n" if len(data) % 2 else b"")
    with open(path, "wb") as f:
        f.write(out)


def make_cpio(path):
    out = b""

    def entry(ino, name, mode, data):
        nb = name.encode() + b"\0"
        fields = [ino, mode, 0, 0, 1, MTIME, len(data), 0, 0, 0, 0, len(nb), 0]
        h = b"070701" + b"".join(b"%08X" % v for v in fields) + nb
        h += b"\0" * (-len(h) % 4)
        return h + data + b"\0" * (-len(data) % 4)

    out += entry(1, "sub", 0o40755, b"")
    for i, rel in enumerate(sorted(TREE)):
        out += entry(2 + i, rel, 0o100644, TREE[rel])
    out += entry(0, "TRAILER!!!", 0, b"")
    out += b"\0" * (-len(out) % 512)
    with open(path, "wb") as f:
        f.write(out)


def make_uue(path, name, data):
    import binascii

    lines = [f"begin 644 {name}\n".encode()]
    for i in range(0, len(data), 45):
        lines.append(binascii.b2a_uu(data[i : i + 45], backtick=True))
    lines.append(b"`\nend\n")
    with open(path, "wb") as f:
        f.write(b"".join(lines))


def lzw_compress(data):
    """Unix compress (`.Z`), block mode, 16-bit maximum; the inputs used here stay below 512
    dictionary entries, so every code is 9 bits wide (asserted)."""
    dictionary = {bytes([i]): i for i in range(256)}
    free = 257
    codes = []
    w = b""
    for c in data:
        wc = w + bytes([c])
        if wc in dictionary:
            w = wc
            continue
        codes.append(dictionary[w])
        dictionary[wc] = free
        free += 1
        w = bytes([c])
    if w:
        codes.append(dictionary[w])
    assert free <= 511, "input too long for 9-bit codes"
    acc = 0
    nbits = 0
    out = bytearray(b"\x1f\x9d\x90")
    for code in codes:
        acc |= code << nbits
        nbits += 9
        while nbits >= 8:
            out.append(acc & 0xFF)
            acc >>= 8
            nbits -= 8
    if nbits:
        out.append(acc & 0xFF)
    return bytes(out)


def make_tar(src_names):
    import io

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as t:
        for rel in src_names:
            info = tarfile.TarInfo(rel)
            info.size = len(TREE[rel])
            info.mtime = MTIME
            info.mode = 0o644
            t.addfile(info, io.BytesIO(TREE[rel]))
    return buf.getvalue()


def main():
    out, scratch = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    os.makedirs(out, exist_ok=True)
    os.makedirs(scratch, exist_ok=True)
    src = os.path.join(scratch, "src")
    write_tree(src)
    make_rar(out, src)
    make_rar4(out)
    make_cab(out, src, scratch)
    make_iso(os.path.join(out, "iso-rockridge.iso"), joliet=False)
    make_iso(os.path.join(out, "iso-joliet.iso"), joliet=True)
    make_ar(os.path.join(out, "fixture.a"))
    make_cpio(os.path.join(out, "fixture.cpio"))
    make_uue(os.path.join(out, "hello.txt.uue"), "hello.txt", TREE["hello.txt"])
    with open(os.path.join(out, "hello.txt.Z"), "wb") as f:
        f.write(lzw_compress(TREE["hello.txt"]))
    with open(os.path.join(out, "small.tar.Z"), "wb") as f:
        f.write(lzw_compress(make_tar(["hello.txt"])))


if __name__ == "__main__":
    main()
