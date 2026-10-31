"""Operation logs for compare.c.

    generate.py [OUT]                         the fixed corpus run.sh replays
    generate.py --seed S [--count N] [OUT]    N random cases for seeds S..S+N-1

A log is a list of ops: a tag byte, little-endian u32 arguments and, for W,
the bytes to write. Tags: N new terminal, W write, R resize, C compress,
K continue on a screen clone, D dump a snapshot. Random cases also use
F extra callbacks and options, V scroll the viewport, X full reset,
L scrollback limit, P clip and overscan the main render state, S select
and T track a grid point.
fuzz.py imports fuzz_case(), parse() and serialize().
"""
import base64, pathlib, random, struct, sys

root = pathlib.Path(__file__).resolve().parent
ARGS = {"N": 2, "W": 1, "R": 2, "C": 0, "K": 0, "D": 0, "F": 2, "V": 2, "X": 0, "L": 2, "P": 5, "S": 3, "T": 2}


class Log:
    def __init__(self):
        self.out = bytearray()

    def op(self, tag, *nums):
        self.out.extend(tag.encode())
        for n in nums: self.out.extend(struct.pack("<I", n & 0xFFFFFFFF))

    def write(self, s):
        if isinstance(s, str): s = s.encode()
        self.op("W", len(s)); self.out.extend(s)

    def dump(self): self.op("D")
    def new(self, cols=80, rows=24): self.op("N", cols, rows)


def parse(data):
    """Split a log into (tag, args, payload) tuples."""
    ops, i = [], 0
    while i < len(data):
        tag = chr(data[i]); i += 1
        n = ARGS[tag]
        args = list(struct.unpack_from("<%dI" % n, data, i)); i += 4 * n
        payload = b""
        if tag == "W":
            payload = bytes(data[i:i + args[0]]); i += args[0]
        ops.append((tag, args, payload))
    return ops


def serialize(ops):
    log = Log()
    for tag, args, payload in ops:
        if tag == "W": log.write(payload)
        else: log.op(tag, *args)
    return bytes(log.out)


def corpus():
    log = Log(); op, write, dump, new = log.op, log.write, log.dump, log.new
    # Deterministic coverage for control classification, UTF-8 boundaries,
    # cell-mask overwrites, graphemes, charsets, alternate screen and reflow.
    new(19,7)
    for s in [
        "ASCII " * 60, "\r\nline\tend\b!\a",
        "\x1b[1;3;4;38;2;17;99;201mstyled\x1b[0m",
        "é中界🙂é👩‍💻🇧🇷️",
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
        op("R",cols,rows); write("x中é "*200); dump()

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
        "中🙂é".encode(), "👩‍💻".encode(),
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
    return bytes(log.out)


# ---------------------------------------------------------------------------
# Random cases. Every choice comes from one seeded generator, so a seed always
# produces the same log.

ESC = "\x1b"
ST_CHOICES = ["\x1b\\", "\x1b\\", "\a", "\a", "\x9c", "\x18", "\x1a", ""]
DEC_MODES = [1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 25, 40, 45, 47, 66, 67, 69, 80, 1000, 1001, 1002,
             1003, 1004, 1005, 1006, 1007, 1015, 1016, 1035, 1036, 1039, 1045, 1047, 1048, 1049,
             2004, 2026, 2027, 2031, 2048, 5522, 7727, 8452]
ANSI_MODES = [2, 4, 12, 20, 34]
WIDE = "中界한글日本語あア漢字ｗＡ"
EMOJI = ["🙂", "👍", "👍🏽", "❤️", "❤︎", "☺️", "1️⃣", "🇧🇷", "🇺🇸", "🇦",
         "👩‍💻", "👨‍👩‍👧‍👦", "🏳️‍🌈", "🏴\U000e0067\U000e0062\U000e0065\U000e006e\U000e0067\U000e007f",
         "🧑🏿‍🦰", "\U0010eeee̅̍"]
COMBINING = ["́", "̀", "̈", "⃝", "̶", "‍", "️", "︎",
             "्", "ि", "ั", "〪", "\U0001f3fb", "ᅡ", "ᆨ"]
ODD = ["​", "﻿", "­", " ", "", "\U0010ffff", "\u0085", "\u009b",
       "\u009d", "\u0090", "\u009c", "ก", "क", "ᄀ", "ㅤ", "�", " ",
       "⁠", "؀", "\U0001f1e6", "䷀", "　", "〿"]
BAD_UTF8 = [b"\xc0\xaf", b"\xe0\x80\xaf", b"\xed\xa0\x80", b"\xed\xbf\xbf", b"\xf4\x90\x80\x80",
            b"\xf8\x88\x80\x80\x80", b"\xff", b"\xfe", b"\x80", b"\xbf", b"\xc2", b"\xe2\x82",
            b"\xf0\x9f\x99", b"\xc3\x28", b"\xe2\x28\xa1", b"\xf0\x28\x8c\xbc", b"\xc1\xbf",
            b"\xf5\x80\x80\x80", b"\x9b", b"\x9d", b"\x90", b"\x9c", b"\x84"]
OSC_BODIES = [
    "0;title", "1;icon", "2;hello", "0;red;0;blue", "2;", "2;" + "x" * 300, "3;x", "3008;start=myctx;user=lennart",
    "3008;end=myctx;exit=success;status=0", "3008;start=", "30;x", "300;x",
    "4;1;rgb:ff/00/00", "4;1;?", "4;0;#00ff00;1;rgb:12/34/56", "4;300;red", "4;a;b;c", "4;", "4;5",
    "4;255;rgbi:0.5/0.5/0.5", "4;17;#fff", "5;0;rgb:ff/ff/ff", "5;1;?", "104", "104;1", "104;1;2;300",
    "105", "105;0", "10;?", "10;rgb:aa/bb/cc", "10;#123", "10;abc", "11;?", "11;#000000", "12;?",
    "12;red", "13;?", "14;?", "17;?", "19;#ff00ff", "10;?;?;?", "110", "111", "112", "117", "119",
    "7;file:///tmp/example", "7;file://host/a%20b", "7;/not/a/url", "7;", "7;kitty-shell-cwd://h/x",
    "8;;", "8;;http://example.com", "8;id=foo;http://example.com", "8;=value:id=foo;http://example.com",
    "8;id=foo;", "8;id=" + "q" * 200 + ";http://x", "8;;" + "https://e.example/" + "p" * 500,
    "9;Hello world", "9;4;1;94", "9;4;0;100", "9;4;4", "9;4;3", "9;4;2;50", "9;4;5", "9;4;1;4_2",
    "9;9;/tmp/x", "9;1;foo", "9;2;hello", "9;3;", "9;10;2", "9;H",
    "21;foreground=?", "21;background=#ff0000;cursor=?", "21;", "21;selection_background=red",
    "22;pointer", "22;text", "22;not-a-shape", "22;", "22;default",
    "52;c;aGVsbG8=", "52;c;?", "52;;?", "52;p;d29ybGQ=", "52;s;", "52;c;!!notbase64", "52;abcd",
    "52;cpqs;Zm9v", "55;x", "66;s=2;kurwa", "66;w=8:v=3:n=16;", "66;s=0_2;text", "66;;\n",
    "72;t=a", "72;;payload", "72;t=q", "99;;foobar", "99;i=bobr:d=0;foobar", "99;p=?;foobar",
    "99;a=report,focus;foobar", "99;o=invisible;foobar", "99;u=1;foobar", "99;e=1;Zm9v",
    "133;A", "133;A;aid=14", "133;A;cl=m", "133;A;redraw=0", "133;B", "133;C", "133;C;cmdline=ls -l",
    "133;C;cmdline_url=echo%20hi", "133;D", "133;D;0", "133;D;12;aid=foo", "133;D;-4_2", "133;L",
    "133;N;aid=foo;cl=line", "133;P;k=c", "133;P;k=i", "133;P;k=r", "133;P;k=s", "133;I", "133;Bextra",
    "633;A", "633;E;ls", "633;P;Cwd=/tmp",
    "777;notify;Title;Body", "777;notify;Title", "777;other",
    "1337;CurrentDir=/tmp", "1337;Copy=:YWJjMTIz", "1337;Copy=:?", "1337;SetUserVar=foo=YmFy",
    "1337;SetBadgeFormat=abc", "1337;BobrKurwa",
    "5522;type=read:mime=dGV4dC9wbGFpbg==", "5522;type=read:status=DATA:mime=dGV4dC9wbGFpbg==",
    "5522;type=write", "5522;type=wdata:mime=dGV4dC9wbGFpbg==;R2hvc3R0eQ==", "5522;type=wdata",
    "5522;type=write:status=DONE", "5522;type=read", "5522;id=*42*", "5522;;",
    "7501;?", "7501;state=working:id=sync:progress=40", "7501;state=done:msg=UGxhbg",
    "7501;state=blocked:kind=permission:progress=42:id=tf/plan:app=terraform", "7501;state=clear:id=",
    "7501;state=idle:app=a:state=done:app=b", "7501;state=sleeping", "7501;",
    "50;font", "46;log", "6;x", "113", "1;", "999;x", ";", "", "0", "x;y",
]
DCS_BODIES = [
    "$qm", "$qr", "$qs", "$q q", "$q\"q", "$q\"p", "$qt", "$q", "$qzz",
    "+q544e", "+q436f", "+q524742", "+q636f6c6f7273", "+q6b6273;544e", "+qzz", "+q",
    "tmux;\x1b\x1b[1mX", "1000p", "q#0;2;0;0;0#1!10~-~", "0;1|17/ab", "=1s", "2$t", "p",
]
APC_BODIES = [
    "Ga=q,i=1,s=1,v=1,f=24;AAAA", "Ga=T,f=24,s=1,v=1;AAAA", "Ga=t,i=7,f=32,s=1,v=1;AAAAAA==",
    "Ga=p,i=7,p=1,c=2,r=1", "Ga=p,i=7,U=1,c=2,r=2", "Ga=d", "Ga=d,d=A", "Ga=d,d=i,i=7", "Ga=d,d=c",
    "Ga=T,f=100,s=1,v=1;iVBORw0KGgo=", "Ga=q;", "G", "G123", "G;<data>", "Gi=1,I=93,p=7;OK",
    "Ga=t,m=1,i=3,f=24,s=2,v=1;AAAA", "Gm=0;AAAA", "Ga=a,i=7,c=1", "Ga=c,i=7", "Ga=f,i=7,r=1;AAAA",
    "25a1;s", "25a1;s;fmt=glyf", "25a1;q;cp=e0a0", "25a1;r;cp=e0a0;AAAAAAAAAAAAAA==", "25a1;c",
    "25a1;c;cp=e0a0", "25a1;r;cp=41;AAAA", "25a1;r;cp=e0a1;reply=2;%%%not-base64%%%", "25a1;",
    "Xjunk", "", "abc",
]
CSI_FINALS = "@ABCDEFGHIJKLMPSTXZ`abcdefghijklmnpqrstuvwxyz{|}~"


class Gen:
    def __init__(self, rng):
        self.rng = rng

    def n(self):
        r = self.rng.random()
        if r < 0.5: return self.rng.randint(0, 5)
        if r < 0.8: return self.rng.randint(0, 30)
        if r < 0.94: return self.rng.randint(0, 300)
        return self.rng.choice([255, 256, 1000, 32767, 32768, 65535, 65536, 2**31 - 1, 2**32,
                                99999999999999999999])

    def params(self, k=None):
        rng = self.rng
        if k is None: k = rng.choice([0, 1, 1, 2, 2, 3, 4, 6, 20])
        out = []
        for _ in range(k):
            r = rng.random()
            if r < 0.07: out.append("")
            elif r < 0.12: out.append(":".join(str(self.n()) for _ in range(rng.randint(1, 4))))
            else: out.append(str(self.n()))
        return ";".join(out)

    def size(self, resize=False):
        rng = self.rng; r = rng.random()
        if resize and r < 0.01: return rng.choice([(0, 5), (5, 0), (0, 0)])
        if r < 0.2: return rng.randint(1, 4), rng.randint(1, 4)
        if r < 0.6: return rng.randint(5, 40), rng.randint(2, 15)
        if r < 0.75: return rng.randint(70, 90), rng.randint(20, 30)
        if r < 0.85: return rng.randint(100, 300), rng.randint(2, 12)
        if r < 0.92: return rng.randint(2, 30), rng.randint(30, 80)
        if r < 0.97: return rng.randint(150, 300), rng.randint(40, 70)
        return rng.randint(500, 1500), rng.randint(1, 4)

    def text(self):
        rng = self.rng
        k = rng.choice([1, 3, 10, 40, 120])
        return "".join(chr(rng.randint(0x20, 0x7E)) for _ in range(k))

    def unicode(self):
        rng = self.rng; out = []
        for _ in range(rng.randint(1, 24)):
            r = rng.random()
            if r < 0.3: out.append(rng.choice(WIDE))
            elif r < 0.5: out.append(rng.choice(EMOJI))
            elif r < 0.7: out.append(rng.choice("aeiouxyz ") + "".join(rng.choice(COMBINING) for _ in range(rng.randint(1, 3))))
            elif r < 0.8: out.append(rng.choice(COMBINING))
            elif r < 0.9: out.append(rng.choice(ODD))
            else: out.append(chr(rng.choice([rng.randint(0xA0, 0x2FFF), rng.randint(0x1F000, 0x1FAFF), rng.randint(0x20000, 0x2FFFF)])))
        return "".join(out)

    def control(self):
        rng = self.rng
        if rng.random() < 0.7: return rng.choice("\r\n\t\b\a\x0e\x0f\x05\x0b\x0c\r\n\n")
        return chr(rng.choice(list(range(0x20)) + [0x7F]))

    def csi(self):
        rng = self.rng; p = self.params
        pick = rng.randint(0, 33)
        if pick == 0: body = p(1) + rng.choice("ABCDEFGIJKLMPSTXZ`abde@")
        elif pick == 1: body = p(2) + rng.choice("Hf")
        elif pick == 2: body = "?" + p(rng.randint(1, 3)) + rng.choice("hl")
        elif pick == 3: body = "?" + ";".join(str(rng.choice(DEC_MODES)) for _ in range(rng.randint(1, 3))) + rng.choice("hlhl")
        elif pick == 4: body = str(rng.choice(ANSI_MODES)) + rng.choice("hl")
        elif pick == 5: body = p(2) + "r"
        elif pick == 6: body = p(2) + "s"
        elif pick == 7: body = rng.choice(["s", "u", "7", "8"])
        elif pick == 8: body = rng.choice(["5n", "6n", "?6n", "?15n", "?25n", "?26n", "?53n", "?996n", "c", ">c", "=c", ">q", ">0q", "?u", "18t", "14t", "16t", "19t", "21t", "11t", "13t"])
        elif pick == 9: body = rng.choice(["22", "23"]) + rng.choice(["", ";0", ";1", ";2", ";3"]) + "t"
        elif pick == 10: body = "8;" + p(2) + "t"
        elif pick == 11: body = p(1) + " q"
        elif pick == 12: body = p(1) + "\"q"
        elif pick == 13: body = "!p"
        elif pick == 14: body = rng.choice(["", "?"]) + str(rng.choice(DEC_MODES + ANSI_MODES + [self.n()])) + "$p"
        elif pick == 15: body = p(6) + "*y"
        elif pick == 16: body = p(1) + "#y"
        elif pick == 17: body = rng.choice([">", "<", "=", "?"]) + p(rng.randint(0, 2)) + "u"
        elif pick == 18: body = ">" + p(rng.randint(0, 2)) + rng.choice("smn")
        elif pick == 19: body = rng.choice(["#{", "#}", "#p", "#q", p(2) + "#{"])
        elif pick == 20: body = p(1) + "g"
        elif pick == 21: body = "?" + p(1) + rng.choice("JK")
        elif pick == 22: body = "?5W"
        elif pick == 23: body = p(1) + rng.choice(["'}", "'~", " @", " A"])
        elif pick == 24: body = p(4) + rng.choice(["$z", "${", "$x", "$v", "$r", "$t"])
        elif pick == 25: body = "?" + p(2) + "S"
        elif pick == 26: body = p(1) + "b"
        elif pick == 27: body = p(1) + rng.choice("IZ")
        elif pick == 28: body = p(1) + rng.choice("LM")
        elif pick == 29: body = p(1) + rng.choice("PX@")
        elif pick == 30: body = p(1) + rng.choice("ST")
        elif pick == 31: body = p(1) + rng.choice("JK")
        elif pick == 32: body = rng.choice(["?", ">", "<", "=", ""]) + p() + rng.choice(["", " ", "!", "\"", "$", "'", "#", "*", "+"]) + rng.choice(CSI_FINALS)
        else:
            body = "".join(chr(rng.randint(0x20, 0x3F)) for _ in range(rng.randint(0, 8))) + chr(rng.randint(0x40, 0x7E))
        intro = rng.choice(["\x1b[", "\x1b[", "\x1b[", "\x1b[", "\x9b", "\u009b"])
        return intro + body

    def sgr(self):
        rng = self.rng; parts = []
        for _ in range(rng.choice([0, 1, 1, 2, 3, 5, 9])):
            r = rng.randint(0, 17)
            if r == 0: parts.append(str(rng.choice([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 21, 22, 23, 24, 25, 27, 28, 29, 53, 55, 59])))
            elif r == 1: parts.append(str(rng.choice(list(range(30, 38)) + list(range(40, 48)) + list(range(90, 98)) + list(range(100, 108)) + [39, 49])))
            elif r == 2: parts.append("%d;5;%d" % (rng.choice([38, 48, 58]), self.n()))
            elif r == 3: parts.append("%d;2;%d;%d;%d" % (rng.choice([38, 48, 58]), rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)))
            elif r == 4: parts.append("%d:5:%d" % (rng.choice([38, 48, 58]), self.n()))
            elif r == 5: parts.append("%d:2::%d:%d:%d" % (rng.choice([38, 48, 58]), rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)))
            elif r == 6: parts.append("%d:2:%d:%d:%d" % (rng.choice([38, 48, 58]), rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)))
            elif r == 7: parts.append("%d:2:%d:%d:%d:%d" % (rng.choice([38, 48, 58]), self.n(), rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)))
            elif r == 8: parts.append("4:%d" % rng.choice([0, 1, 2, 3, 4, 5, 6, 9, self.n()]))
            elif r == 9: parts.append(rng.choice(["38;5", "38;2;1", "38:2", "38;6;1", "48;2", "58:", "4:", "38::5:3", "38;2;;;", "4:3:1", "38:5:1:2"]))
            elif r == 10: parts.append(str(self.n()))
            elif r == 11: parts.append("")
            else: parts.append(str(rng.randint(0, 107)))
        sep = ";" if rng.random() < 0.9 else rng.choice([":", ";;", ";:"])
        return "\x1b[" + sep.join(parts) + "m"

    def esc(self):
        rng = self.rng
        return ESC + rng.choice(["7", "8", "c", "D", "E", "H", "M", "Z", "=", ">", "#8", "#3", "#4", "#5", "#6",
                                 "(0", "(B", "(A", "(<", "(U", ")0", ")B", "*0", "+0", "-A", ".A", "/A", "n", "o",
                                 "|", "}", "~", "N", "O", "V", "W", "%G", "%@", " F", " G", "\\", "6", "9", "l", "m",
                                 chr(rng.randint(0x20, 0x7E)), "(" + chr(rng.randint(0x20, 0x7E))])

    def osc(self):
        rng = self.rng
        body = rng.choice(OSC_BODIES)
        r = rng.random()
        if r < 0.15: body = str(rng.choice([0, 2, 4, 7, 8, 9, 10, 22, 52, 104, 133, 1337, 5522, 7501, self.n()])) + ";" + self.unicode()
        elif r < 0.2: body = body + self.text()
        elif r < 0.23: body = "52;c;" + base64.b64encode(bytes(rng.randint(0, 255) for _ in range(rng.randint(0, 64)))).decode()
        return "\x1b]" + body + rng.choice(ST_CHOICES)

    def dcs(self):
        rng = self.rng
        return "\x1bP" + rng.choice(DCS_BODIES) + rng.choice(ST_CHOICES)

    def apc(self):
        rng = self.rng
        intro = rng.choice(["\x1b_", "\x1b_", "\x1b_", "\x1bX", "\x1b^"])
        return intro + rng.choice(APC_BODIES) + rng.choice(ST_CHOICES)

    def storm(self):
        rng = self.rng; k = rng.randint(5, 300); kind = rng.randint(0, 5)
        if kind == 0:
            return "".join("\x1b[38;2;%d;%d;%dm%s" % (rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255), rng.choice("ab中")) for _ in range(k))
        if kind == 1:
            return "".join(rng.choice("ae中🙂") + "".join(rng.choice(COMBINING) for _ in range(rng.randint(1, 4))) for _ in range(k))
        if kind == 2:
            return "".join("\x1b]8;id=%d;https://h.example/%d\x1b\\%s" % (i, rng.randint(0, 9999), rng.choice(["x", "yz", "中"])) for i in range(k)) + "\x1b]8;;\x1b\\"
        if kind == 3:
            return "".join("line %d %s\r\n" % (i, "w" * rng.randint(0, 40)) for i in range(k * rng.choice([1, 2, 10])))
        if kind == 4:
            return "\n" * (k * rng.choice([1, 5, 10]))
        return rng.choice(WIDE) * k

    def malformed(self):
        rng = self.rng
        return b"".join(rng.choice([rng.choice(BAD_UTF8), b"a", "中".encode()]) for _ in range(rng.randint(1, 8)))

    def token(self, depth=0):
        rng = self.rng; r = rng.random()
        if r < 0.12: t = self.text()
        elif r < 0.22: t = self.unicode()
        elif r < 0.30: t = self.control()
        elif r < 0.48: t = self.csi()
        elif r < 0.56: t = self.sgr()
        elif r < 0.62: t = self.esc()
        elif r < 0.70: t = self.osc()
        elif r < 0.73: t = self.dcs()
        elif r < 0.77: t = self.apc()
        elif r < 0.80: t = self.malformed()
        elif r < 0.82: t = bytes(rng.randint(0, 255) for _ in range(rng.randint(1, 16)))
        elif r < 0.84: t = self.storm()
        elif r < 0.86: t = "\x1b#8"
        elif depth == 0:
            # A sequence cut short and followed by whatever comes next.
            t = self.token(1)
            if isinstance(t, str): t = t.encode()
            if len(t) > 1: t = t[:rng.randint(1, len(t) - 1)]
        else: t = self.text()
        return t.encode() if isinstance(t, str) else t

    def payload(self):
        rng = self.rng
        data = b"".join(self.token() for _ in range(rng.choice([1, 1, 2, 3, 5, 8, 16])))
        if data and rng.random() < 0.05:
            b = bytearray(data)
            for _ in range(rng.randint(1, 4)): b[rng.randrange(len(b))] = rng.randint(0, 255)
            data = bytes(b)
        return data


def fuzz_case(seed):
    rng = random.Random(seed); g = Gen(rng); log = Log()
    cols, rows = g.size()
    log.new(cols, rows)
    if rng.random() < 0.85:
        log.op("F", rng.getrandbits(5) | (1 if rng.random() < 0.8 else 0), rng.choice([0, 0, 8, 64, 2048]))
    for _ in range(rng.randint(4, 80)):
        r = rng.random()
        if r < 0.72: log.write(g.payload())
        elif r < 0.80:
            c, r_ = g.size(resize=True)
            log.op("R", c, r_)
            if c and r_: cols, rows = c, r_
        elif r < 0.83: log.op("C")
        elif r < 0.86: log.op("K")
        elif r < 0.92:
            tag = rng.randint(0, 4)
            log.op("V", tag, rng.randint(0, 300) if tag == 3 else rng.randint(-60, 60))
        elif r < 0.93: log.op("X")
        elif r < 0.95:
            kind = rng.randint(0, 1)
            log.op("L", kind, rng.choice([0, 1, 10, 100, 1000, 100000]) if kind == 0 else rng.choice([0, 1, 4096, 100000, 1000000, 10000000]))
        elif r < 0.965: log.op("S", rng.randint(0, 4), rng.randint(0, cols), rng.randint(0, rows))
        elif r < 0.975: log.op("T", rng.randint(0, cols), rng.randint(0, rows))
        elif r < 0.985:
            log.op("P", rng.randint(0, 12), rng.choice([0, 1, 2, 5, 20]), rng.choice([0, 1, 3, 10, 100]),
                   rng.choice([0, 0, 1, 3]), rng.choice([0, 0, 1, 3]))
        else: log.dump()
        if rng.random() < 0.3: log.dump()
    log.dump()
    return bytes(log.out)


def main(argv):
    seed = count = None; out = root / "cases.bin"; args = iter(argv)
    for a in args:
        if a == "--seed": seed = int(next(args))
        elif a == "--count": count = int(next(args))
        else: out = pathlib.Path(a)
    if seed is None and count is None: data = corpus()
    else:
        seed = seed or 0
        data = b"".join(fuzz_case(s) for s in range(seed, seed + (count or 1)))
    out.write_bytes(data)
    print(f"{len(data)} input bytes")


if __name__ == "__main__":
    main(sys.argv[1:])
