# The bytecode module: compile, load, run, and build bytecode by hand.
import bytecode;

let data = bytecode.compile("def sq(x) { return x * x; }\nlet answer = sq(7);\nprint('ran', answer);", "demo");
print(type(data), data[:6], bytecode.MAGIC, bytecode.VERSION);
let f = bytecode.load(data, "demo");
f();
let m = bytecode.run(data, "demo2");
print(m.answer, m.sq(5));

# a module assembled byte by byte: print(1 + 2)
def u32(n) { return n.to_bytes(4, "little"); }
def text(s) { let b = s.encode(); return u32(len(b)) + b; }
def op(name, *args) {
    let out = bytearray([bytecode.opcodes[name]]);
    for (a in args) { out.extend(u32(a)); }
    out.extend(u32(1));    # line number
    return bytes(out);
}
let ops = [op("GetGlobal", 0), op("Const", 0), op("Const", 1), op("Add"), op("Call", 1), op("Pop"), op("None"), op("Return")];
let body = text("<module>") + u32(0) + u32(0) + bytes([0]) + u32(0) + u32(0) + u32(0)
    + u32(2) + bytes([3]) + (1).to_bytes(8, "little") + bytes([3]) + (2).to_bytes(8, "little")
    + u32(len(ops)) + b"".join(ops);
let mine = bytecode.MAGIC + (1).to_bytes(2, "little") + u32(0) + u32(1) + text("print") + body;
bytecode.load(mine)();
print(bytecode.operands["JumpIfNotLtLC"], bytecode.operands["Add"] == "", len(bytecode.opcodes) > 100);

# broken files are errors, not crashes
let broken = [mine[:20], b"nope", mine + b"x", mine[:-5] + op("Jump", 99)];
for (b in broken) {
    try { bytecode.load(b)(); print("loaded?!"); } except (ValueError as e) { print("ValueError:", str(e)[:60]); }
}
# stack underflow: Add with nothing on the stack
let bad_ops = [op("Add"), op("Return")];
let bad = bytecode.MAGIC + (1).to_bytes(2, "little") + u32(0) + u32(0) + text("<module>") + u32(0) + u32(0) + bytes([0]) + u32(0) + u32(0) + u32(0) + u32(0) + u32(2) + b"".join(bad_ops);
try { bytecode.load(bad); } except (ValueError as e) { print("ValueError:", e); }
# paths that meet with different stack depths
let mix = [op("True"), op("JumpIfFalse", 3), op("Const", 0), op("None"), op("Return")];
let mixed = bytecode.MAGIC + (1).to_bytes(2, "little") + u32(0) + u32(0) + text("<module>") + u32(0) + u32(0) + bytes([0]) + u32(0) + u32(0) + u32(0) + u32(1) + bytes([0]) + u32(len(mix)) + b"".join(mix);
try { bytecode.load(mixed); } except (ValueError as e) { print("ValueError:", e); }
print(bytecode.dis(bytecode.compile("x = 1 + 2;")).splitlines()[0]);
