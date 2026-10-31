"""Random differential testing against zz's Zig libghostty-vt.

    fuzz.py ZIG_PREFIX [--time SECS] [--seeds A:B] [--jobs N] [--out DIR] [--checked]
    fuzz.py ZIG_PREFIX --replay SEED|CASE [--chunk N]
    fuzz.py ZIG_PREFIX --minimize SEED|CASE [--chunk N] [--timeout SECS]
    fuzz.py --show CASE

The default mode builds both runners once (run.sh with BUILD_ONLY=1), then runs
random cases from generate.py --seed in parallel, each at write-chunk size 0
and two more picked by the seed. A case fails when the two snapshot streams
differ or either runner crashes or hangs. Failures go to OUT/failures/, a few
per signature (which snapshot field differed first, or how a runner died).

--checked builds the Rust side with debug assertions (run.sh RUST_PROFILE=dev),
so page integrity checks and overflow checks abort the Rust runner. Pair it
with a Debug build of the Zig library (zig build -Demit-lib-vt=true
-Doptimize=Debug --prefix DIR) so a crash on both sides shows up as
crash-both: a bug the port shares with upstream, not a port difference.

--replay runs one case and prints the first differing snapshot and the first
differing lines of a value trace (compare.c's DIFF_TRACE). --minimize drops
ops and write bytes while the case still fails the same way, then replays the
result. --show prints a log as Python literals.

known/ keeps minimized cases of bugs both engines share, so
both engines agree on them: shared-viewport-pin-integrity.bin trips the
page list integrity check in debug builds of either engine (--checked), and
clone-insert-lines-livelock.bin never finishes on either (it writes to a
screen clone, which is zz's addition, so the bug is in zz's clone code).
"""
import concurrent.futures as cf, os, pathlib, random, re, shutil, signal, subprocess, sys, tempfile, time

here = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(here))
import generate  # noqa: E402

CHUNKS = [1, 2, 3, 4, 5, 7, 8, 13, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 4096]
TIMEOUT = 30  # seconds per runner; --checked uses 300
runners = {}


def build(zig, out, checked):
    env = dict(os.environ, OUT=str(out), BUILD_ONLY="1", RUST_PROFILE="dev" if checked else "release")
    subprocess.run([str(here / "run.sh"), zig], env=env, check=True)
    return {"zig": str(out / "zig-runner"), "rust": str(out / "rust-runner")}


def chunks_for(seed):
    rng = random.Random(seed * 7919 + 1)
    return [0] + rng.sample(CHUNKS, 2)


def run(side, path, chunk, trace=None):
    env = dict(os.environ)
    if trace: env["DIFF_TRACE"] = trace
    try:
        p = subprocess.run([runners[side], path, str(chunk)], capture_output=True, timeout=TIMEOUT, env=env)
        return p.returncode, p.stdout.decode(errors="replace"), p.stderr.decode(errors="replace")
    except subprocess.TimeoutExpired as e:
        return "hang", (e.stdout or b"").decode(errors="replace"), ""


def fields(line):
    t = line.split()
    if len(t) > 1 and t[0].isdigit() and t[1] == "state":
        return dict(zip(t[1::2], t[2::2]))
    return {"line": line}


def first_diff(a, b):
    """(index, field, zig line, rust line) of the first differing output line."""
    al, bl = a.splitlines(), b.splitlines()
    for i, (x, y) in enumerate(zip(al, bl)):
        if x != y:
            fx, fy = fields(x), fields(y)
            names = [k for k in list(fx) + [k for k in fy if k not in fx] if fx.get(k) != fy.get(k)]
            return i, "+".join(names), x, y
    if len(al) != len(bl):
        i = min(len(al), len(bl))
        return i, "length", (al[i] if i < len(al) else "<end>"), (bl[i] if i < len(bl) else "<end>")
    return None


def crash_key(err):
    """A short stable description of why a runner died."""
    lines = [l.strip() for l in err.splitlines() if l.strip()]
    for i, line in enumerate(lines):
        if " panicked at " in line:
            # Rust: "thread '..' (id) panicked at FILE:LINE:COL:" then the message.
            where = line.split(" panicked at ", 1)[1].rstrip(":")
            return "%s %s" % (where, lines[i + 1][:120] if i + 1 < len(lines) else "")
        if line.startswith("panic:") or "Assertion" in line or " result " in line:
            return line[:160]
    return lines[0][:160] if lines else "no message"


def outcome(path, chunk):
    """None when both runners agree, else (signature, detail)."""
    zr, zo, ze = run("zig", path, chunk)
    rr, ro, re_ = run("rust", path, chunk)
    if zr == "hang" or rr == "hang":
        side = "both" if zr == rr else ("zig" if zr == "hang" else "rust")
        return "hang-" + side, {"chunk": chunk}
    if zr != 0 or rr != 0:
        side = "both" if zr != 0 and rr != 0 else ("zig" if zr != 0 else "rust")
        err = ze if side == "zig" else re_
        return "crash-%s %s" % (side, crash_key(err)), {"chunk": chunk, "zig_rc": zr, "rust_rc": rr,
                                                       "zig_err": ze[:1500], "rust_err": re_[:1500]}
    d = first_diff(zo, ro)
    if d:
        return "diff " + d[1], {"chunk": chunk, "line": d[0], "zig": d[2], "rust": d[3]}
    return None


def check_seed(seed, workdir):
    path = os.path.join(workdir, "case-%d.bin" % os.getpid())
    data = generate.fuzz_case(seed)
    with open(path, "wb") as f: f.write(data)
    for chunk in chunks_for(seed):
        o = outcome(path, chunk)
        if o: return seed, o[0], o[1], data
    return seed, None, None, None


def check_batch(seeds, workdir, zig_runner, rust_runner, timeout):
    global TIMEOUT
    TIMEOUT = timeout
    runners.update(zig=zig_runner, rust=rust_runner)
    return [check_seed(s, workdir) for s in seeds]


def save(out, sig, seed, detail, data, keep=3):
    slug = "".join(c if c.isalnum() else "-" for c in sig)[:80].strip("-")
    d = out / "failures" / slug
    d.mkdir(parents=True, exist_ok=True)
    if len(list(d.glob("*.bin"))) >= keep: return
    (d / ("seed-%d.bin" % seed)).write_bytes(data)
    text = "signature: %s\nseed: %d\n" % (sig, seed) + "".join("%s: %s\n" % kv for kv in detail.items())
    (d / ("seed-%d.txt" % seed)).write_text(text)


def campaign(out, start, stop, seconds, jobs):
    work = tempfile.mkdtemp(prefix="fuzz-", dir=out)
    # Run private copies: rebuilding a runner in place while the campaign
    # uses it gets the running copy killed on macOS.
    for side in ("zig", "rust"):
        shutil.copy2(runners[side], os.path.join(work, side + "-runner"))
        runners[side] = os.path.join(work, side + "-runner")
    seen, total, t0, last = {}, 0, time.time(), time.time()
    batch = 16
    nxt = start
    log = open(out / "campaign.log", "a")
    def say(msg):
        print(msg, flush=True); log.write(msg + "\n"); log.flush()
    say("fuzz: seeds from %d, %s, %d jobs" % (start, "until %d" % stop if stop else "%ds" % seconds, jobs))
    def stop_now(*_): raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, stop_now)
    pool = cf.ProcessPoolExecutor(jobs)
    try:
        pending = set()
        def more():
            nonlocal nxt
            while len(pending) < jobs * 3 and (stop is None or nxt < stop) and (seconds is None or time.time() - t0 < seconds):
                hi = nxt + batch if stop is None else min(nxt + batch, stop)
                pending.add(pool.submit(check_batch, list(range(nxt, hi)), work, runners["zig"], runners["rust"], TIMEOUT))
                nxt = hi
        more()
        while pending:
            done, _ = cf.wait(pending, return_when=cf.FIRST_COMPLETED)
            for fut in done:
                pending.discard(fut)
                for seed, sig, detail, data in fut.result():
                    total += 1
                    if sig:
                        new = sig not in seen
                        seen.setdefault(sig, []).append(seed)
                        save(out, sig, seed, detail, data)
                        if new: say("NEW %s (seed %d, chunk %s)" % (sig, seed, detail.get("chunk")))
            more()
            if time.time() - last > 60:
                last = time.time()
                say("%d seeds in %.0fs (%.1f/s), %d failing seeds, %d signatures" % (
                    total, last - t0, total / (last - t0), sum(map(len, seen.values())), len(seen)))
        pool.shutdown()
    except KeyboardInterrupt:
        # Workers outlive a killed parent otherwise, runners included.
        for p in list(getattr(pool, "_processes", {}).values()): p.kill()
        say("interrupted")
    shutil.rmtree(work, ignore_errors=True)
    say("done: %d seeds (%d..%d) in %.0fs, %d failing seeds" % (total, start, nxt - 1, time.time() - t0, sum(map(len, seen.values()))))
    with open(out / "failing-seeds.txt", "a") as f:
        for sig, seeds in sorted(seen.items(), key=lambda kv: -len(kv[1])):
            say("  %6d  %s  e.g. seeds %s" % (len(seeds), sig, " ".join(map(str, seeds[:5]))))
            f.write("%s\t%s\n" % (sig, " ".join(map(str, sorted(seeds)))))
    return 1 if seen else 0


def load(arg):
    p = pathlib.Path(arg)
    if p.exists(): return p.read_bytes(), None
    return generate.fuzz_case(int(arg)), int(arg)


def show(data):
    for tag, args, payload in generate.parse(data):
        print("%s %s" % (tag, repr(payload) if tag == "W" else " ".join(str(a if a < 2**31 else a - 2**32) for a in args)))


def failing_chunk(path, seed, chunk):
    if chunk is not None: return chunk, outcome(path, chunk)
    for c in (chunks_for(seed) if seed is not None else [0]) + CHUNKS:
        o = outcome(path, c)
        if o: return c, o
    return None, None


def replay(data, seed, chunk, out):
    path = str(out / "replay.bin"); pathlib.Path(path).write_bytes(data)
    chunk, o = failing_chunk(path, seed, chunk)
    if not o:
        print("no difference at any chunk size"); return 0
    sig, detail = o
    print("signature: %s\nchunk: %d" % (sig, chunk))
    for k, v in detail.items():
        if k != "chunk": print("%s: %s" % (k, v))
    tz, tr = str(out / "trace-zig.txt"), str(out / "trace-rust.txt")
    run("zig", path, chunk, tz); run("rust", path, chunk, tr)
    a = pathlib.Path(tz).read_text(errors="replace").splitlines()
    b = pathlib.Path(tr).read_text(errors="replace").splitlines()
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y: break
    else:
        i = min(len(a), len(b))
    print("first differing trace line %d (traces in %s and %s):" % (i, tz, tr))
    for line in a[max(0, i - 6):i]: print("   " + line[:300])
    for line in a[i:i + 3]: print("zig  " + line[:300])
    for line in b[i:i + 3]: print("rust " + line[:300])
    return 1


def ddmin(items, test, max_parts=None):
    """Delta debugging: drop parts of items while test still passes. With
    max_parts the parts never get smaller than len(items) / max_parts, which
    keeps long writes from costing one run per byte."""
    n = 2
    while len(items) >= 2:
        size = -(-len(items) // n)
        for i in range(0, len(items), size):
            rest = items[:i] + items[i + size:]
            if test(rest):
                items = rest; n = max(n - 1, 2); break
        else:
            if n >= len(items) or (max_parts and n >= max_parts): break
            n = min(n * 2, len(items), max_parts or len(items))
    return items


TOKEN = re.compile(
    rb"\x1b\[[0-?]*[ -/]*[@-~]"                   # CSI
    rb"|\x1b[\]P_X^][^\x07\x1b\x9c]*(?:\x07|\x1b\\|\x9c)?"  # OSC, DCS, APC, SOS, PM
    rb"|\x1b[ -/]*[0-~]"                          # other escapes
    rb"|[\xc2-\xdf][\x80-\xbf]|[\xe0-\xef][\x80-\xbf]{2}|[\xf0-\xf4][\x80-\xbf]{3}"  # UTF-8
    rb"|[\x00-\xff]", re.S)


def tokens(payload):
    """Split a write at escape sequence and character boundaries."""
    return TOKEN.findall(payload)


def minimize(data, seed, chunk, out):
    path = str(out / "minimize.bin")
    pathlib.Path(path).write_bytes(data)
    chunk, o = failing_chunk(path, seed, chunk)
    if not o:
        print("case does not fail"); return 0
    sig = o[0]
    print("minimizing %s at chunk %d, %d bytes" % (sig, chunk, len(data)), flush=True)
    runs = [0]
    def fails(ops):
        runs[0] += 1
        pathlib.Path(path).write_bytes(generate.serialize(ops))
        r = outcome(path, chunk)
        return r is not None and r[0] == sig
    ops = generate.parse(data)
    if sig.startswith("diff"):
        # Snapshots after the first differing one cannot matter.
        line, dumps = int(o[1]["line"]), 0
        for i, (tag, _, _) in enumerate(ops):
            if tag == "D":
                if dumps == line and fails(ops[:i + 1]): ops = ops[:i + 1]; break
                dumps += 1
    while True:
        before = generate.serialize(ops)
        head, rest = ops[:1], ops[1:]
        ops = head + ddmin(rest, lambda r: fails(head + r))
        for i in range(len(ops)):
            tag, args, payload = ops[i]
            if tag == "W" and len(payload) > 1:
                def with_payload(p, i=i):
                    p = b"".join(p)
                    return ops[:i] + [("W", [len(p)], p)] + ops[i + 1:]
                # Whole sequences and characters first, then bytes.
                p = ddmin(tokens(payload), lambda p: fails(with_payload(p)))
                p = b"".join(p)
                p = ddmin([p[j:j + 1] for j in range(len(p))], lambda p: fails(with_payload(p)), None if len(p) <= 512 else 64)
                ops = with_payload(p)
            if tag in "NR":
                for small in ([1, 1], [2, 2], [5, 3], [10, 4], [20, 5], [args[0], 3], [3, args[1]], [80, 24]):
                    if small[0] <= args[0] and small[1] <= args[1] and small != args and fails(ops[:i] + [(tag, small, b"")] + ops[i + 1:]):
                        ops = ops[:i] + [(tag, small, b"")] + ops[i + 1:]; break
        # Merge neighbouring writes when the failure survives it.
        i = 1
        while i < len(ops):
            if ops[i][0] == "W" and ops[i - 1][0] == "W":
                p = ops[i - 1][2] + ops[i][2]
                cand = ops[:i - 1] + [("W", [len(p)], p)] + ops[i + 1:]
                if fails(cand): ops = cand; continue
            i += 1
        if generate.serialize(ops) == before: break
    data = generate.serialize(ops)
    print("minimized to %d ops, %d bytes after %d runs:" % (len(ops), len(data), runs[0]))
    show(data)
    dest = out / "minimized.bin"; dest.write_bytes(data)
    print("saved %s" % dest)
    replay(data, None, chunk, out)
    return 1


def main(argv):
    opts = {"--time": None, "--seeds": None, "--jobs": str(max(1, (os.cpu_count() or 2) // 2)),
            "--out": None, "--replay": None, "--minimize": None, "--chunk": None, "--show": None,
            "--timeout": None}
    pos, it, checked = [], iter(argv), False
    for a in it:
        if a == "--checked": checked = True
        elif a in opts: opts[a] = next(it)
        elif a in ("-h", "--help"): print(__doc__); return 0
        else: pos.append(a)
    if opts["--show"]:
        show(pathlib.Path(opts["--show"]).read_bytes()); return 0
    if len(pos) != 1:
        print(__doc__); return 2
    out = pathlib.Path(opts["--out"] or here.parent.parent / "target" / ("fuzz-checked" if checked else "fuzz")).resolve()
    out.mkdir(parents=True, exist_ok=True)
    runners.update(build(pos[0], out, checked))
    global TIMEOUT
    if checked: TIMEOUT = 300
    if opts["--timeout"]: TIMEOUT = float(opts["--timeout"])
    chunk = int(opts["--chunk"]) if opts["--chunk"] else None
    if opts["--replay"]:
        data, seed = load(opts["--replay"]); return replay(data, seed, chunk, out)
    if opts["--minimize"]:
        data, seed = load(opts["--minimize"]); return minimize(data, seed, chunk, out)
    start, stop, seconds = 0, None, None
    if opts["--seeds"]:
        a, b = opts["--seeds"].split(":"); start, stop = int(a), int(b)
    if opts["--time"]: seconds = float(opts["--time"])
    if stop is None and seconds is None: seconds = 600
    return campaign(out, start, stop, seconds, int(opts["--jobs"]))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
