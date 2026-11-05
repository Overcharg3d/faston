# The Faston lexer, in Faston. Turns source text into tokens; f-strings are
# desugared here into calls of the hidden built-ins $fmt / $format, so the
# compiler never sees them.
#
# A token is a tuple (kind, value, line). Keywords and punctuation use their
# text as the kind ("let", "(", "=="); the others are "Int", "Float", "Str",
# "Bytes", "Ident" and "EOF".

class CompileError(Exception) { pass; }

const KEYWORDS = {
    "let": 1, "const": 1, "global": 1, "def": 1, "return": 1, "if": 1, "elif": 1,
    "else": 1, "while": 1, "for": 1, "in": 1, "break": 1, "continue": 1, "pass": 1,
    "class": 1, "import": 1, "lambda": 1, "nonlocal": 1, "from": 1, "try": 1,
    "except": 1, "finally": 1, "raise": 1, "as": 1, "assert": 1, "and": 1, "or": 1,
    "not": 1, "True": 1, "False": 1, "None": 1, "is": 1, "del": 1, "with": 1, "yield": 1
};

# two-character operators, then one-character ones (`&&`, `||` and `!` are aliases)
const TWO = {
    "**": "**", "//": "//", "==": "==", "!=": "!=", "<=": "<=", ">=": ">=", "+=": "+=",
    "-=": "-=", "*=": "*=", "/=": "/=", "%=": "%=", "&&": "and", "||": "or", "&=": "&=",
    "|=": "|=", "^=": "^=", "<<": "<<", ">>": ">>"
};
const ONE = {
    "&": "&", "|": "|", "^": "^", "~": "~", "@": "@", "!": "not", "(": "(", ")": ")",
    "{": "{", "}": "}", "[": "[", "]": "]", ",": ",", ".": ".", ";": ";", ":": ":",
    "+": "+", "-": "-", "*": "*", "/": "/", "%": "%", "=": "=", "<": "<", ">": ">"
};

const HEX = "0123456789abcdefABCDEF";

def is_digit(c) { return c >= "0" and c <= "9" and len(c) == 1; }

# Rust's f64 Display (no exponent), for error messages
def float_text(x) {
    if (x == int(x) and abs(x) < 1e16) { return str(int(x)); }
    return repr(x);
}

# How a token reads in an error message.
def token_text(t) {
    let k = t[0];
    if (k == "Int") { return str(t[1]); }
    if (k == "Float") { return float_text(t[1]); }
    if (k == "Str") { return "\"" + t[1] + "\""; }
    if (k == "Bytes") { return "b\"" + t[1].decode("utf-8", "replace") + "\""; }
    if (k == "Ident") { return t[1]; }
    if (k == "EOF") { return "end of file"; }
    return k;
}

class Lexer {
    def init(self, src) {
        self.chars = list(src);
        self.n = len(self.chars);
        self.i = 0;
        self.line = 1;
        self.out = [];
    }

    def peek(self, off) {
        let j = self.i + off;
        if (j < self.n) { return self.chars[j]; }
        return "\0";
    }

    def at_end(self) { return self.i >= self.n; }

    def err(self, msg) { return CompileError(f"[line {self.line}] SyntaxError: {msg}"); }

    def run(self) {
        let chars = self.chars;
        while (self.i < self.n) {
            let c = chars[self.i];
            if (c == "\n") {
                self.line += 1;
                self.i += 1;
            } elif (c.isspace()) {
                self.i += 1;
            } elif (c == "#") {
                while (self.i < self.n and chars[self.i] != "\n") { self.i += 1; }
            } elif (is_digit(c)) {
                self.number();
            } elif ((c == "f" or c == "F") and (self.peek(1) == "\"" or self.peek(1) == "'")) {
                self.i += 1;
                self.fstring();
            } else {
                let p = self.bytes_prefix();
                if (p > 0) {
                    self.i += p;
                    let line = self.line;
                    let s = self.raw_string() if p == 2 else self.string();
                    let b = bytearray();
                    for (ch in s) {
                        let o = ord(ch);
                        if (o > 255) { raise self.err("bytes can only contain ASCII characters (use \\x escapes)"); }
                        b.append(o);
                    }
                    self.out.append(("Bytes", bytes(b), line));
                } elif ((c == "r" or c == "R") and (self.peek(1) == "\"" or self.peek(1) == "'")) {
                    self.i += 1;
                    let line = self.line;
                    let s = self.raw_string();
                    self.out.append(("Str", s, line));
                } elif (c.isalpha() or c == "_") {
                    self.ident();
                } elif (c == "\"" or c == "'") {
                    let line = self.line;
                    let s = self.string();
                    self.out.append(("Str", s, line));
                } else {
                    self.symbol();
                }
            }
        }
    }

    def number(self) {
        if (self.peek(0) == "0" and (self.peek(1) == "x" or self.peek(1) == "X")) {
            self.i += 2;
            let start = self.i;
            while (self.peek(0) in HEX and self.peek(0) != "\0" or self.peek(0) == "_") { self.i += 1; }
            let text = "".join(self.chars[start:self.i]).replace("_", "");
            let v = 0;
            try {
                if (text == "") { raise ValueError("empty"); }
                v = int(text, 16);
            } except (Exception) {
                raise self.err("invalid hex literal");
            }
            self.out.append(("Int", v, self.line));
            return;
        }
        let start = self.i;
        let is_float = False;
        while (is_digit(self.peek(0)) or self.peek(0) == "_") { self.i += 1; }
        if (self.peek(0) == "." and is_digit(self.peek(1))) {
            is_float = True;
            self.i += 1;
            while (is_digit(self.peek(0)) or self.peek(0) == "_") { self.i += 1; }
        }
        if (self.peek(0) == "e" or self.peek(0) == "E") {
            let j = 1;
            if (self.peek(1) == "+" or self.peek(1) == "-") { j = 2; }
            if (is_digit(self.peek(j))) {
                is_float = True;
                self.i += j;
                while (is_digit(self.peek(0))) { self.i += 1; }
            }
        }
        let text = "".join(self.chars[start:self.i]).replace("_", "");
        if (is_float) {
            let x = 0.0;
            try { x = float(text); } except (Exception) { raise self.err(f"invalid number '{text}'"); }
            self.out.append(("Float", x, self.line));
        } else {
            let v = 0;
            try { v = int(text); } except (Exception) { raise self.err(f"integer literal too large: {text}"); }
            self.out.append(("Int", v, self.line));
        }
    }

    def ident(self) {
        let start = self.i;
        while (self.peek(0).isalnum() or self.peek(0) == "_") { self.i += 1; }
        let word = "".join(self.chars[start:self.i]);
        if (word in KEYWORDS) {
            self.out.append((word, None, self.line));
        } else {
            self.out.append(("Ident", word, self.line));
        }
    }

    # A raw string starting at the opening quote: no escape processing (a
    # backslash before the quote keeps both characters, like Python).
    def raw_string(self) {
        let quote = self.peek(0);
        let triple = self.peek(1) == quote and self.peek(2) == quote;
        self.i += 3 if triple else 1;
        let s = [];
        while (True) {
            if (self.at_end()) { raise self.err("unterminated string"); }
            let c = self.chars[self.i];
            self.i += 1;
            if (c == quote and (not triple or (self.peek(0) == quote and self.peek(1) == quote))) {
                if (triple) { self.i += 2; }
                break;
            }
            if (c == "\n") { self.line += 1; }
            s.append(c);
            if (c == "\\" and not self.at_end()) {
                let e = self.chars[self.i];
                self.i += 1;
                if (e == "\n") { self.line += 1; }
                s.append(e);
            }
        }
        return "".join(s);
    }

    # 1 for b"..", 2 for rb".." / br"..", 0 otherwise
    def bytes_prefix(self) {
        let q0 = self.peek(1) == "\"" or self.peek(1) == "'";
        let q1 = self.peek(2) == "\"" or self.peek(2) == "'";
        let a = self.peek(0).lower();
        let b = self.peek(1).lower();
        if (a == "b" and q0) { return 1; }
        if (((a == "r" and b == "b") or (a == "b" and b == "r")) and q1) { return 2; }
        return 0;
    }

    def hex_escape(self, count, what) {
        let v = 0;
        for (k in range(count)) {
            let c = "\0" if self.at_end() else self.chars[self.i];
            if (not (c in HEX) or c == "\0") { raise self.err(f"truncated \\{what} escape"); }
            v = v * 16 + int(c, 16);
            self.i += 1;
        }
        if (v > 0x10FFFF or (v >= 0xD800 and v <= 0xDFFF)) { raise self.err(f"invalid \\{what} escape"); }
        return chr(v);
    }

    # A quoted string starting at the opening quote; processes escapes.
    def string(self) {
        let quote = self.peek(0);
        let triple = self.peek(1) == quote and self.peek(2) == quote;
        self.i += 3 if triple else 1;
        let s = [];
        while (True) {
            if (self.at_end()) { raise self.err("unterminated string"); }
            let c = self.chars[self.i];
            self.i += 1;
            if (c == quote and (not triple or (self.peek(0) == quote and self.peek(1) == quote))) {
                if (triple) { self.i += 2; }
                break;
            }
            if (c == "\n") { self.line += 1; }
            if (c == "\\") {
                if (self.at_end()) { raise self.err("unterminated string"); }
                let e = self.chars[self.i];
                self.i += 1;
                match (e) {
                    case "n" { s.append("\n"); }
                    case "t" { s.append("\t"); }
                    case "r" { s.append("\r"); }
                    case "0" { s.append("\0"); }
                    case "a" { s.append("\a"); }
                    case "b" { s.append("\b"); }
                    case "f" { s.append("\f"); }
                    case "v" { s.append("\v"); }
                    case "x" { s.append(self.hex_escape(2, "x")); }
                    case "u" { s.append(self.hex_escape(4, "u")); }
                    case "U" { s.append(self.hex_escape(8, "U")); }
                    case "\\" { s.append("\\"); }
                    case "'" { s.append("'"); }
                    case "\"" { s.append("\""); }
                    case "\n" { self.line += 1; }
                    case _ { s.append("\\"); s.append(e); }
                }
            } else {
                s.append(c);
            }
        }
        return "".join(s);
    }

    # f"{name} hp: {hp:.1f}!"  ->  $fmt((name), " hp: ", $format((hp), ".1f"), "!")
    def fstring(self) {
        let line = self.line;
        let cs = list(self.string());
        let n = len(cs);
        let toks = [("Ident", "$fmt", line), ("(", None, line)];
        let lit = [];
        let first = True;
        let j = 0;
        while (j < n) {
            let c = cs[j];
            if (c == "{" and j + 1 < n and cs[j + 1] == "{") { lit.append("{"); j += 2; continue; }
            if (c == "}" and j + 1 < n and cs[j + 1] == "}") { lit.append("}"); j += 2; continue; }
            if (c == "}") { raise CompileError(f"[line {line}] SyntaxError: single '}}' in f-string"); }
            if (c == "{") {
                if (len(lit) > 0) {
                    if (not first) { toks.append((",", None, line)); }
                    first = False;
                    toks.append(("Str", "".join(lit), line));
                    lit = [];
                }
                let depth = 0;
                let k = j + 1;
                let quote = None;
                let colon = None;
                let conv = None;
                while (k < n) {
                    let ch = cs[k];
                    if (quote != None) {
                        if (ch == quote) { quote = None; }
                    } elif (ch == "\"" or ch == "'") {
                        quote = ch;
                    } elif (ch == "{" or ch == "[" or ch == "(") {
                        depth += 1;
                    } elif (ch == ")" or ch == "]") {
                        depth -= 1;
                    } elif (ch == "}") {
                        if (depth == 0) { break; }
                        depth -= 1;
                    } elif (ch == ":" and depth == 0 and colon == None) {
                        colon = k;
                    } elif (ch == "!" and depth == 0 and colon == None and conv == None
                            and k + 2 < n and cs[k + 1] in "rsa" and (cs[k + 2] == ":" or cs[k + 2] == "}")) {
                        conv = k;
                    }
                    k += 1;
                }
                if (k >= n) { raise CompileError(f"[line {line}] SyntaxError: unterminated '{{' in f-string"); }
                let expr_end = k;
                if (colon != None) { expr_end = colon; }
                if (conv != None) { expr_end = conv; }
                let inner = "".join(cs[j + 1:expr_end]);
                if (inner.strip() == "") { raise CompileError(f"[line {line}] SyntaxError: empty expression in f-string"); }
                let inner_toks = [];
                try {
                    inner_toks = lex(inner);
                } except (CompileError as e) {
                    raise CompileError(f"[line {line}] SyntaxError in f-string: {e}");
                }
                inner_toks.pop();   # EOF
                if (conv != None) {
                    let f = {"r": "$repr", "a": "$ascii", "s": "$str"}[cs[conv + 1]];
                    inner_toks = [("Ident", f, line), ("(", None, line)] + inner_toks + [(")", None, line)];
                }
                if (not first) { toks.append((",", None, line)); }
                first = False;
                if (colon == None) {
                    toks.append(("(", None, line));
                    for (t in inner_toks) { toks.append((t[0], t[1], line)); }
                    toks.append((")", None, line));
                } else {
                    let spec = "".join(cs[colon + 1:k]);
                    toks.append(("Ident", "$format", line));
                    toks.append(("(", None, line));
                    toks.append(("(", None, line));
                    for (t in inner_toks) { toks.append((t[0], t[1], line)); }
                    toks.append((")", None, line));
                    toks.append((",", None, line));
                    toks.append(("Str", spec, line));
                    toks.append((")", None, line));
                }
                j = k + 1;
                continue;
            }
            lit.append(c);
            j += 1;
        }
        if (len(lit) > 0) {
            if (not first) { toks.append((",", None, line)); }
            toks.append(("Str", "".join(lit), line));
        }
        toks.append((")", None, line));
        self.out.extend(toks);
    }

    def symbol(self) {
        let c = self.peek(0);
        let two = c + self.peek(1);
        if (two in TWO) {
            self.out.append((TWO[two], None, self.line));
            self.i += 2;
            return;
        }
        if (c in ONE) {
            self.out.append((ONE[c], None, self.line));
            self.i += 1;
            return;
        }
        raise self.err(f"unexpected character '{c}'");
    }
}

# Source -> tokens (ending with an EOF token).
def lex(src) {
    let lx = Lexer(src);
    lx.run();
    lx.out.append(("EOF", None, lx.line));
    return lx.out;
}
