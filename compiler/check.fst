# Compiles every .fst file under the given directories with the running
# Faston's compiler (bytecode.compile) and with compiler.fst, and checks that
# both produce the same bytes (or the same error message).
#
#   faston compiler/check.fst [dirs...]      (default: the repository)

import os, sys, time, bytecode;
import compiler;

def files_in(d, out) {
    for (name in sorted(os.listdir(d))) {
        let p = os.path.join(d, name);
        if (os.path.isdir(p)) {
            if (name != "target" and name != "node_modules" and not name.startswith(".")) { files_in(p, out); }
        } elif (name.endswith(".fst")) {
            out.append(p);
        }
    }
}

# first differing line of two disassembly listings
def first_diff(a, b) {
    let la = a.splitlines();
    let lb = b.splitlines();
    for (i in range(min(len(la), len(lb)))) {
        if (la[i] != lb[i]) {
            let ctx = max(0, i - 3);
            return "\n".join(["   stage0: " + x for x in la[ctx:i + 2]]) + "\n" + "\n".join(["   ours:   " + x for x in lb[ctx:i + 2]]);
        }
    }
    return f"   listings differ in length ({len(la)} vs {len(lb)} lines)";
}

let root = os.path.dirname(os.path.dirname(os.path.abspath(sys.argv[0])));
let dirs = sys.argv[1:] if len(sys.argv) > 1 else [root];
let files = [];
for (d in dirs) { files_in(d, files); }

let same = 0;
let failed = [];
let t_ref = 0.0;
let t_ours = 0.0;
for (path in files) {
    let src = "";
    try {
        with (open(path) as f) { src = f.read(); }
    } except (Exception) {
        continue;
    }
    let name = os.path.basename(path);
    let want = None;
    let want_err = None;
    let t0 = time.perf_counter();
    try { want = bytecode.compile(src, name); } except (SyntaxError as e) { want_err = str(e); }
    let t1 = time.perf_counter();
    let got = None;
    let got_err = None;
    try {
        got = compiler.compile(src, name);
    } except (compiler.CompileError as e) {
        got_err = str(e);
    } except (Exception as e) {
        got_err = "CRASH: " + type(e) + ": " + str(e);
    }
    let t2 = time.perf_counter();
    t_ref += t1 - t0;
    t_ours += t2 - t1;
    if (want == got and want_err == got_err) {
        same += 1;
        continue;
    }
    let why = "";
    if (want_err != None or got_err != None) {
        why = f"   stage0: {want_err}\n   ours:   {got_err}";
    } else {
        why = first_diff(bytecode.dis(want), bytecode.dis(got));
        if (why.startswith("   listings differ in length") and len(want) != len(got)) {
            why = why + f" (files: {len(want)} vs {len(got)} bytes)";
        } elif (bytecode.dis(want) == bytecode.dis(got)) {
            why = "   same instructions; constants or tables differ";
        }
    }
    failed.append(path + "\n" + why);
}
for (f in failed) { print("DIFF", f); }
print(f"{same} of {len(files)} files identical; stage0 {t_ref * 1000:.0f} ms, compiler.fst {t_ours * 1000:.0f} ms");
if (len(failed) > 0) { exit(1); }
