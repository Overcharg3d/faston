'use strict';
// A small, error-tolerant front end for Faston, written for the editor:
// lexer (mirrors src/lexer.rs), structural parser (scopes and symbols), and
// name / member / type resolution. It never throws on bad input; partial
// code still produces useful results.

const BI = require('./builtins');

const KEYWORDS = new Set(BI.KEYWORDS);
const OPEN = { '(': ')', '[': ']', '{': '}' };
const CLOSE = { ')': '(', ']': '[', '}': '{' };
const TWO_CHAR = new Set(['**', '//', '==', '!=', '<=', '>=', '+=', '-=', '*=', '/=', '%=',
    '&&', '||', '&=', '|=', '^=', '<<', '>>']);

// ---------------------------------------------------------------- lexer

/**
 * @typedef {{ t: 'id'|'kw'|'num'|'str'|'fstr'|'op', v: string, s: number, e: number,
 *             grp?: Token[], i?: number, unterminated?: boolean, skip?: boolean }} Token
 */

function isIdStart(c) {
    return /[\p{L}_]/u.test(c);
}
function isIdPart(c) {
    return /[\p{L}\p{N}_]/u.test(c);
}

/** Finds the end (index after the closing quote) of a string starting at `i` (the quote). */
function scanString(text, i) {
    const q = text[i];
    const triple = text[i + 1] === q && text[i + 2] === q;
    const ql = triple ? 3 : 1;
    let k = i + ql;
    while (k < text.length) {
        const c = text[k];
        if (c === '\\') { k += 2; continue; }
        if (c === q && (!triple || (text[k + 1] === q && text[k + 2] === q))) return { end: k + ql, unterminated: false, ql };
        k++;
    }
    return { end: text.length, unterminated: true, ql };
}

/**
 * Lexes `text`. Offsets are shifted by `base` (used for f-string interpolations).
 * Returns main tokens, comments and the token groups of f-string interpolations.
 */
function lex(text, base = 0) {
    const tokens = [];
    const comments = [];
    const groups = []; // { s, e, tokens } for each f-string interpolation
    let i = 0;
    const n = text.length;
    while (i < n) {
        const c = text[i];
        if (c === ' ' || c === '\t' || c === '\r' || c === '\n' || /\s/.test(c)) { i++; continue; }
        if (c === '#') {
            let j = text.indexOf('\n', i);
            if (j < 0) j = n;
            let end = j;
            if (end > i && text[end - 1] === '\r') end--;
            comments.push({ s: base + i, e: base + end, v: text.slice(i, end) });
            i = j;
            continue;
        }
        if (c >= '0' && c <= '9') {
            const re = /0[xX][0-9a-fA-F_]*|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?/y;
            re.lastIndex = i;
            const m = re.exec(text);
            const len = m ? m[0].length : 1;
            tokens.push({ t: 'num', v: text.slice(i, i + len), s: base + i, e: base + i + len });
            i += len;
            continue;
        }
        if ((c === 'f' || c === 'F') && (text[i + 1] === '"' || text[i + 1] === "'")) {
            const { end, unterminated, ql } = scanString(text, i + 1);
            const tok = { t: 'fstr', v: text.slice(i, end), s: base + i, e: base + end, unterminated };
            tokens.push(tok);
            // interpolations: {expr} or {expr:spec}; {{ and }} are literal braces
            const contentStart = i + 1 + ql;
            const contentEnd = unterminated ? end : end - ql;
            let k = contentStart;
            while (k < contentEnd) {
                const ch = text[k];
                if (ch === '\\') { k += 2; continue; }
                if (ch === '{' && text[k + 1] === '{') { k += 2; continue; }
                if (ch === '}' && text[k + 1] === '}') { k += 2; continue; }
                if (ch === '{') {
                    let depth = 0, quote = null, colon = -1, m = k + 1;
                    for (; m < contentEnd; m++) {
                        const x = text[m];
                        if (quote) { if (x === quote) quote = null; continue; }
                        if (x === '"' || x === "'") quote = x;
                        else if (x === '(' || x === '[' || x === '{') depth++;
                        else if (x === ')' || x === ']') depth--;
                        else if (x === '}') { if (depth === 0) break; depth--; }
                        else if (x === ':' && depth === 0 && colon < 0) colon = m;
                    }
                    const exprEnd = colon >= 0 ? colon : m;
                    const inner = lex(text.slice(k + 1, exprEnd), base + k + 1);
                    groups.push({ s: base + k + 1, e: base + exprEnd, tokens: inner.tokens, spec: colon >= 0 ? { s: base + colon, e: base + m } : null });
                    for (const g of inner.groups) groups.push(g);
                    k = m + 1;
                    continue;
                }
                k++;
            }
            i = end;
            continue;
        }
        if ((c === 'r' || c === 'R') && (text[i + 1] === '"' || text[i + 1] === "'")) {
            // raw string: scanString's backslash rule matches Python's raw strings
            const { end, unterminated } = scanString(text, i + 1);
            tokens.push({ t: 'str', v: text.slice(i, end), s: base + i, e: base + end, unterminated });
            i = end;
            continue;
        }
        if (isIdStart(c)) {
            let j = i + 1;
            while (j < n && isIdPart(text[j])) j++;
            const w = text.slice(i, j);
            tokens.push({ t: KEYWORDS.has(w) ? 'kw' : 'id', v: w, s: base + i, e: base + j });
            i = j;
            continue;
        }
        if (c === '"' || c === "'") {
            const { end, unterminated } = scanString(text, i);
            tokens.push({ t: 'str', v: text.slice(i, end), s: base + i, e: base + end, unterminated });
            i = end;
            continue;
        }
        const two = text.slice(i, i + 2);
        const len = TWO_CHAR.has(two) ? 2 : 1;
        tokens.push({ t: 'op', v: text.slice(i, i + len), s: base + i, e: base + i + len });
        i += len;
    }
    return { tokens, comments, groups };
}

/** match[i] = index of the bracket matching tokens[i] (or -1). Unclosed ones match the end. */
function matchBrackets(tokens) {
    const match = new Array(tokens.length).fill(-1);
    const stack = [];
    for (let i = 0; i < tokens.length; i++) {
        const t = tokens[i];
        if (t.t !== 'op') continue;
        if (OPEN[t.v]) stack.push(i);
        else if (CLOSE[t.v]) {
            // tolerate mismatches: pop until the right kind (or give up)
            let k = stack.length - 1;
            while (k >= 0 && tokens[stack[k]].v !== CLOSE[t.v]) k--;
            if (k < 0) continue;
            while (stack.length - 1 > k) {
                const o = stack.pop();
                match[o] = tokens.length - 1;
            }
            const o = stack.pop();
            match[o] = i;
            match[i] = o;
        }
    }
    for (const o of stack) match[o] = tokens.length - 1;
    return match;
}

// ---------------------------------------------------------------- scopes

class Scope {
    constructor(kind, name, s, e, parent, owner) {
        this.kind = kind; // module | function | class | lambda
        this.name = name;
        this.s = s;
        this.e = e;
        this.parent = parent;
        this.owner = owner || null; // the def/class symbol
        this.children = [];
        /** @type {Map<string, any>} */
        this.symbols = new Map();
        this.order = [];
        this.globals = new Set();
        this.nonlocals = new Set();
        /** class scopes: fields assigned through self.x = ... */
        this.fields = new Map();
        if (parent) parent.children.push(this);
    }
    get(name) {
        return this.symbols.get(name);
    }
}

// ---------------------------------------------------------------- analysis

class Analysis {
    constructor(text, uri) {
        this.text = text;
        this.uri = uri || null;
        /** @type {(name: string) => Analysis|null} */
        this.getModule = null;
        this.lineStarts = [0];
        for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) this.lineStarts.push(i + 1);

        const r = lex(text);
        this.tokens = r.tokens;
        this.comments = r.comments;
        this.tokens.forEach((t, i) => { t.grp = this.tokens; t.i = i; });
        this.tokens.match = matchBrackets(this.tokens);
        this.groups = r.groups;
        for (const g of this.groups) {
            g.tokens.forEach((t, i) => { t.grp = g.tokens; t.i = i; t.inF = true; });
            g.tokens.match = matchBrackets(g.tokens);
        }
        this.all = this.tokens.concat(...this.groups.map((g) => g.tokens)).sort((a, b) => a.s - b.s);

        this.commentByLine = new Map();
        for (const c of this.comments) {
            const line = this.lineOf(c.s);
            const before = text.slice(this.lineStarts[line], c.s);
            this.commentByLine.set(line, { c, full: before.trim() === '' });
        }

        this.module = new Scope('module', '<module>', 0, text.length + 1, null);
        this.scopes = [this.module];
        this.symbols = [];
        this.imports = []; // { module, tok, names: [{ name, tok }] }
        try {
            this.parse();
        } catch (e) {
            // never let a parser bug break the editor
            this.parseError = e;
        }
    }

    // ------------------------------------------------------------ positions

    lineOf(offset) {
        const ls = this.lineStarts;
        let lo = 0, hi = ls.length - 1;
        while (lo < hi) {
            const mid = (lo + hi + 1) >> 1;
            if (ls[mid] <= offset) lo = mid; else hi = mid - 1;
        }
        return lo;
    }
    posOf(offset) {
        const line = this.lineOf(offset);
        return { line, character: offset - this.lineStarts[line] };
    }
    offsetOf(line, character) {
        if (line >= this.lineStarts.length) return this.text.length;
        return Math.min(this.lineStarts[line] + character, this.text.length);
    }
    lineText(line) {
        const s = this.lineStarts[line];
        const e = line + 1 < this.lineStarts.length ? this.lineStarts[line + 1] : this.text.length;
        return this.text.slice(s, e).replace(/\r?\n$/, '');
    }

    // ------------------------------------------------------------ parsing

    define(scope, props) {
        let target = scope;
        if (props.kind === 'variable' || props.kind === 'constant') {
            if (scope.globals.has(props.name)) target = this.module;
            else if (scope.nonlocals.has(props.name)) return null;
        }
        const existing = target.symbols.get(props.name);
        if (existing) {
            if (!existing.hint && props.hint) existing.hint = props.hint;
            return existing;
        }
        const sym = Object.assign({ scope: target, doc: '' }, props);
        target.symbols.set(sym.name, sym);
        target.order.push(sym);
        this.symbols.push(sym);
        return sym;
    }

    docFor(tok) {
        const line = this.lineOf(tok.s);
        const out = [];
        for (let l = line - 1; l >= 0; l--) {
            const c = this.commentByLine.get(l);
            if (!c || !c.full) break;
            out.unshift(c.c.v);
        }
        if (!out.length) {
            for (let l = line + 1; ; l++) {
                const c = this.commentByLine.get(l);
                if (!c || !c.full) break;
                out.push(c.c.v);
            }
        }
        return cleanComment(out);
    }

    trailingDoc(tok) {
        const c = this.commentByLine.get(this.lineOf(tok.s));
        return c && !c.full ? cleanComment([c.c.v]) : '';
    }

    parseParams(T, from, to, scope, method) {
        const params = [];
        let k = from;
        while (k < to) {
            let star = '';
            if (T[k].v === '*' || T[k].v === '**') { star = T[k].v; k++; }
            if (k < to && T[k].t === 'id') {
                const p = { name: T[k].v, tok: T[k], star, def: null };
                k++;
                if (k < to && T[k].v === '=') {
                    k++;
                    const ds = k;
                    let depth = 0;
                    while (k < to && !(depth === 0 && T[k].v === ',')) {
                        if (OPEN[T[k].v]) depth++;
                        else if (CLOSE[T[k].v]) depth--;
                        k++;
                    }
                    if (k > ds) p.def = this.text.slice(T[ds].s, T[k - 1].e);
                    p.hint = this.valueHint(T, ds);
                }
                params.push(p);
            }
            while (k < to && T[k].v !== ',') k++;
            k++;
        }
        params.forEach((p, idx) => {
            const sym = this.define(scope, {
                name: p.name, kind: 'parameter', tok: p.tok, star: p.star, def: p.def, hint: p.hint,
            });
            if (sym && method && idx === 0 && !p.star) {
                sym.isSelf = true;
                sym.selfClass = method;
            }
        });
        return params;
    }

    valueHint(T, k) {
        const t = T[k];
        if (!t) return null;
        const M = T.match;
        const next = T[k + 1];
        const simpleEnd = (j) => !T[j] || [';', ',', ')', ']', '}', 'as'].includes(T[j].v);
        // a postfix chain ending in a call: Foo(...), a.b(...), "x".split(), make()(...)
        if (t.t === 'id' || t.t === 'str' || t.t === 'fstr') {
            let j = k + 1, last = k;
            while (T[j]) {
                if (T[j].v === '.' && T[j + 1] && T[j + 1].t === 'id') { last = j + 1; j += 2; continue; }
                if ((T[j].v === '(' || T[j].v === '[') && M[j] > j) { last = M[j]; j = M[j] + 1; continue; }
                break;
            }
            if (last > k && simpleEnd(last + 1) && T[last].v === ')') {
                const o = M[last];
                if (T[o - 1] && T[o - 1].t === 'id') return { callAt: o - 1, grp: T };
            }
        }
        if (t.t === 'str' || t.t === 'fstr') return simpleEnd(k + 1) || (next && next.v === '+') ? { type: 'str' } : null;
        if (t.t === 'num') {
            if (!simpleEnd(k + 1)) return null;
            return { type: /^0[xX]/.test(t.v) || !/[.eE]/.test(t.v) ? 'int' : 'float' };
        }
        if (t.t === 'kw') {
            if (t.v === 'True' || t.v === 'False') return { type: 'bool' };
            if (t.v === 'lambda') return { type: 'function' };
            return null;
        }
        if (t.v === '[') return simpleEnd(M[k] + 1) ? { type: 'list' } : null;
        if (t.v === '{') {
            if (!simpleEnd(M[k] + 1)) return null;
            if (T[k + 1] && T[k + 1].v === '}') return { type: 'dict' };
            let depth = 0;
            for (let j = k + 1; j < M[k]; j++) {
                const v = T[j].v;
                if (OPEN[v]) depth++;
                else if (CLOSE[v]) depth--;
                else if (depth === 0 && v === ':') return { type: 'dict' };
                else if (depth === 0 && (v === ',' || (T[j].t === 'kw' && v === 'for'))) break;
            }
            return { type: 'set' };
        }
        if (t.v === '(') {
            if (!simpleEnd(M[k] + 1)) return null;
            if (T[k + 1] && T[k + 1].v === ')') return { type: 'tuple' };
            let depth = 0;
            for (let j = k + 1; j < M[k]; j++) {
                const v = T[j].v;
                if (OPEN[v]) depth++;
                else if (CLOSE[v]) depth--;
                else if (depth === 0 && v === ',') return { type: 'tuple' };
            }
            return null;
        }
        if (t.t === 'id') {
            if (next && next.v === '(' && simpleEnd(M[k + 1] + 1)) return { call: t.v };
            if (next && next.v === '.' && T[k + 2] && T[k + 2].t === 'id' && T[k + 3] && T[k + 3].v === '('
                && simpleEnd(M[k + 3] + 1)) {
                return { call: t.v + '.' + T[k + 2].v };
            }
            if (simpleEnd(k + 1)) return { alias: t.v };
            // `a, b` tuple of names
            if (next && next.v === ',') return null;
        }
        return null;
    }

    parse() {
        const T = this.tokens;
        const M = T.match;
        const stack = [{ scope: this.module, close: Infinity }];
        const end = (idx) => (T[idx] ? T[idx].e : this.text.length);

        for (let i = 0; i < T.length; i++) {
            while (stack.length > 1 && i > stack[stack.length - 1].close) stack.pop();
            const scope = stack[stack.length - 1].scope;
            const t = T[i];
            const prev = T[i - 1];
            const atStmt = !prev || prev.v === ';' || prev.v === '{' || prev.v === '}';

            if (t.t === 'kw') {
                switch (t.v) {
                    case 'def': {
                        const nameTok = T[i + 1];
                        if (!nameTok || nameTok.t !== 'id') break;
                        const lp = i + 2;
                        const hasParams = T[lp] && T[lp].v === '(';
                        const rp = hasParams ? M[lp] : i + 1;
                        const body = T[rp + 1] && T[rp + 1].v === '{' ? rp + 1 : -1;
                        const close = body >= 0 ? M[body] : rp;
                        const isMethod = scope.kind === 'class';
                        const sym = this.define(scope, {
                            name: nameTok.v, kind: isMethod ? 'method' : 'function', tok: nameTok,
                            doc: this.docFor(t), fullStart: t.s, fullEnd: end(close),
                            classSym: isMethod ? scope.owner : null, params: [],
                        });
                        const fs = new Scope('function', nameTok.v, hasParams ? T[lp].s : nameTok.e, end(close), scope, sym);
                        this.scopes.push(fs);
                        const params = hasParams ? this.parseParams(T, lp + 1, rp, fs, isMethod ? scope.owner : null) : [];
                        if (sym && sym.tok === nameTok) {
                            sym.params = params;
                            sym.inner = fs;
                        }
                        stack.push({ scope: fs, close });
                        // walk the parameter list too: defaults can contain lambdas
                        i = hasParams ? lp : rp;
                        continue;
                    }
                    case 'class': {
                        const nameTok = T[i + 1];
                        if (!nameTok || nameTok.t !== 'id') break;
                        let j = i + 2;
                        let base = null;
                        if (T[j] && T[j].v === '(') {
                            const c = M[j];
                            const ids = [];
                            for (let k = j + 1; k < c; k++) if (T[k].t === 'id' || T[k].v === '.') ids.push(T[k].v);
                            base = ids.join('') || null;
                            j = c + 1;
                        }
                        const body = T[j] && T[j].v === '{' ? j : -1;
                        const close = body >= 0 ? M[body] : j - 1;
                        const sym = this.define(scope, {
                            name: nameTok.v, kind: 'class', tok: nameTok, base, doc: this.docFor(t),
                            fullStart: t.s, fullEnd: end(close),
                        });
                        const cs = new Scope('class', nameTok.v, body >= 0 ? T[body].s : nameTok.e, end(close), scope, sym);
                        this.scopes.push(cs);
                        if (sym && sym.tok === nameTok) sym.members = cs;
                        stack.push({ scope: cs, close });
                        i = body >= 0 ? body : j - 1;
                        continue;
                    }
                    case 'lambda': {
                        if (!T[i + 1] || T[i + 1].v !== '(') break;
                        const rp = M[i + 1];
                        let close;
                        if (T[rp + 1] && T[rp + 1].v === '{') close = M[rp + 1];
                        else close = this.exprEnd(T, rp + 1);
                        const ls = new Scope('lambda', 'lambda', T[i + 1].s, end(close), scope, null);
                        this.scopes.push(ls);
                        this.parseParams(T, i + 2, rp, ls, null);
                        stack.push({ scope: ls, close });
                        i = rp;
                        continue;
                    }
                    case 'let':
                    case 'const': {
                        if (!atStmt) break;
                        let j = i + 1;
                        const names = [];
                        while (T[j] && T[j].t === 'id') {
                            names.push(T[j]);
                            if (T[j + 1] && T[j + 1].v === ',' && T[j + 2] && T[j + 2].t === 'id') j += 2;
                            else { j++; break; }
                        }
                        const hint = T[j] && T[j].v === '=' && names.length === 1 ? this.valueHint(T, j + 1) : null;
                        for (const nt of names) {
                            this.define(scope, {
                                name: nt.v, kind: t.v === 'const' ? 'constant' : 'variable', tok: nt,
                                declared: t.v, hint, doc: this.trailingDoc(nt) || this.docFor(t),
                            });
                        }
                        i = j - 1;
                        continue;
                    }
                    case 'global':
                    case 'nonlocal': {
                        let j = i + 1;
                        while (T[j] && T[j].t === 'id') {
                            (t.v === 'global' ? scope.globals : scope.nonlocals).add(T[j].v);
                            if (T[j + 1] && T[j + 1].v === ',') j += 2; else { j++; break; }
                        }
                        i = j - 1;
                        continue;
                    }
                    case 'import': {
                        // import a, b.c, d.e as f;
                        let j = i + 1;
                        while (T[j] && T[j].t === 'id') {
                            const d = this.dotted(T, j);
                            j = d.next;
                            if (T[j] && T[j].v === 'as' && T[j + 1] && T[j + 1].t === 'id') {
                                // the dotted name only names a module here
                                for (const t of d.toks) {
                                    t.skip = true;
                                    t.modPrefix = d.prefixes.get(t);
                                }
                                const nameTok = T[j + 1];
                                this.define(scope, { name: nameTok.v, kind: 'module', tok: nameTok, module: d.name, modTok: d.toks[d.toks.length - 1] });
                                j += 2;
                            } else {
                                // `import a.b` binds `a`; `.b` resolves as a submodule
                                const first = d.toks[0];
                                this.define(scope, { name: first.v, kind: 'module', tok: first, module: first.v, modTok: first });
                            }
                            this.imports.push({ module: d.name, tok: d.toks[0], toks: d.toks, names: null });
                            if (T[j] && T[j].v === ',') j += 1; else break;
                        }
                        i = j;
                        continue;
                    }
                    case 'from': {
                        // from a.b import c, d as e;
                        if (!T[i + 1] || T[i + 1].t !== 'id') break;
                        const d = this.dotted(T, i + 1);
                        if (!T[d.next] || T[d.next].v !== 'import') break;
                        for (const t of d.toks) {
                            t.skip = true;
                            t.modPrefix = d.prefixes.get(t);
                        }
                        const modName = d.name;
                        let j = d.next + 1;
                        const entry = { module: modName, tok: d.toks[0], toks: d.toks, names: [] };
                        this.imports.push(entry);
                        while (T[j] && T[j].t === 'id') {
                            const orig = T[j];
                            let nameTok = orig;
                            if (T[j + 1] && T[j + 1].v === 'as' && T[j + 2] && T[j + 2].t === 'id') {
                                orig.skip = true;
                                orig.importOf = { module: modName, name: orig.v };
                                nameTok = T[j + 2];
                                j += 2;
                            }
                            entry.names.push({ name: orig.v, tok: orig });
                            this.define(scope, {
                                name: nameTok.v, kind: 'import', tok: nameTok, module: modName, imported: orig.v,
                            });
                            if (T[j + 1] && T[j + 1].v === ',') j += 2; else { j++; break; }
                        }
                        i = j - 1;
                        continue;
                    }
                    case 'for': {
                        let j = i + 1;
                        const names = [];
                        while (T[j] && !(T[j].t === 'kw' && T[j].v === 'in') && T[j].v !== ';' && T[j].v !== '{') {
                            if (T[j].t === 'id') names.push(T[j]);
                            j++;
                        }
                        const loop = T[i + 1] && T[i + 1].v === '(';
                        for (const nt of names) this.define(scope, { name: nt.v, kind: 'variable', tok: nt, loopVar: true, comp: !loop });
                        break;
                    }
                    case 'with': {
                        // with (expr as name, expr2 as name2) { ... }
                        if (!T[i + 1] || T[i + 1].v !== '(') break;
                        const c = M[i + 1];
                        let start = i + 2;
                        let depth = 0;
                        for (let k = i + 2; k <= c && k < T.length; k++) {
                            const v = T[k].v;
                            if (k < c && T[k].t === 'op' && OPEN[v]) depth++;
                            else if (k < c && T[k].t === 'op' && CLOSE[v]) depth--;
                            if (k === c || (depth === 0 && v === ',')) {
                                for (let j = start; j < k; j++) {
                                    if (T[j].t === 'kw' && T[j].v === 'as' && T[j + 1] && T[j + 1].t === 'id') {
                                        this.define(scope, { name: T[j + 1].v, kind: 'variable', tok: T[j + 1], hint: this.valueHint(T, start) });
                                    }
                                }
                                start = k + 1;
                            }
                        }
                        break;
                    }
                    case 'except': {
                        if (!T[i + 1] || T[i + 1].v !== '(') break;
                        const c = M[i + 1];
                        let exc = null;
                        for (let k = i + 2; k < c; k++) {
                            if (T[k].t === 'id' && exc === null) exc = T[k].v;
                            if (T[k].v === 'as' && T[k + 1] && T[k + 1].t === 'id') {
                                this.define(scope, { name: T[k + 1].v, kind: 'variable', tok: T[k + 1], hint: exc ? { exc } : null });
                                break;
                            }
                        }
                        break;
                    }
                }
                continue;
            }

            if (t.t !== 'id') continue;

            // self.field = value  (inside a method)
            if (t.v !== undefined && prev?.v !== '.' && T[i + 1] && T[i + 1].v === '.' && T[i + 2] && T[i + 2].t === 'id'
                && T[i + 3] && T[i + 3].v === '=') {
                const fnScope = scope.kind === 'function' ? scope : null;
                const selfSym = fnScope && fnScope.get(t.v);
                if (selfSym && selfSym.isSelf && selfSym.selfClass && selfSym.selfClass.members) {
                    const cls = selfSym.selfClass.members;
                    const ft = T[i + 2];
                    if (!cls.fields.has(ft.v)) {
                        const f = {
                            name: ft.v, kind: 'field', tok: ft, scope: cls, classSym: selfSym.selfClass,
                            hint: this.valueHint(T, i + 4), doc: this.trailingDoc(ft),
                        };
                        cls.fields.set(ft.v, f);
                        this.symbols.push(f);
                    } else {
                        const f = cls.fields.get(ft.v);
                        if (!f.hint) f.hint = this.valueHint(T, i + 4);
                    }
                }
                continue;
            }

            // name = ... / a, b = ...
            if (atStmt) {
                let j = i;
                const names = [];
                while (T[j] && T[j].t === 'id') {
                    names.push(T[j]);
                    if (T[j + 1] && T[j + 1].v === ',' && T[j + 2] && T[j + 2].t === 'id') j += 2;
                    else { j++; break; }
                }
                if (T[j] && T[j].v === '=') {
                    const hint = names.length === 1 ? this.valueHint(T, j + 1) : null;
                    for (const nt of names) {
                        this.define(scope, {
                            name: nt.v, kind: scope.kind === 'class' ? 'field' : 'variable', tok: nt, hint,
                            doc: this.trailingDoc(nt), classSym: scope.kind === 'class' ? scope.owner : undefined,
                        });
                    }
                    i = j;
                }
            }
        }
    }

    /** A dotted module name starting at token `j`: { name, toks, prefixes (tok -> "a.b"), next }. */
    dotted(T, j) {
        const toks = [T[j]];
        const prefixes = new Map([[T[j], T[j].v]]);
        let name = T[j].v;
        j++;
        while (T[j] && T[j].v === '.' && T[j + 1] && T[j + 1].t === 'id') {
            name += '.' + T[j + 1].v;
            toks.push(T[j + 1]);
            prefixes.set(T[j + 1], name);
            j += 2;
        }
        return { name, toks, prefixes, next: j };
    }

    /** Index of the last token of an expression starting at `k` (stops at , ; or an unmatched closer). */
    exprEnd(T, k) {
        let depth = 0;
        for (let j = k; j < T.length; j++) {
            const v = T[j].v;
            if (T[j].t === 'op') {
                if (OPEN[v]) depth++;
                else if (CLOSE[v]) {
                    if (depth === 0) return j - 1;
                    depth--;
                } else if (depth === 0 && (v === ',' || v === ';')) return j - 1;
            }
        }
        return T.length - 1;
    }

    // ------------------------------------------------------------ queries

    scopeAt(offset) {
        let sc = this.module;
        for (;;) {
            const child = sc.children.find((c) => c.s <= offset && offset < c.e);
            if (!child) return sc;
            sc = child;
        }
    }

    /** The symbol a plain name refers to at `offset` (Python's scoping rules). */
    lookup(name, offset) {
        const start = this.scopeAt(offset);
        let sc = start;
        while (sc) {
            if (sc.kind === 'class' && sc !== start) { sc = sc.parent; continue; }
            if (sc.globals.has(name)) return this.module.get(name) || null;
            const s = sc.get(name);
            if (s) return s;
            sc = sc.parent;
        }
        return null;
    }

    /** All names visible at `offset`, innermost first. */
    visible(offset) {
        const out = new Map();
        const start = this.scopeAt(offset);
        let sc = start;
        while (sc) {
            if (!(sc.kind === 'class' && sc !== start)) {
                for (const s of sc.order) if (!out.has(s.name)) out.set(s.name, s);
            }
            sc = sc.parent;
        }
        return out;
    }

    /** The class whose method contains `offset`. */
    enclosingClass(offset) {
        let sc = this.scopeAt(offset);
        while (sc) {
            if (sc.kind === 'class') return sc.owner;
            sc = sc.parent;
        }
        return null;
    }

    /** The token at `offset` (identifiers win when the cursor touches two tokens). */
    tokenAt(offset) {
        const a = this.all;
        let lo = 0, hi = a.length - 1, found = -1;
        while (lo <= hi) {
            const mid = (lo + hi) >> 1;
            if (a[mid].s <= offset) { found = mid; lo = mid + 1; } else hi = mid - 1;
        }
        if (found < 0) return null;
        const t = a[found];
        if (offset < t.e) return t;
        if (offset === t.e) {
            return t;
        }
        return null;
    }

    /** The token group (main code or one f-string interpolation) that `offset` is in. */
    groupAt(offset) {
        for (const g of this.groups) if (g.s <= offset && offset <= g.e) return g.tokens;
        return this.tokens;
    }

    /** Is `offset` inside a comment or a plain string (where completion makes no sense)? */
    inCommentOrString(offset) {
        for (const c of this.comments) if (c.s < offset && offset <= c.e) return true;
        const t = this.tokens.find((x) => x.s < offset && (offset < x.e || (x.unterminated && offset <= x.e)));
        if (!t) return false;
        if (t.t === 'str') return true;
        if (t.t === 'fstr') return !this.groups.some((g) => g.s <= offset && offset <= g.e);
        return false;
    }
}

function cleanComment(lines) {
    return lines.map((l) => l.replace(/^#\s?/, '').replace(/^-{2,}\s*/, '')).join('\n').trim();
}

// ---------------------------------------------------------------- resolution

function resolveName(an, name, offset, depth = 0) {
    const sym = an.lookup(name, offset);
    if (sym) return followSym(an, sym, depth);
    if (BI.FUNCTIONS[name]) return { kind: 'builtinFn', name };
    if (BI.EXCEPTIONS[name]) return { kind: 'exception', name };
    return null;
}

/** Follows `from m import x` to the symbol it names. */
function followSym(an, sym, depth = 0) {
    if (sym.kind === 'import' && depth < 8) {
        const bm = BI.MODULES[sym.module];
        if (bm) {
            if (bm.members[sym.imported]) {
                return { kind: 'moduleMember', module: sym.module, name: sym.imported, via: { an, sym } };
            }
        } else {
            const mod = an.getModule && an.getModule(sym.module);
            if (mod) {
                const target = mod.module.get(sym.imported);
                if (target) {
                    const r = followSym(mod, target, depth + 1);
                    if (!r.via) r.via = { an, sym };
                    return r;
                }
            }
        }
    }
    return { kind: 'symbol', an, sym };
}

function typeOfEntity(ent, depth = 0) {
    if (!ent || depth > 10) return null;
    switch (ent.kind) {
        case 'builtinFn': return { t: 'function' };
        case 'exception': return { t: 'exceptionClass', name: ent.name };
        case 'moduleRef': {
            if (BI.MODULES[ent.name]) return { t: 'module', builtin: ent.name };
            const mod = ent.an.getModule && ent.an.getModule(ent.name);
            return mod ? { t: 'module', an: mod, name: ent.name, importer: ent.an } : null;
        }
        case 'moduleMember': {
            const m = BI.MODULES[ent.module].members[ent.name];
            if (m && m.module) return { t: 'module', builtin: m.module };
            if (m && m.value) return { t: 'builtin', name: typeof m.value === 'string' ? m.value : 'float' };
            return { t: 'function' };
        }
        case 'symbol': {
            const { an, sym } = ent;
            switch (sym.kind) {
                case 'class': return { t: 'class', an, sym };
                case 'module': {
                    if (BI.MODULES[sym.module]) return { t: 'module', builtin: sym.module };
                    const mod = an.getModule && an.getModule(sym.module);
                    return mod ? { t: 'module', an: mod, name: sym.module, importer: an } : null;
                }
                case 'function':
                case 'method':
                    return { t: 'function' };
                case 'parameter':
                    if (sym.isSelf && sym.selfClass) return { t: 'instance', an, sym: sym.selfClass };
                    return sym.hint ? typeFromHint(an, sym.hint, sym.tok.s, depth + 1) : null;
                default:
                    return sym.hint ? typeFromHint(an, sym.hint, sym.tok.s, depth + 1) : null;
            }
        }
    }
    return null;
}

function typeFromHint(an, hint, offset, depth) {
    if (depth > 10) return null;
    if (hint.type) return { t: 'builtin', name: hint.type };
    if (hint.callAt !== undefined) {
        if (hint.busy) return null;
        hint.busy = true;
        try {
            return instanceOf(entityOfIdent(an, hint.grp, hint.callAt, depth + 1));
        } finally {
            hint.busy = false;
        }
    }
    if (hint.exc) {
        const ent = resolveName(an, hint.exc, offset, depth + 1);
        return instanceOf(ent) || { t: 'exception', name: hint.exc };
    }
    if (hint.call) {
        const parts = hint.call.split('.');
        let ent;
        if (parts.length === 1) ent = resolveName(an, parts[0], offset, depth + 1);
        else ent = memberEntity(typeOfEntity(resolveName(an, parts[0], offset, depth + 1), depth + 1), parts[1], depth + 1);
        return instanceOf(ent);
    }
    if (hint.alias) {
        const sym = an.lookup(hint.alias, offset);
        if (sym && sym.hint === hint) return null;
        return typeOfEntity(resolveName(an, hint.alias, offset, depth + 1), depth + 1);
    }
    return null;
}

/** The type of the value you get by calling `ent`. */
function instanceOf(ent) {
    if (!ent) return null;
    if (ent.kind === 'symbol' && ent.sym.kind === 'class') return { t: 'instance', an: ent.an, sym: ent.sym };
    if (ent.kind === 'exception') return { t: 'exception', name: ent.name };
    if (ent.kind === 'builtinFn') {
        const r = BI.FUNCTIONS[ent.name].returns;
        return r ? { t: 'builtin', name: r } : null;
    }
    if (ent.kind === 'method') {
        const m = BI.METHODS[ent.type][ent.name];
        return m && m.returns ? { t: 'builtin', name: m.returns } : null;
    }
    if (ent.kind === 'moduleMember') {
        const m = BI.MODULES[ent.module].members[ent.name];
        return m && m.returns ? { t: 'builtin', name: m.returns } : null;
    }
    return null;
}

function classMembers(an, sym, seen = new Set(), depth = 0) {
    const out = new Map();
    if (!sym || seen.has(sym) || depth > 10) return out;
    seen.add(sym);
    if (sym.base) {
        const parts = sym.base.split('.');
        let baseEnt = resolveName(an, parts[0], sym.tok.s);
        if (parts.length > 1) baseEnt = memberEntity(typeOfEntity(baseEnt), parts[1]);
        if (baseEnt && baseEnt.kind === 'symbol' && baseEnt.sym.kind === 'class') {
            for (const [k, v] of classMembers(baseEnt.an, baseEnt.sym, seen, depth + 1)) out.set(k, v);
        } else if (baseEnt && baseEnt.kind === 'exception') {
            for (const k of Object.keys(BI.METHODS.exception)) out.set(k, { kind: 'method', type: 'exception', name: k });
        }
    }
    const cs = sym.members;
    if (cs) {
        for (const [k, f] of cs.fields) out.set(k, { kind: 'symbol', an, sym: f });
        for (const s of cs.order) out.set(s.name, { kind: 'symbol', an, sym: s });
    }
    return out;
}

/** Members of a type, as name -> entity. Null when the type is unknown. */
function members(type, depth = 0) {
    if (!type || depth > 10) return null;
    const out = new Map();
    switch (type.t) {
        case 'module':
            if (type.builtin) {
                for (const k of Object.keys(BI.MODULES[type.builtin].members)) {
                    out.set(k, { kind: 'moduleMember', module: type.builtin, name: k });
                }
            } else {
                for (const s of type.an.module.order) out.set(s.name, followSym(type.an, s, depth + 1));
            }
            return out;
        case 'class':
        case 'instance':
            return classMembers(type.an, type.sym);
        case 'builtin': {
            const ms = BI.METHODS[type.name];
            if (!ms) return out;
            for (const k of Object.keys(ms)) out.set(k, { kind: 'method', type: type.name, name: k });
            return out;
        }
        case 'exception':
        case 'exceptionClass':
            for (const k of Object.keys(BI.METHODS.exception)) out.set(k, { kind: 'method', type: 'exception', name: k });
            return out;
    }
    return null;
}

function memberEntity(type, name, depth = 0) {
    const m = members(type, depth);
    const found = m && m.get(name);
    if (found) return found;
    // a submodule of a package: `game.enemies`
    if (type && type.t === 'module' && type.name && type.importer && type.importer.getModule) {
        const full = type.name + '.' + name;
        if (type.importer.getModule(full)) return { kind: 'moduleRef', name: full, an: type.importer };
    }
    return null;
}

/** Type of the expression ending just before token grp[dotIdx] (a '.'). */
function typeBefore(an, grp, dotIdx, depth = 0) {
    if (depth > 10) return null;
    const p = grp[dotIdx - 1];
    if (!p) return null;
    if (p.t === 'str' || p.t === 'fstr') return { t: 'builtin', name: 'str' };
    if (p.t === 'id' || (p.t === 'kw' && false)) {
        return typeOfEntity(entityOfIdent(an, grp, dotIdx - 1, depth + 1), depth + 1);
    }
    if (p.t === 'op') {
        const M = grp.match;
        const o = M ? M[dotIdx - 1] : -1;
        if (o < 0) return null;
        const before = grp[o - 1];
        const literal = !before || (before.t === 'op' && !CLOSE[before.v]) || before.t === 'kw';
        if (p.v === ')') {
            if (before && before.t === 'id') return instanceOf(entityOfIdent(an, grp, o - 1, depth + 1));
            if (before && before.t === 'op' && before.v === ')') return null;
            return null;
        }
        if (p.v === ']') return literal ? { t: 'builtin', name: 'list' } : null;
        if (p.v === '}') return literal ? { t: 'builtin', name: 'dict' } : null;
    }
    return null;
}

/** What identifier token grp[idx] refers to (a member if it follows a '.'). */
function entityOfIdent(an, grp, idx, depth = 0) {
    const t = grp[idx];
    if (!t || t.t !== 'id') return null;
    if (t.importOf) {
        const bm = BI.MODULES[t.importOf.module];
        if (bm) return bm.members[t.v] ? { kind: 'moduleMember', module: t.importOf.module, name: t.v } : null;
        const mod = an.getModule && an.getModule(t.importOf.module);
        const target = mod && mod.module.get(t.v);
        return target ? followSym(mod, target) : null;
    }
    // a part of a module name in `from a.b import ...` / `import a.b as c`
    if (t.modPrefix) {
        if (BI.MODULES[t.modPrefix]) return { kind: 'moduleRef', name: t.modPrefix, an, builtin: true };
        return { kind: 'moduleRef', name: t.modPrefix, an };
    }
    const prev = grp[idx - 1];
    if (prev && prev.v === '.') return memberEntity(typeBefore(an, grp, idx - 1, depth + 1), t.v, depth + 1);
    // a module name in `import a, b;`
    const sym = an.lookup(t.v, t.s);
    if (sym && sym.kind === 'module' && sym.modTok === t) return { kind: 'symbol', an, sym };
    return resolveName(an, t.v, t.s, depth);
}

/** Signature info for something callable: { label, params: [{label, doc}], doc } or null. */
function callSignature(ent, viaInstance) {
    if (!ent) return null;
    const fmtParams = (ps) => ps.map((p) => (p.star || '') + p.name + (p.def != null ? ' = ' + p.def : ''));
    switch (ent.kind) {
        case 'builtinFn': {
            const f = BI.FUNCTIONS[ent.name];
            return { name: ent.name, params: f.params, doc: f.doc, returns: f.returns };
        }
        case 'moduleMember': {
            const m = BI.MODULES[ent.module].members[ent.name];
            if (m.value || m.module) return null;
            return { name: `${ent.module}.${ent.name}`, params: m.params, doc: m.doc };
        }
        case 'method': {
            const m = BI.METHODS[ent.type][ent.name];
            if (m.field) return null;
            return { name: ent.name, params: m.params, doc: m.doc, returns: m.returns };
        }
        case 'exception':
            return { name: ent.name, params: ['message=""'], doc: BI.EXCEPTIONS[ent.name].doc };
        case 'symbol': {
            const { an, sym } = ent;
            if (sym.kind === 'function' || sym.kind === 'method') {
                let ps = sym.params || [];
                if (sym.kind === 'method' && viaInstance) ps = ps.slice(1);
                return { name: sym.name, params: fmtParams(ps), doc: sym.doc };
            }
            if (sym.kind === 'class') {
                const init = classMembers(an, sym).get('init');
                if (init && init.kind === 'symbol') {
                    return { name: sym.name, params: fmtParams((init.sym.params || []).slice(1)), doc: sym.doc || init.sym.doc };
                }
                if (init && init.kind === 'method') return { name: sym.name, params: ['message=""'], doc: sym.doc };
                return { name: sym.name, params: [], doc: sym.doc };
            }
            if (sym.kind === 'parameter' || sym.kind === 'variable' || sym.kind === 'field') {
                return null;
            }
        }
    }
    return null;
}

/** For a call whose '(' is grp[parenIdx]: the callee entity and whether it's called through an instance. */
function calleeOf(an, grp, parenIdx) {
    const c = grp[parenIdx - 1];
    if (!c || c.t !== 'id') return null;
    const ent = entityOfIdent(an, grp, parenIdx - 1);
    if (!ent) return null;
    let viaInstance = false;
    if (grp[parenIdx - 2] && grp[parenIdx - 2].v === '.') {
        const recv = typeBefore(an, grp, parenIdx - 2);
        viaInstance = !!recv && recv.t === 'instance';
    }
    return { ent, viaInstance };
}

module.exports = {
    lex, matchBrackets, Analysis, Scope, resolveName, followSym, typeOfEntity, typeFromHint, instanceOf,
    classMembers, members, memberEntity, typeBefore, entityOfIdent, callSignature, calleeOf, OPEN, CLOSE,
};
