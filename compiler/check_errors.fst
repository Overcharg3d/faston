# Stress test for error messages: cuts every .fst file at many points (giving
# all kinds of broken programs) and checks that compiler.fst reports exactly
# what the running Faston's compiler reports (or produces the same bytes).
#
#   faston compiler/check_errors.fst [cuts-per-file]

import os, sys, bytecode;
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

let root = os.path.dirname(os.path.dirname(os.path.abspath(sys.argv[0])));
let cuts = int(sys.argv[1]) if len(sys.argv) > 1 else 40;
let files = [];
files_in(root, files);
let total = 0;
let bad = 0;
for (path in files) {
    let src = "";
    with (open(path) as f) { src = f.read(); }
    let n = len(src);
    for (c in range(1, cuts + 1)) {
        # cut points spread over the file, plus a dropped character
        let at = n * c // (cuts + 1);
        let variants = [src[:at], src[:at] + src[at + 1:]];
        for (s in variants) {
            total += 1;
            let want = None;
            let got = None;
            try { want = bytecode.compile(s, "x.fst"); } except (SyntaxError as e) { want = "ERR " + str(e); }
            try {
                got = compiler.compile(s, "x.fst");
            } except (compiler.CompileError as e) {
                got = "ERR " + str(e);
            } except (Exception as e) {
                got = "CRASH " + type(e) + ": " + str(e);
            }
            if (want != got) {
                bad += 1;
                if (bad <= 15) {
                    let w = want if isinstance(want, str) else f"<{len(want)} bytes>";
                    let g = got if isinstance(got, str) else f"<{len(got)} bytes>";
                    print(f"DIFF {os.path.basename(path)} cut {at}:\n   stage0: {w}\n   ours:   {g}");
                }
            }
        }
    }
}
print(f"{total - bad} of {total} broken programs handled identically");
if (bad > 0) { exit(1); }
