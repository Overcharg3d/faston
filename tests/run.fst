# The test suite, run with the Faston compiler (seed/):
#
#   faston tests/run.fst [--quick]
#
# Every program in tests/cases must print its .out file, and every program in
# tests/errors must fail with each line of its .err file in the output. Each
# runs interpreted, compiled to machine code, from a .fbc file, and (without
# --quick) as a built executable. Then the compiler is checked against stage 0
# and must reproduce itself.
#
# FASTON=path/to/faston picks the faston to test with (default: this one).

import os, sys, subprocess, time;

let root = os.path.dirname(os.path.dirname(os.path.abspath(sys.argv[0])));
let faston = os.environ.get("FASTON", sys.executable);
let quick = "--quick" in sys.argv;
let tmp = os.path.join(root, "build", "tests");
os.makedirs(tmp, exist_ok=True);

let env = dict(os.environ);
env["FASTON_COMPILER"] = os.path.join(root, "seed");

def programs(dir) {
    let d = os.path.join(root, "tests", dir);
    return [os.path.join(d, n) for n in sorted(os.listdir(d)) if n.endswith(".fst") and not n.startswith("_")];
}

def read(path) {
    with (open(path) as f) { return f.read().replace("\r\n", "\n"); }
}

def run(cmd, extra = None) {
    let e = dict(env);
    if (extra != None) { e.update(extra); }
    return subprocess.run(cmd, capture_output=True, env=e);
}

# How each mode runs a program: a CompletedProcess.
def interpreted(p) { return run([faston, p], {"FASTON_NATIVE": "0"}); }
def native(p) { return run([faston, p], {"FASTON_NATIVE": "1"}); }
def from_fbc(p) {
    let fbc = p[:-4] + ".test.fbc";   # next to the source, so imports resolve
    let c = run([faston, "compile", p, "-o", fbc]);
    if (c.returncode != 0) { return c; }
    let r = run([faston, fbc]);
    os.remove(fbc);
    return r;
}
def built(p) {
    let exe = os.path.join(tmp, os.path.basename(p)[:-4] + (".exe" if sys.platform == "win32" else ""));
    let b = run([faston, "build", p, "-o", exe], {"FASTON_RT": faston});
    if (b.returncode != 0) { return b; }
    return run([exe]);
}

let modes = [("interpreted", interpreted), ("native", native), (".fbc", from_fbc)];
if (not quick) { modes.append(("built", built)); }

let failures = [];
let count = 0;
let t0 = time.perf_counter();
for (m in modes) {
    let mode = m[0];
    let f = m[1];
    for (p in programs("cases")) {
        count += 1;
        let want = read(p[:-4] + ".out") if os.path.exists(p[:-4] + ".out") else "";
        let r = f(p);
        let got = r.stdout.replace("\r\n", "\n");
        if (r.returncode != 0 or got != want) {
            failures.append(f"{os.path.basename(p)} ({mode}): exit {r.returncode}\n--- expected\n{want}--- got\n{got}{r.stderr}");
        }
    }
    for (p in programs("errors")) {
        count += 1;
        let want = read(p[:-4] + ".err");
        let r = f(p);
        let text = (r.stdout + r.stderr).replace("\r\n", "\n");
        let missing = [l for l in want.split("\n") if l != "" and not (l in text)];
        if (r.returncode == 0 or len(missing) > 0) {
            failures.append(f"{os.path.basename(p)} ({mode}): exit {r.returncode}, missing {missing} in:\n{text}");
        }
    }
    print(f"  {mode}: done");
}

# the compiler: same bytes as stage 0 on every file, same errors on broken
# programs, and it reproduces itself
let checks = [
    ("compiler == stage 0", [faston, "--stage0", os.path.join(root, "compiler", "check.fst")]),
    ("errors == stage 0", [faston, "--stage0", os.path.join(root, "compiler", "check_errors.fst"), "10"]),
    ("bootstrap", [faston, os.path.join(root, "compiler", "bootstrap.fst"), "--seed", os.path.join(tmp, "seed")]),
];
for (c in checks) {
    count += 1;
    let r = run(c[1]);
    if (r.returncode != 0) {
        failures.append(f"{c[0]}: exit {r.returncode}\n{r.stdout}{r.stderr}");
    } else {
        print(f"  {c[0]}: {r.stdout.strip().splitlines()[-1]}");
    }
}

for (f in failures) { print("FAIL", f); }
print(f"{count - len(failures)} of {count} passed in {time.perf_counter() - t0:.1f} s");
if (len(failures) > 0) { exit(1); }
