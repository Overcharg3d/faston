# Bootstraps the Faston compiler:
#
#   stage 1 = compiler/*.fst compiled by stage 0 (the Rust compiler)
#   stage 2 = compiler/*.fst compiled by stage 1
#   stage 3 = compiler/*.fst compiled by stage 2   (must equal stage 2)
#
# and installs stage 2 as the seed (the compiler `faston` uses).
#
#   faston compiler/bootstrap.fst [--from-seed] [--seed DIR]
#
# --from-seed starts from the current seed instead of stage 0, which is how the
# compiler is rebuilt after changing it (no Rust involved).

import os, sys, subprocess, shutil, time;

const PARTS = ["lexer", "ops", "compiler"];

let here = os.path.dirname(os.path.abspath(sys.argv[0]));
let root = os.path.dirname(here);
let seed = os.path.join(root, "seed");
let from_seed = False;
let i = 1;
while (i < len(sys.argv)) {
    let a = sys.argv[i];
    if (a == "--from-seed") {
        from_seed = True;
    } elif (a == "--seed") {
        i += 1;
        seed = sys.argv[i];
    } else {
        print(f"unknown option {a}");
        exit(2);
    }
    i += 1;
}
let out = os.path.join(root, "build", "bootstrap");

# compiles every part of the compiler into `dest` with the given compiler
# (a folder with compiler.fbc, or None for stage 0)
def compile_with(compiler, dest) {
    os.makedirs(dest, exist_ok=True);
    let env = dict(os.environ);
    let cmd = [sys.executable];
    if (compiler == None) {
        cmd.append("--stage0");
    } else {
        env["FASTON_COMPILER"] = compiler;
    }
    for (p in PARTS) {
        let r = subprocess.run(cmd + ["compile", os.path.join(here, p + ".fst"), "-o", os.path.join(dest, p + ".fbc")],
                               capture_output=True, env=env);
        if (r.returncode != 0) {
            print(r.stdout + r.stderr);
            print(f"compiling {p}.fst failed");
            exit(1);
        }
    }
}

def read(path) {
    with (open(path, "rb") as f) { return f.read(); }
}

def same(a, b) {
    for (p in PARTS) {
        if (read(os.path.join(a, p + ".fbc")) != read(os.path.join(b, p + ".fbc"))) { return False; }
    }
    return True;
}

let stages = [None if not from_seed else seed];
let names = ["stage 0" if not from_seed else "seed"];
for (n in [1, 2, 3]) {
    let dest = os.path.join(out, f"stage{n}");
    let t0 = time.perf_counter();
    compile_with(stages[-1], dest);
    print(f"stage {n}: compiled by {names[-1]} in {(time.perf_counter() - t0) * 1000:.0f} ms");
    stages.append(dest);
    names.append(f"stage {n}");
}
if (not same(stages[2], stages[3])) {
    print("stage 2 and stage 3 differ: the compiler doesn't reproduce itself");
    exit(1);
}
print("stage 3 == stage 2: the compiler reproduces itself" + (" (and matches stage 1)" if same(stages[1], stages[2]) else ""));
os.makedirs(seed, exist_ok=True);
for (p in PARTS) { shutil.copyfile(os.path.join(stages[2], p + ".fbc"), os.path.join(seed, p + ".fbc")); }
print(f"installed stage 2 in {seed}");
