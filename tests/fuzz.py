#!/usr/bin/env python3
"""Differential fuzzer: generates random programs, runs them with Faston and
CPython, and reports any difference in output.

    python3 tests/fuzz.py [iterations] [seed]

Each program is generated once as a tree and printed in both syntaxes, so the
two versions are guaranteed to mean the same thing.
"""
import os
import random
import subprocess
import sys
import tempfile

FASTON = os.path.join(os.path.dirname(__file__), "..", "target", "release", "faston")

CMP = ["<", "<=", ">", ">=", "==", "!="]
ARITH = ["+", "-", "*"]


class Gen:
    def __init__(self, rng, names):
        self.r = rng
        self.names = names

    def atom(self):
        k = self.r.random()
        if k < 0.45 and self.names:
            return self.r.choice(self.names)
        if k < 0.8:
            return str(self.r.randint(-9, 9))
        return self.r.choice(["0.5", "1.5", "-2.25", "3.0", "0.1"])

    def num(self, d):
        """Numeric expression (same text in both languages)."""
        if d <= 0 or self.r.random() < 0.3:
            return self.atom()
        k = self.r.random()
        if k < 0.55:
            return f"({self.num(d-1)} {self.r.choice(ARITH)} {self.num(d-1)})"
        if k < 0.7:
            # divisor is always >= 1 (Python % with a positive modulus is non-negative)
            op = self.r.choice(["//", "%"])
            return f"({self.num(d-1)} {op} (abs({self.num(d-1)}) % 5 + 1))"
        if k < 0.8:
            return f"(-{self.num(d-1)})"
        if k < 0.9:
            return f"({self.num(d-1)} if {self.cond(d-1)} else {self.num(d-1)})"
        return f"(min({self.num(d-1)}, {self.num(d-1)}) + max({self.num(d-1)}, 1))"

    def cond(self, d):
        if d <= 0 or self.r.random() < 0.3:
            return f"{self.num(0)} {self.r.choice(CMP)} {self.num(0)}"
        k = self.r.random()
        if k < 0.35:
            return f"({self.num(d-1)} {self.r.choice(CMP)} {self.num(d-1)})"
        if k < 0.5:
            # chained comparison
            return (f"({self.num(d-1)} {self.r.choice(CMP)} {self.num(d-1)} "
                    f"{self.r.choice(CMP)} {self.num(d-1)})")
        if k < 0.7:
            return f"({self.cond(d-1)} and {self.cond(d-1)})"
        if k < 0.85:
            return f"({self.cond(d-1)} or {self.cond(d-1)})"
        return f"(not {self.cond(d-1)})"


PRELUDE_PY = """class P:
    def __init__(self, x):
        self.x = x
        self.y = 0
    def step(self, d):
        self.x += d
        return self.x
class Q(P):
    def step(self, d):
        self.y -= d
        return self.y
"""
PRELUDE_FS = """class P {
    def init(self, x) { self.x = x; self.y = 0; }
    def step(self, d) { self.x += d; return self.x; }
}
class Q(P) {
    def step(self, d) { self.y -= d; return self.y; }
}
"""


def program(seed):
    r = random.Random(seed)
    params = ["a", "b", "c"]
    g = Gen(r, params + ["i", "acc", "len(xs)", "len(s)", "objs[i % 2].x"])
    gt = Gen(r, [])
    n = r.randint(1, 12)
    body_py, body_fs = [], []

    def stmt(indent_py, indent_fs):
        kind = r.random()
        if kind < 0.35:
            k2 = r.random()
            e = g.num(2)
            if k2 < 0.25:
                body_py.append(f"{indent_py}xs.append({e})")
                body_fs.append(f"{indent_fs}xs.append({e});")
            elif k2 < 0.45:
                lo, hi = g.num(1), g.num(1)
                body_py.append(f"{indent_py}acc += sum(xs[int({lo}) % 7:int({hi}) % 9])")
                body_fs.append(f"{indent_fs}acc += sum(xs[int({lo}) % 7:int({hi}) % 9]);")
            elif k2 < 0.6:
                body_py.append(f"{indent_py}s = (s + str(int({e}) % 10))[-6:]")
                body_fs.append(f"{indent_fs}s = (s + str(int({e}) % 10))[-6:];")
            elif k2 < 0.8:
                body_py.append(f"{indent_py}acc += objs[i % 2].step({e})")
                body_fs.append(f"{indent_fs}acc += objs[i % 2].step({e});")
            else:
                body_py.append(f"{indent_py}acc += len([v for v in xs if v > {e}])")
                body_fs.append(f"{indent_fs}acc += len([v for v in xs if v > {e}]);")
        elif kind < 0.5:
            c = g.cond(1)
            body_py.extend([f"{indent_py}j = 0", f"{indent_py}while j < 5:",
                            f"{indent_py}    j += 1", f"{indent_py}    if {c}:",
                            f"{indent_py}        break", f"{indent_py}    acc += j"])
            body_fs.append(f"{indent_fs}let j = 0; while (j < 5) {{ j += 1; if ({c}) {{ break; }} acc += j; }}")
        elif kind < 0.65:
            e = g.num(3)
            op = r.choice(["+=", "-="])
            body_py.append(f"{indent_py}acc {op} {e}")
            body_fs.append(f"{indent_fs}acc {op} {e};")
        elif kind < 0.8:
            c, e1, e2 = g.cond(2), g.num(2), g.num(2)
            body_py.extend([f"{indent_py}if {c}:", f"{indent_py}    acc += {e1}",
                        f"{indent_py}else:", f"{indent_py}    acc -= {e2}"])
            body_fs.extend([f"{indent_fs}if ({c}) {{ acc += {e1}; }} else {{ acc -= {e2}; }}"])
        else:
            c = g.cond(2)
            body_py.extend([f"{indent_py}if {c}:", f"{indent_py}    continue"])
            body_fs.extend([f"{indent_fs}if ({c}) {{ continue; }}"])
        # keep values bounded so neither side overflows / loses precision differently
        body_py.append(f"{indent_py}acc = acc % 100003 if acc == acc and abs(acc) < 1e15 else 0")
        body_fs.append(f"{indent_fs}acc = acc % 100003 if acc == acc and abs(acc) < 1e15 else 0;")

    for _ in range(r.randint(1, 4)):
        stmt("        ", "        ")

    args = [str(r.randint(-20, 20)) for _ in params]
    tops = [gt.num(3) for _ in range(3)] + [gt.cond(3) for _ in range(2)]

    setup_py = ["    acc = 0", "    xs = [3, 1, 4, 1, 5]", "    s = 'ab'", "    objs = [P(a), Q(b)]"]
    setup_fs = ["    let acc = 0;", "    let xs = [3, 1, 4, 1, 5];", "    let s = 'ab';",
                "    let objs = [P(a), Q(b)];"]
    result = "[acc, len(xs), s, objs[0].x, objs[1].y]"
    py = [PRELUDE_PY, "def f(a, b, c):"] + setup_py + [f"    for i in range({n}):"] + body_py + \
         [f"    return {result}", f"print(f({', '.join(args)}))"] + [f"print({t})" for t in tops]
    fs = [PRELUDE_FS, "def f(a, b, c) {"] + setup_fs + [f"    for (i in range({n})) {{"] + body_fs + \
         ["    }", f"    return {result};", "}", f"print(f({', '.join(args)}));"] + \
         [f"print({t});" for t in tops]
    return "\n".join(py) + "\n", "\n".join(fs) + "\n"


def run(cmd):
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=20)
    return p.returncode, p.stdout, p.stderr


def main():
    iters = int(sys.argv[1]) if len(sys.argv) > 1 else 300
    seed0 = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    bad = 0
    with tempfile.TemporaryDirectory() as d:
        for k in range(iters):
            py, fs = program(seed0 + k)
            pp, fp = os.path.join(d, "t.py"), os.path.join(d, "t.fst")
            open(pp, "w").write(py)
            open(fp, "w").write(fs)
            rp, op, ep = run([sys.executable, pp])
            rf, of, ef = run([FASTON, fp])
            if rp != 0:
                continue  # generator produced something Python rejects; skip
            if rf != 0 or op != of:
                bad += 1
                print(f"=== MISMATCH seed {seed0 + k}\n--- faston source\n{fs}--- python\n{op}"
                      f"--- faston (exit {rf})\n{of}{ef}")
                if bad >= 3:
                    break
    print(f"{iters} programs, {bad} mismatches")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
