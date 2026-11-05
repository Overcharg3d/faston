# Instructions as tuples: (name, operand, ...). Peephole fusion rules and jump
# targets, matching the VM's instruction set (bytecode.opcodes lists them all).

# (previous, next) -> builder of the fused instruction
const FUSE = {
    ("GetLocal", "GetLocal"): lambda (p, n) ("GetLocal2", p[1], n[1]),
    ("GetLocal", "GetAttr"): lambda (p, n) ("GetLocalAttr", p[1], n[1]),
    ("GetLocal", "AddLocal"): lambda (p, n) ("AddLocalL", n[1], p[1]),
    ("Const", "AddLocal"): lambda (p, n) ("AddLocalC", n[1], p[1]),
    ("Const", "Add"): lambda (p, n) ("AddC", p[1]),
    ("Const", "Sub"): lambda (p, n) ("SubC", p[1]),
    ("Const", "Mul"): lambda (p, n) ("MulC", p[1]),
    ("Const", "Mod"): lambda (p, n) ("ModC", p[1]),
    ("GetLocal", "AddC"): lambda (p, n) ("AddLC", p[1], n[1]),
    ("GetLocal", "SubC"): lambda (p, n) ("SubLC", p[1], n[1]),
    ("GetLocal", "MulC"): lambda (p, n) ("MulLC", p[1], n[1]),
    ("GetLocal", "Add"): lambda (p, n) ("AddL", p[1]),
    ("GetLocal", "Sub"): lambda (p, n) ("SubL", p[1]),
    ("GetLocal", "Mul"): lambda (p, n) ("MulL", p[1]),
    ("GetLocal2", "Add"): lambda (p, n) ("AddLL", p[1], p[2]),
    ("GetLocal2", "Sub"): lambda (p, n) ("SubLL", p[1], p[2]),
    ("GetLocal2", "Mul"): lambda (p, n) ("MulLL", p[1], p[2]),
    ("GetLocal", "ModC"): lambda (p, n) ("ModLC", p[1], n[1]),
    ("Const", "Eq"): lambda (p, n) ("EqC", p[1]),
    ("Const", "Ne"): lambda (p, n) ("NeC", p[1]),
    ("Const", "Lt"): lambda (p, n) ("LtC", p[1]),
    ("Const", "Le"): lambda (p, n) ("LeC", p[1]),
    ("Const", "Gt"): lambda (p, n) ("GtC", p[1]),
    ("Const", "Ge"): lambda (p, n) ("GeC", p[1]),
    ("Eq", "JumpIfFalse"): lambda (p, n) ("JumpIfNotEq", n[1]),
    ("Ne", "JumpIfFalse"): lambda (p, n) ("JumpIfNotNe", n[1]),
    ("Lt", "JumpIfFalse"): lambda (p, n) ("JumpIfNotLt", n[1]),
    ("Le", "JumpIfFalse"): lambda (p, n) ("JumpIfNotLe", n[1]),
    ("Gt", "JumpIfFalse"): lambda (p, n) ("JumpIfNotGt", n[1]),
    ("Ge", "JumpIfFalse"): lambda (p, n) ("JumpIfNotGe", n[1]),
    ("EqC", "JumpIfTrue"): lambda (p, n) ("JumpIfEqC", p[1], n[1]),
    ("NeC", "JumpIfTrue"): lambda (p, n) ("JumpIfNeC", p[1], n[1]),
    ("LtC", "JumpIfTrue"): lambda (p, n) ("JumpIfLtC", p[1], n[1]),
    ("LeC", "JumpIfTrue"): lambda (p, n) ("JumpIfLeC", p[1], n[1]),
    ("GtC", "JumpIfTrue"): lambda (p, n) ("JumpIfGtC", p[1], n[1]),
    ("GeC", "JumpIfTrue"): lambda (p, n) ("JumpIfGeC", p[1], n[1]),
    ("EqC", "JumpIfFalse"): lambda (p, n) ("JumpIfNotEqC", p[1], n[1]),
    ("NeC", "JumpIfFalse"): lambda (p, n) ("JumpIfNotNeC", p[1], n[1]),
    ("LtC", "JumpIfFalse"): lambda (p, n) ("JumpIfNotLtC", p[1], n[1]),
    ("LeC", "JumpIfFalse"): lambda (p, n) ("JumpIfNotLeC", p[1], n[1]),
    ("GtC", "JumpIfFalse"): lambda (p, n) ("JumpIfNotGtC", p[1], n[1]),
    ("GeC", "JumpIfFalse"): lambda (p, n) ("JumpIfNotGeC", p[1], n[1]),
    ("GetLocal", "JumpIfNotEqC"): lambda (p, n) ("JumpIfNotEqLC", p[1], n[1], n[2]),
    ("GetLocal", "JumpIfNotNeC"): lambda (p, n) ("JumpIfNotNeLC", p[1], n[1], n[2]),
    ("GetLocal", "JumpIfNotLtC"): lambda (p, n) ("JumpIfNotLtLC", p[1], n[1], n[2]),
    ("GetLocal", "JumpIfNotLeC"): lambda (p, n) ("JumpIfNotLeLC", p[1], n[1], n[2]),
    ("GetLocal", "JumpIfNotGtC"): lambda (p, n) ("JumpIfNotGtLC", p[1], n[1], n[2]),
    ("GetLocal", "JumpIfNotGeC"): lambda (p, n) ("JumpIfNotGeLC", p[1], n[1], n[2]),
    ("GetLocal2", "JumpIfNotEq"): lambda (p, n) ("JumpIfNotEqLL", p[1], p[2], n[1]),
    ("GetLocal2", "JumpIfNotNe"): lambda (p, n) ("JumpIfNotNeLL", p[1], p[2], n[1]),
    ("GetLocal2", "JumpIfNotLt"): lambda (p, n) ("JumpIfNotLtLL", p[1], p[2], n[1]),
    ("GetLocal2", "JumpIfNotLe"): lambda (p, n) ("JumpIfNotLeLL", p[1], p[2], n[1]),
    ("GetLocal2", "JumpIfNotGt"): lambda (p, n) ("JumpIfNotGtLL", p[1], p[2], n[1]),
    ("GetLocal2", "JumpIfNotGe"): lambda (p, n) ("JumpIfNotGeLL", p[1], p[2], n[1])
};

# The fused instruction for two neighbours, or None.
def fuse(p, n) {
    let f = FUSE.get((p[0], n[0]));
    if (f == None) { return None; }
    return f(p, n);
}

# instruction name -> index of its jump-target operand in the tuple
const TARGET = {
    "Jump": 1, "JumpIfFalse": 1, "JumpIfTrue": 1, "JumpIfFalseOrPop": 1, "JumpIfTrueOrPop": 1,
    "JumpIfNotEq": 1, "JumpIfNotNe": 1, "JumpIfNotLt": 1, "JumpIfNotLe": 1, "JumpIfNotGt": 1,
    "JumpIfNotGe": 1,
    "JumpIfNotEqC": 2, "JumpIfNotNeC": 2, "JumpIfNotLtC": 2, "JumpIfNotLeC": 2, "JumpIfNotGtC": 2,
    "JumpIfNotGeC": 2, "JumpIfEqC": 2, "JumpIfNeC": 2, "JumpIfLtC": 2, "JumpIfLeC": 2,
    "JumpIfGtC": 2, "JumpIfGeC": 2,
    "JumpIfNotEqLC": 3, "JumpIfNotNeLC": 3, "JumpIfNotLtLC": 3, "JumpIfNotLeLC": 3,
    "JumpIfNotGtLC": 3, "JumpIfNotGeLC": 3,
    "JumpIfNotEqLL": 3, "JumpIfNotNeLL": 3, "JumpIfNotLtLL": 3, "JumpIfNotLeLL": 3,
    "JumpIfNotGtLL": 3, "JumpIfNotGeLL": 3,
    "ForIter": 3, "ForLoop": 3, "SetupTry": 1
};

def jump_target(op) {
    let i = TARGET.get(op[0]);
    if (i == None) { return None; }
    return op[i];
}

def with_target(op, t) {
    let i = TARGET.get(op[0]);
    if (i == None) { return op; }
    let l = list(op);
    l[i] = t;
    return tuple(l);
}
