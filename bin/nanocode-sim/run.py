#!/usr/bin/env python3
"""nanocode-sim runner: one prebuilt binary, a job manifest, any number of machines.

  run.py table [--seeds N] [--scenarios "a b"|all] [--bin SIM]     print the job manifest
  run.py run [--bin SIM] [--jobs-file F] [--job "NAME KIND N FLAGS..."]... [--shard K/N]
             [-j J] [--results PATH] [--durations PATH] [--seed-timeout S] [--recheck N]
  run.py pack OUT.tgz --bin SIM [--jobs-file F] [--durations PATH]  binary + runner + manifest

A manifest line is "NAME pass|canary N FLAGS...", the format scripts/check writes: a pass job
runs seeds 0..N and every seed must pass; a canary job (a mutation) passes once one seed in
0..N catches it, trying seed 0, then 1..8, then the rest. "table" builds it from cfo.sh's table
the way scripts/check does. "run" splits jobs into seed blocks of 4, keeps the blocks this shard
owns (--shard K/N, 1 <= K <= N: a pass block by hash of job and block, a canary by hash of its
name, so N machines given the same manifest run every seed once), joins a job's adjacent blocks
into units of about 25 ms by recorded or built-in per-seed times, and runs the units longest
first, one nanocode-sim process each, on J workers (default: the CPUs this process may use).

Each seed is timed from its SEED line (nanocode-sim --seed-lines). A seed that prints no verdict
within --seed-timeout seconds (default 60) is a liveness FAIL; a process that dies by a signal
or exits without a verdict for every seed is a crash FAIL at the first seed without one; both
print a replay command and the job's remaining seeds run in a new process. After the pass, every
failing or catching seed and --recheck N (default 16) sampled passing seeds run alone twice: the
two outputs must hash the same and give the first pass's verdict, or the seed is a
nondeterminism FAIL. --results PATH gets one JSON line per seed: {"type": "seed", "job",
"scenario", "flags", "seed", "duration_ms", "outcome": pass|fail|caught|missed|crash|timeout|
nondeterministic, "canary", "trace_hash", "shard"}, then {"type": "job", "job", "verdict",
"seeds", "caught_by"} per job and one {"type": "run", ...} summary. --durations PATH (default
durations.json beside the binary) is read for scheduling and updated after the run.
Prints FAIL/CRASH lines, PASSJOB/FAILJOB/CAUGHTJOB/MISSEDJOB per job and a SIM summary; exits 1
on any failed job, missed canary or nondeterminism.
"""
import hashlib
import heapq
import json
import os
import re
import select
import shlex
import signal
import subprocess
import sys
import tarfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BLOCK = 4
UNIT_MS = 25.0
# Per-seed milliseconds by scenario, release build on one core, used until durations are recorded.
BUILTIN_MS = {"board": 260, "executor": 90, "request_close": 95, "cell_calls": 45, "runtime": 30,
              "inbox_stress": 25, "agents": 9, "long_thread": 9, "alloc": 9, "board_sync": 9}
DEFAULT_MS = 4


def h64(text):
    return int.from_bytes(hashlib.sha1(text.encode()).digest()[:8], "big")


def scenario_of(flags):
    return flags[flags.index("--scenario") + 1] if "--scenario" in flags else "store"


def find_bin(arg):
    for b in ([arg] if arg else []) + [os.path.join(HERE, "nanocode-sim"),
                                     os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(HERE, "../../target")), "release/nanocode-sim")]:
        if b and os.access(b, os.X_OK):
            return os.path.abspath(b)
    sys.exit("run.py: no nanocode-sim binary (give --bin)")


def scenarios_of(binary):
    out = subprocess.run([binary], capture_output=True, text=True)
    return re.search(r"--scenario ([a-z_|]+)\]", out.stdout + out.stderr).group(1).split("|")


def table(argv):
    seeds, wanted, binary, cfo = 8, "all", None, os.path.join(HERE, "cfo.sh")
    while argv:
        a = argv.pop(0)
        if a == "--seeds": seeds = int(argv.pop(0))
        elif a == "--scenarios": wanted = argv.pop(0)
        elif a == "--bin": binary = argv.pop(0)
        elif a == "--cfo": cfo = argv.pop(0)
        else: sys.exit("run.py table: unknown option " + a)
    text = open(cfo).read()
    rows = text.split("\ndefault_table='\n", 1)[1].split("\n'", 1)[0].splitlines()
    names = scenarios_of(find_bin(binary)) if wanted == "all" else wanted.split()
    listed = {scenario_of(r.split()[3:]) for r in rows if len(r.split()) >= 3 and "--scenario" in r.split()}
    for s in names:
        if s not in listed:
            rows += ["8 %s pass --scenario %s" % (s, s), "2 %s_twice pass --scenario %s --twice" % (s, s)]
    out = []
    for r in rows:
        f = r.split()
        if len(f) < 3: continue
        flags = f[3:]
        if scenario_of(flags) not in names: continue
        if f[2] == "pass": out.append("%s pass %d %s" % (f[1], seeds, " ".join(flags)))
        elif f[2].startswith("canary"):
            out.append("%s canary %s %s" % (f[1], f[2].split(":")[1] if ":" in f[2] else "400", " ".join(flags)))
    print("\n".join(out))


class Job:
    def __init__(self, line):
        f = shlex.split(line)
        self.name, self.kind, self.n, self.flags = f[0], f[1], int(f[2]), f[3:]
        assert self.kind in ("pass", "canary"), line
        self.scenario = scenario_of(self.flags)
        self.key = " ".join(self.flags)
        self.done = False
        self.caught_by = None
        self.stage = 0
        self.outstanding = 0


class Durations:
    def __init__(self, path):
        self.path, self.seeds, self.jobs = path, {}, {}
        try:
            d = json.load(open(path))
            self.seeds, self.jobs = d.get("seeds", {}), d.get("jobs", {})
        except (OSError, ValueError):
            pass

    def est(self, job, seed):
        k = job.key
        return self.seeds.get(k, {}).get(str(seed), self.jobs.get(k, BUILTIN_MS.get(job.scenario, DEFAULT_MS)))

    def save(self, records):
        per = {}
        for r in records:
            if r["outcome"] in ("pass", "fail", "caught", "missed"):
                per.setdefault(r["flags"], []).append((r["seed"], r["duration_ms"]))
        for k, v in per.items():
            ms = sorted(m for _, m in v)
            med = ms[len(ms) // 2]
            self.jobs[k] = med
            slow = self.seeds.setdefault(k, {})
            for s, m in v:
                if m > max(3 * med, 20): slow[str(s)] = m
                else: slow.pop(str(s), None)
        os.makedirs(os.path.dirname(self.path) or ".", exist_ok=True)
        tmp = self.path + ".tmp"
        json.dump({"jobs": self.jobs, "seeds": self.seeds}, open(tmp, "w"), sort_keys=True)
        os.replace(tmp, self.path)


def run_proc(binary, job, seeds, timeout):
    """Runs seeds (a contiguous list) of job in one process. Returns (per-seed list, output lines,
    stop) with per-seed entries (seed, verdict, ms) and stop None, or ("timeout"|"crash", detail)
    at the first seed without a verdict."""
    cmd = [binary, "--seeds", "%d..%d" % (seeds[0], seeds[-1] + 1), "--seed-lines"] + job.flags
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
    fd, buf, lines, per = p.stdout.fileno(), b"", [], []
    last = time.monotonic()
    stop = None
    while True:
        left = last + timeout - time.monotonic()
        ready = select.select([fd], [], [], max(left, 0))[0] if left > 0 else []
        if not ready:
            os.killpg(p.pid, signal.SIGKILL)
            stop = ("timeout", "no verdict within %gs" % timeout)
            break
        chunk = os.read(fd, 1 << 16)
        if not chunk: break
        now = time.monotonic()
        buf += chunk
        *done, buf = buf.split(b"\n")
        for raw in done:
            line = raw.decode(errors="replace")
            if line.startswith("SEED seed="):
                s, verdict = line[10:].split()
                per.append((int(s), verdict, round((now - last) * 1000, 2)))
                last = now
            else:
                lines.append(line)
    if buf: lines.append(buf.decode(errors="replace"))
    rc = p.wait()
    p.stdout.close()
    if stop is None and (rc < 0 or rc not in (0, 1) or len(per) < len(seeds)):
        why = ("killed by %s" % signal.Signals(-rc).name) if rc < 0 else ("exit %d" % rc)
        if len(per) == len(seeds): why += " after every seed's verdict"
        stop = ("crash", why)
    return per, lines, stop, rc


class Runner:
    def __init__(self, binary, jobs, shard, workers, timeout, recheck, durations, results):
        self.binary, self.jobs, self.workers, self.timeout = binary, jobs, workers, timeout
        self.recheck, self.durations, self.results_path = recheck, durations, results
        self.k, self.n = shard
        self.shard = "%d/%d" % shard
        self.lock = threading.Condition()
        self.queue, self.tick, self.running = [], 0, 0
        self.records, self.fails = {}, []
        self.out = sys.stdout

    def say(self, line):
        with self.lock:
            print(line, file=self.out, flush=True)

    def replay(self, job, seed):
        return "nanocode-sim --seed %d %s" % (seed, " ".join(job.flags))

    def mine(self, key):
        return self.n == 1 or h64(key) % self.n == self.k - 1

    def push(self, job, seeds, prio):
        self.tick += 1
        heapq.heappush(self.queue, (-prio, self.tick, job, seeds))
        job.outstanding += 1

    def units(self, job, lo, hi, want=None):
        """Queues job's seeds lo..hi this shard owns, as units of about UNIT_MS. A pass unit's
        priority is its own time; a canary unit's is the canary's time from its first seed to
        the end of its budget, so canaries go first, lowest seeds first."""
        cur, cur_ms = [], 0.0
        tail = sum(self.durations.est(job, s) for s in range(lo, job.n)) if job.kind == "canary" else 0.0

        def flush():
            nonlocal tail
            self.push(job, cur, 1e6 + tail if job.kind == "canary" else cur_ms)
            tail -= cur_ms
        for b in range(lo - lo % BLOCK, hi, BLOCK):
            if want is not None and not want(b): continue
            for s in range(max(b, lo), min(b + BLOCK, hi)):
                ms = self.durations.est(job, s)
                if cur and (cur[-1] + 1 != s or cur_ms + ms > UNIT_MS):
                    flush(); cur, cur_ms = [], 0.0
                cur.append(s); cur_ms += ms
        if cur: flush()

    def stage(self, job):
        """Queues a canary's next stage: seed 0, then 1..8, then the rest of its budget."""
        bounds = [(0, 1), (1, 8), (8, job.n)]
        while job.stage < 3:
            lo, hi = bounds[job.stage]
            job.stage += 1
            hi = min(hi, job.n)
            if lo < hi:
                self.units(job, lo, hi)
                return True
        return False

    def record(self, job, seed, outcome, ms, trace=None):
        r = {"type": "seed", "job": job.name, "scenario": job.scenario, "flags": job.key, "seed": seed,
             "duration_ms": ms, "outcome": outcome, "canary": job.kind == "canary", "trace_hash": trace,
             "shard": self.shard}
        self.records[(job.name, seed)] = r

    def fail(self, job, seed, why):
        self.fails.append((job, seed))
        self.say("FAIL seed=%d job=%s %s :: replay: %s" % (seed, job.name, why, self.replay(job, seed)))

    def work(self):
        while True:
            with self.lock:
                while not self.queue and self.running:
                    self.lock.wait()
                if not self.queue:
                    self.lock.notify_all(); return
                _, _, job, seeds = heapq.heappop(self.queue)
                job.outstanding -= 1
                if job.done:
                    self.lock.notify_all(); continue
                self.running += 1
            per, lines, stop, rc = run_proc(self.binary, job, seeds, self.timeout)
            trace = hashlib.sha1("\n".join(lines).encode()).hexdigest()[:16] if len(seeds) == 1 and not stop else None
            with self.lock:
                self.running -= 1
                msgs = {}
                for l in lines:
                    m = re.match(r"(FAIL|CAUGHT) seed=(\d+) (.*)", l)
                    if m: msgs[int(m.group(2))] = m.group(3).split(" :: replay:")[0]
                for s, verdict, ms in per:
                    if job.kind == "pass":
                        self.record(job, s, verdict, ms, trace)
                        if verdict == "fail": self.fail(job, s, msgs.get(s, "wrong"))
                    else:
                        self.record(job, s, "caught" if verdict == "fail" else "missed", ms, trace)
                        # Caught as scripts/check counts it: a failing seed, exit 0 and the
                        # sim's own "MUTATION ... caught by" line.
                        if verdict == "fail" and not stop and rc == 0 and any(re.match(r"MUTATION .* caught by", l) for l in lines) \
                                and (job.caught_by is None or s < job.caught_by):
                            job.caught_by = s
                if job.kind == "pass" and not stop and rc != 0 and all(v == "pass" for _, v, _ in per):
                    tail = [l for l in lines if l.startswith(("RESULT", "FAIL", "INVARIANT")) or "panicked" in l][:3] or lines[-3:]
                    self.fail(job, seeds[0], "exit %d with every seed passing (seeds %d..%d) | %s" % (rc, seeds[0], seeds[-1] + 1, " | ".join(tail)))
                if stop:
                    s = seeds[len(per)] if len(per) < len(seeds) else seeds[-1]
                    tail = [l for l in lines if "panicked" in l or "error" in l.lower()][:3] or lines[-3:]
                    self.record(job, s, stop[0], round(self.timeout * 1000 if stop[0] == "timeout" else 0, 2))
                    what = "liveness: " if stop[0] == "timeout" else "crash: "
                    self.fail(job, s, what + stop[1] + (" | " + " | ".join(tail) if tail else ""))
                    rest = seeds[len(per) + 1:]
                    if rest: self.push(job, rest, len(rest) * self.durations.est(job, rest[0]))
                if job.kind == "canary" and job.caught_by is not None:
                    job.done = True
                elif job.kind == "canary" and job.outstanding == 0 and not self.stage(job):
                    job.done = True
                self.lock.notify_all()

    def pool(self):
        threads = [threading.Thread(target=self.work, daemon=True) for _ in range(self.workers)]
        for t in threads: t.start()
        for t in threads: t.join()

    def run(self):
        t0 = time.monotonic()
        mine = []
        for job in self.jobs:
            if job.kind == "pass":
                # A pass block belongs to one shard by hash of "job:first seed".
                owned = lambda b, j=job: self.mine("%s:%d" % (j.name, b))
                if any(owned(b) for b in range(0, job.n, BLOCK)):
                    mine.append(job)
                    self.units(job, 0, job.n, want=owned)
            elif self.mine(job.name):
                mine.append(job)
                self.stage(job)
        self.pool()
        main_s = time.monotonic() - t0
        nondet = self.recheck_pass()
        wall = time.monotonic() - t0
        return self.report(mine, main_s, wall, nondet)

    def recheck_pass(self):
        """Reruns failing/catching seeds and a sample of passing ones alone, twice each."""
        bad = [r for r in self.records.values() if r["outcome"] in ("fail", "caught")][:32]
        good = sorted((r for r in self.records.values() if r["outcome"] == "pass"),
                      key=lambda r: h64("%s:%d" % (r["job"], r["seed"])))[:self.recheck]
        todo = bad + good
        if not todo: return 0
        byname = {j.name: j for j in self.jobs}
        got = {}
        sem = threading.Semaphore(self.workers)

        def one(r, i):
            with sem:
                job = byname[r["job"]]
                per, lines, stop, _ = run_proc(self.binary, job, [r["seed"]], self.timeout)
                got[(r["job"], r["seed"], i)] = (per[0][1] if per and not stop else (stop or ("?",))[0],
                                                 hashlib.sha1("\n".join(lines).encode()).hexdigest()[:16])
        ts = [threading.Thread(target=one, args=(r, i)) for r in todo for i in (0, 1)]
        for t in ts: t.start()
        for t in ts: t.join()
        nondet = 0
        for r in todo:
            job = byname[r["job"]]
            (v0, h0), (v1, h1) = got[(r["job"], r["seed"], 0)], got[(r["job"], r["seed"], 1)]
            first = "fail" if r["outcome"] in ("fail", "caught") else "pass"
            mismatch = []
            # A passing seed's whole output must repeat byte for byte; a failing or catching
            # seed must repeat its verdict (some catches print wall-clock counts).
            if first == "pass" and h0 != h1: mismatch.append("trace hash %s then %s" % (h0, h1))
            if first == "pass" and r["trace_hash"] and r["trace_hash"] not in (h0, h1):
                mismatch.append("first pass hashed %s" % r["trace_hash"])
            if v0 != first or v1 != first: mismatch.append("verdict %s, then alone %s and %s" % (first, v0, v1))
            if mismatch and job.kind == "canary":
                # Some canaries catch on wall-clock budgets (the executor's real threads), so a
                # catch that doesn't replay is reported but doesn't fail the run.
                r["recheck"] = "flaky_catch"
                self.say("FLAKY-CATCH seed=%d job=%s %s :: replay: %s" % (r["seed"], job.name, "; ".join(mismatch), self.replay(job, r["seed"])))
            elif mismatch:
                nondet += 1
                r["outcome"] = "nondeterministic"
                self.fails.append((job, r["seed"]))
                self.say("FAIL seed=%d job=%s nondeterminism: %s :: replay: %s" % (r["seed"], job.name, "; ".join(mismatch), self.replay(job, r["seed"])))
            r["trace_hash"] = h0
        self.say("RECHECK seeds=%d nondeterministic=%d" % (len(todo), nondet))
        return nondet

    def report(self, mine, main_s, wall, nondet):
        recs = sorted(self.records.values(), key=lambda r: (r["job"], r["seed"]))
        counts = dict(passed=0, failed=0, caught=0, missed=0)
        failed_jobs = set(j.name for j, _ in self.fails)
        jobrecs = []
        for job in mine:
            if job.kind == "pass":
                v = "fail" if job.name in failed_jobs else "pass"
                self.say(("FAILJOB %s :: nanocode-sim --seeds 0..%d %s" if v == "fail" else "PASSJOB %s") % ((job.name, job.n, job.key) if v == "fail" else (job.name,)))
                counts["failed" if v == "fail" else "passed"] += 1
            elif job.caught_by is not None and job.name not in failed_jobs:
                v = "caught"
                self.say("CAUGHTJOB %s seed=%d" % (job.name, job.caught_by)); counts["caught"] += 1
            elif job.caught_by is not None or job.name in failed_jobs:
                v = "fail"
                self.say("FAILJOB %s :: nanocode-sim --seeds 0..%d %s" % (job.name, job.n, job.key))
                counts["failed"] += 1
            else:
                v = "missed"
                self.say("MISSEDJOB %s :: nanocode-sim --seeds 0..%d %s" % (job.name, job.n, job.key))
                counts["missed"] += 1
            jobrecs.append({"type": "job", "job": job.name, "kind": job.kind, "flags": job.key, "verdict": v,
                            "seeds": job.n, "caught_by": job.caught_by, "shard": self.shard})
        nseeds = len(recs)
        cpu_ms = sum(r["duration_ms"] for r in recs)
        crashes = sum(r["outcome"] == "crash" for r in recs)
        timeouts = sum(r["outcome"] == "timeout" for r in recs)
        ok = counts["failed"] == 0 and counts["missed"] == 0 and nondet == 0 and counts["passed"] + counts["caught"] > 0
        summary = {"type": "run", "shard": self.shard, "jobs": len(mine), "seeds": nseeds, "wall_s": round(wall, 3),
                   "main_s": round(main_s, 3), "seed_ms_total": round(cpu_ms, 1), "workers": self.workers,
                   "seeds_per_hour": int(nseeds / main_s * 3600) if main_s > 0 else 0, "crashes": crashes,
                   "timeouts": timeouts, "nondeterministic": nondet, "ok": ok, **counts}
        self.say("SIM pass_jobs=%d failed_jobs=%d canaries_caught=%d missed=%d seeds=%d crashes=%d timeouts=%d nondeterministic=%d wall=%.2fs seeds_per_hour=%d shard=%s"
                 % (counts["passed"], counts["failed"], counts["caught"], counts["missed"], nseeds, crashes, timeouts, nondet, wall, summary["seeds_per_hour"], self.shard))
        if self.results_path:
            os.makedirs(os.path.dirname(os.path.abspath(self.results_path)), exist_ok=True)
            with open(self.results_path, "w") as f:
                for r in recs + jobrecs + [summary]: f.write(json.dumps(r) + "\n")
        try:
            self.durations.save(recs)
        except OSError:
            pass
        return 0 if ok else 1


def cpus():
    try: return len(os.sched_getaffinity(0))
    except AttributeError: return os.cpu_count() or 1


def parse_shard(s):
    m = re.fullmatch(r"(?:hash:)?(\d+)/(\d+)", s)
    if not m or not 1 <= int(m.group(1)) <= int(m.group(2)): sys.exit("run.py: --shard wants K/N with 1 <= K <= N")
    return int(m.group(1)), int(m.group(2))


def run(argv):
    binary, files, lines, shard, workers, results, durations = None, [], [], (1, 1), cpus(), None, None
    timeout, recheck = 60.0, 16
    while argv:
        a = argv.pop(0)
        if a == "--bin": binary = argv.pop(0)
        elif a == "--jobs-file": files.append(argv.pop(0))
        elif a == "--job": lines.append(argv.pop(0))
        elif a in ("--shard", "--partition"): shard = parse_shard(argv.pop(0))
        elif a in ("-j", "--jobs"): workers = int(argv.pop(0))
        elif a == "--results": results = argv.pop(0)
        elif a == "--durations": durations = argv.pop(0)
        elif a == "--seed-timeout": timeout = float(argv.pop(0))
        elif a == "--recheck": recheck = int(argv.pop(0))
        else: sys.exit("run.py run: unknown option " + a)
    binary = find_bin(binary)
    if not files and not lines and os.path.exists(os.path.join(HERE, "jobs.txt")): files = [os.path.join(HERE, "jobs.txt")]
    for f in files:
        lines += [l for l in (sys.stdin if f == "-" else open(f)).read().splitlines() if l.strip() and not l.startswith("#")]
    if not lines: sys.exit("run.py run: no jobs (give --jobs-file or --job)")
    jobs = [Job(l) for l in lines]
    dur = Durations(durations or os.path.join(os.path.dirname(binary), "durations.json"))
    return Runner(binary, jobs, shard, workers, timeout, recheck, dur, results).run()


def pack(argv):
    out, binary, files, durations = argv.pop(0), None, [], None
    while argv:
        a = argv.pop(0)
        if a == "--bin": binary = argv.pop(0)
        elif a == "--jobs-file": files.append(argv.pop(0))
        elif a == "--durations": durations = argv.pop(0)
        else: sys.exit("run.py pack: unknown option " + a)
    binary = find_bin(binary)
    with tarfile.open(out, "w:gz") as t:
        t.add(binary, "nanocode-sim")
        t.add(os.path.abspath(__file__), "run.py")
        for f in files: t.add(f, "jobs.txt")
        dpath = durations or os.path.join(os.path.dirname(binary), "durations.json")
        if os.path.exists(dpath): t.add(dpath, "durations.json")
    print("PACK %s sha256=%s" % (out, hashlib.sha256(open(binary, "rb").read()).hexdigest()[:16]))


def main():
    argv = sys.argv[1:]
    cmd = argv.pop(0) if argv and argv[0] in ("table", "run", "pack") else "run"
    if argv[:1] in (["-h"], ["--help"]): print(__doc__); return 0
    return {"table": table, "run": run, "pack": pack}[cmd](argv) or 0


if __name__ == "__main__":
    sys.exit(main())
