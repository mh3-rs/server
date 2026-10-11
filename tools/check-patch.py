#!/usr/bin/env python3
"""Checks a generated patch against a disc's main.dol, outside CI (game data never enters a repository).

For each <memory> line of a `wit` XML from `mh3sp patch` (wit/<domain>-<game>.xml): the address lies in one of the
DOL's sections, the bytes there are the line's `original` (the string the patch replaces), and the value fits: past
the original, only padding (zeros) up to its end, and no overlap with the next edit. The CA's line has no original:
its address must hold a 924-byte DER certificate (30 82 03 98).
Exits 1 on any mismatch.

Usage: tools/check-patch.py <main.dol> <wit xml>   (main.dol from `nodtool extract <disc> <scratch dir>`)
"""
import re
import struct
import sys

dol = open(sys.argv[1], "rb").read()
xml = open(sys.argv[2]).read()

# The DOL header: 7 text and 11 data sections, their file offsets, addresses and sizes.
offsets = struct.unpack(">18I", dol[0x00:0x48])
addresses = struct.unpack(">18I", dol[0x48:0x90])
sizes = struct.unpack(">18I", dol[0x90:0xD8])


def file_offset(address):
    for offset, start, size in zip(offsets, addresses, sizes):
        if size and start <= address < start + size:
            return offset + address - start
    return None


edits = []
for line in re.findall(r"<memory [^>]*/>", xml):
    attrs = dict(re.findall(r'(\w+)="([^"]*)"', line))
    edits.append((int(attrs["offset"], 16), bytes.fromhex(attrs["value"]), bytes.fromhex(attrs.get("original", ""))))
edits.sort()

failed = False
for i, (address, value, original) in enumerate(edits):
    at = file_offset(address)
    expected = original or bytes.fromhex("30820398")
    found = None if at is None else dol[at : at + len(expected)]
    spills = i + 1 < len(edits) and address + len(value) > edits[i + 1][0]
    # A string's value runs past its original into what must be padding; the CA replaces 924 bytes with 924.
    tail = b"" if at is None or not original else dol[at + len(original) : at + len(value)]
    spills |= any(tail)
    ok = found == expected and not spills
    failed |= not ok
    what = original.decode() if original else "the CA (924 bytes)"
    print(f"{'ok  ' if ok else 'FAIL'} 0x{address:08X} {what}" + (" (overwrites more than padding)" if spills else ""))
sys.exit(1 if failed else 0)
