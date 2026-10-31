import pathlib, random, struct

root = pathlib.Path(__file__).resolve().parent
out = bytearray()
def op(tag, *nums):
    out.extend(tag.encode())
    for n in nums: out.extend(struct.pack("<I", n))
def write(s):
    if isinstance(s,str): s=s.encode()
    op("W",len(s)); out.extend(s)
def dump(): op("D")
def new(cols=80,rows=24): op("N",cols,rows)

# Deterministic coverage for control classification, UTF-8 boundaries,
# cell-mask overwrites, graphemes, charsets, alternate screen and reflow.
new(19,7)
for s in [
    "ASCII " * 60, "\r\nline\tend\b!\a",
    "\x1b[1;3;4;38;2;17;99;201mstyled\x1b[0m",
    "é中界🙂e\u0301👩\u200d💻🇧🇷\ufe0f",
    "\x1b]8;id=hello;https://example.com/path\x1b\\link\x1b]8;;\x1b\\",
    "\x1b[H\x1b[2Koverwrite " * 8,
    "\x1b[2;4r\x1b[?6h\x1b[Hscroll\n" * 20,
    "\x1b[r\x1b[?6l\x1b(0lqqk\x1b(B",
    "\x1b[?1049hALT\x1b[2J\x1b[H中xx\x1b[?1049l",
    "\x1b[4hinsert\x1b[4l\x1b[1P\x1b[2@\x1b[3X",
    "\x1b[6n\x1b[5n\x1b[c\x1b[>c\x1b[?7$p",
    "\x1b]2;test-title\a\x1b]7;file:///tmp/test\x1b\\",
    "\x1b[38:2::25:100:200;4:3mcolor\x1b[0m",
    b"bad\xc0\xaf\xe0\x80\xaf\xf4\x90\x80\x80\xed\xa0\x80\xff!",
    bytes(range(256)),
]:
    write(s); dump()
for cols,rows in [(1,1),(2,2),(5,3),(33,9),(80,24),(215,10)]:
    op("R",cols,rows); write("x中e\u0301 "*200); dump()

# Every byte at each position around the SIMD boundaries, inside an
# otherwise printable run. New terminal for each case isolates parser state.
for n in [3,4,15,16,17,31,32,33]:
    for byte in range(256):
        new(41,4); write(b"A"*n+bytes([byte])+b"z"*40+b"\x1b[0mTAIL"); dump()

tokens=[
    b"abcdefghijklmnop"*8,b"\r\n",b"\t",b"\b",b"\a",
    b"\x1b[H",b"\x1b[2J",b"\x1b[K",b"\x1b[1K",b"\x1b[2K",
    b"\x1b[1P",b"\x1b[2@",b"\x1b[3X",b"\x1b[2L",b"\x1b[1M",
    b"\x1b[1;31m",b"\x1b[0m",b"\x1b[48;2;30;70;90m",
    b"\x1b[?7l",b"\x1b[?7h",b"\x1b[4h",b"\x1b[4l",
    b"\x1b[?1049h",b"\x1b[?1049l",b"\x1b[2;5r",b"\x1b[r",
    b"\x1b[6n",b"\x1b[?25l",b"\x1b[?25h",
    b"\x1b]2;title\a",b"\x1b]8;;https://example.org\x1b\\",
    b"\x1b]8;;\x1b\\",b"\x1b(0",b"\x1b(B",
    "中🙂e\u0301".encode(), "👩\u200d💻".encode(),
    b"\xff\xc2",b"\xe2\x82",b"\xf0\x9f\x99\x82",
]
for seed in range(64):
    rng=random.Random(seed); new(rng.choice([1,2,5,19,80]),rng.choice([2,4,12]))
    for i in range(150):
        if i%37==36: op("R",rng.choice([2,5,19,80]),rng.choice([2,4,12]))
        else: write(rng.choice(tokens))
        if i%5==4: dump()
        if i%41==40: op("K")
    op("C"); dump(); op("K"); write(rng.choice(tokens)); dump()

# Long scrollback exercises page turnover, compression, and restore on read.
new(80,24)
for i in range(3000): write(f"line {i:04d} \x1b[3{i%8}m" + "hello "*10 + "\x1b[0m\r\n")
dump(); op("C"); dump(); op("K"); dump(); write("after clone \x1b]8;;https://x.example\x1b\\link\x1b]8;;\x1b\\\r\n"*50); dump(); op("C"); op("R",37,12); dump(); op("R",120,40); dump()

# Line-limited scrollback reuses pruned pages (spare page) and erases history.
new(180,50)
for i in range(6000): write(f"{i}:" + "y"*(i%170) + "\r\n")
dump(); write("\x1b[3J"); dump(); op("C"); dump(); op("K"); write("tail\r\n"*30); dump()
(pathlib.Path(__import__("sys").argv[1]) if len(__import__("sys").argv)>1 else root/"cases.bin").write_bytes(out)
print(f"{len(out)} input bytes")
