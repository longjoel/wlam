import struct
data=open('benchmarks/minesweeper/bin/minesweeper-x64.exe','rb').read()
u16=lambda o: struct.unpack_from('<H',data,o)[0]
u32=lambda o: struct.unpack_from('<I',data,o)[0]
u64=lambda o: struct.unpack_from('<Q',data,o)[0]
e=u32(0x3c); opt=e+24; ns=u16(e+6); osz=u16(e+20); sec=e+24+osz
secs=[]
for i in range(ns):
    o=sec+i*40
    name=data[o:o+8].rstrip(b'\0').decode(errors='replace')
    secs.append((name,u32(o+12),u32(o+8),u32(o+20),u32(o+16)))
def off(rva, n=8):
    for (name,va,vs,rp,rs) in secs:
        if va <= rva < va+max(vs,rs):
            o=rp+(rva-va)
            if (rva-va) < rs and o+n <= len(data): return o
    return None
rva=0x2b000
for d in range(7):
    o=off(rva+d*20)
    oft=u32(o); nm=u32(o+12); ft=u32(o+16)
    name_off=off(nm,1); lib=bytes(data[name_off:data.find(b'\0',name_off)]).decode()
    # first few thunk slots
    lookup = oft or ft
    vals=[]
    for i in range(4):
        to=off(lookup+i*8,8)
        vals.append(None if to is None else u64(to))
    print(f"rva={rva+d*20:#x} lib={lib} oft={oft:#x} ft={ft:#x} first_thunks={[None if v is None else hex(v) for v in vals]}")
