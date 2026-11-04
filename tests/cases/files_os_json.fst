# Files, os / os.path / shutil, sys and json (expected output generated with CPython).
import os, sys, json, shutil;

let base = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "_tmp_files_test_" + str(os.getpid()));
if (os.path.exists(base)) { shutil.rmtree(base); }
os.makedirs(os.path.join(base, "sub", "deeper"));
os.makedirs(os.path.join(base, "sub"), exist_ok=True);

# write, append, read back
let p = os.path.join(base, "notes.txt");
let f = open(p, "w");
print(f.write("first line\n"), f.name.endswith("notes.txt"), f.mode, f.closed);
f.writelines(["second\n", "third\n"]);
f.close();
print(f.closed);
let g = open(p, "a");
g.write("fourth\n");
g.close();

let r = open(p);
print(repr(r.readline()), repr(r.read(3)), repr(r.readline()));
print(r.readlines());
print(repr(r.read()), repr(r.readline()));
r.close();
try {
    r.read();
} except (ValueError as e) {
    print("ValueError:", e);
}

# iterate over lines, seek/tell
let total = 0;
let h = open(p);
for (line in h) {
    total += len(line);
}
print("chars", total, h.tell());
h.seek(0);
print(repr(h.read(5)), h.tell());
h.close();

# print(file=...)
let out = open(os.path.join(base, "printed.txt"), "w");
print("a", 1, [2, 3], sep="|", end="!\n", file=out);
print("second", file=out);
out.close();
print(open(os.path.join(base, "printed.txt")).read(), end="");

# "\r\n" is read back as "\n"
let crlf = open(os.path.join(base, "crlf.txt"), "w");
crlf.write("x\r\ny\r\n");
crlf.close();
print(open(os.path.join(base, "crlf.txt")).readlines());

# errors
try {
    open(os.path.join(base, "missing.txt"));
} except (FileNotFoundError as e) {
    print("FileNotFoundError caught:", isinstance(e, OSError));
}
try {
    os.mkdir(base);
} except (FileExistsError) {
    print("FileExistsError caught");
}

# os / os.path
print(os.listdir(base));
print(os.path.isfile(p), os.path.isdir(p), os.path.isdir(base), os.path.exists(os.path.join(base, "nope")));
print(os.path.basename("a/b/c.txt"), os.path.dirname("a/b/c.txt"), os.path.splitext("a/b/c.tar.gz"), os.path.splitext(".bashrc"));
print(os.path.split("a/b/c.txt"), os.path.basename("a/b/"), os.path.splitext("noext"));
print(os.path.getsize(p), os.path.isabs(base), os.path.isabs("rel/x"));
shutil.copyfile(p, os.path.join(base, "sub", "copy.txt"));
os.rename(os.path.join(base, "sub", "copy.txt"), os.path.join(base, "sub", "moved.txt"));
for (entry in os.walk(base)) {
    let rel = os.path.relpath(entry[0], base);
    print(rel.replace("\\", "/"), entry[1], entry[2]);
}
os.remove(os.path.join(base, "sub", "moved.txt"));
print(os.listdir(os.path.join(base, "sub")));
print(os.getenv("FASTON_SURELY_UNSET_VAR"), os.getenv("FASTON_SURELY_UNSET_VAR", "fallback"));
print(type(sys.argv), len(sys.argv) >= 1, type(sys.platform), sys.maxsize);

# json
let data = {"name": "Ann", "tags": ["a", "b"], "n": 3, "pi": 3.5, "ok": True, "none": None, "nested": {"x": [1, {"y": []}]}};
let text = json.dumps(data);
print(text);
print(json.dumps(data, indent=2, sort_keys=True));
print(json.dumps([1, "é", "\n\"q\""]), json.dumps("é", ensure_ascii=False), json.dumps({}), json.dumps([]));
print(json.dumps({1: "one", True: "t", None: "n"}), json.dumps((1, 2)), json.dumps([1.0, 1e20, -0.5]));
print(json.loads(text) == data, json.loads("[1, 2.5, true, null, \"\\u00e9\\n\"]"));
print(json.loads("  {\"a\": {\"b\": [1, 2, {\"c\": false}]}}  "));
let jf = open(os.path.join(base, "data.json"), "w");
json.dump(data, jf, indent=4);
jf.close();
print(json.load(open(os.path.join(base, "data.json")))["nested"]);
for (bad in ["", "[1, 2", "{\"a\" 1}", "{'a': 1}", "[1] x", "tru"]) {
    try {
        json.loads(bad);
    } except (ValueError as e) {
        print("error:", e);
    }
}
try {
    json.dumps({"s": {1, 2}});
} except (TypeError as e) {
    print("TypeError:", e);
}

shutil.rmtree(base);
print(os.path.exists(base));
