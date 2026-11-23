#!/usr/bin/env python3
"""Minimal ELF32 -> UF2 converter for RP2350 (Arm secure), no dependencies.

Takes PT_LOAD segments with FileSize > 0 whose physical address is in flash
(0x10000000..0x10400000), packs them into 256-byte UF2 payloads, and tags every
block with family ID 0xe48bff59 (RP2350_ARM_S) and the familyID-present flag.
Gaps inside a 256-byte page are zero-filled; pages are emitted in address order.

Usage: elf2uf2.py input.elf output.uf2
"""
import struct, sys

UF2_MAGIC0, UF2_MAGIC1, UF2_MAGIC_END = 0x0A324655, 0x9E5D5157, 0x0AB16F30
FLAG_FAMILY = 0x00002000
RP2350_ARM_S = 0xE48BFF59
FLASH_LO, FLASH_HI = 0x10000000, 0x10000000 + 4 * 1024 * 1024
PAGE = 256

def segments(elf):
    assert elf[:4] == b'\x7fELF' and elf[4] == 1 and elf[5] == 1, 'need ELF32 little-endian'
    e_phoff, = struct.unpack_from('<I', elf, 0x1C)
    e_phentsize, e_phnum = struct.unpack_from('<HH', elf, 0x2A)
    for i in range(e_phnum):
        p_type, p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_flags, p_align = \
            struct.unpack_from('<8I', elf, e_phoff + i * e_phentsize)
        if p_type == 1 and p_filesz > 0:
            yield p_paddr, elf[p_offset:p_offset + p_filesz]

def main(src, dst):
    elf = open(src, 'rb').read()
    pages = {}
    for addr, data in segments(elf):
        if not (FLASH_LO <= addr and addr + len(data) <= FLASH_HI):
            sys.exit(f'segment 0x{addr:08x}+{len(data)} is outside flash; refusing')
        for off in range(len(data)):
            a = addr + off
            pg = pages.setdefault(a & ~(PAGE - 1), bytearray(PAGE))
            pg[a % PAGE] = data[off]
    keys = sorted(pages)
    out = bytearray()
    for n, base in enumerate(keys):
        hdr = struct.pack('<8I', UF2_MAGIC0, UF2_MAGIC1, FLAG_FAMILY, base, PAGE, n, len(keys), RP2350_ARM_S)
        blk = hdr + bytes(pages[base]) + bytes(476 - PAGE) + struct.pack('<I', UF2_MAGIC_END)
        assert len(blk) == 512
        out += blk
    open(dst, 'wb').write(out)
    print(f'{dst}: {len(keys)} blocks, 0x{keys[0]:08x}..0x{keys[-1] + PAGE:08x}, family 0x{RP2350_ARM_S:08x}')

if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
