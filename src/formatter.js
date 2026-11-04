'use strict';
// Re-indents Faston code from its bracket structure. It only touches
// leading/trailing whitespace, never code, and leaves multi-line string
// contents alone, so it can't change what a program does.

const { lex, OPEN, CLOSE } = require('./analyzer');

/**
 * @param {string} text
 * @param {{ tabSize: number, insertSpaces: boolean }} options
 * @returns {{ line: number, text: string }[]} changed lines only
 */
function formatLines(text, options, startLine = 0, endLine = Infinity) {
    const unit = options.insertSpaces ? ' '.repeat(options.tabSize || 4) : '\t';
    const lines = text.split('\n');
    const lineStarts = [];
    let off = 0;
    for (const l of lines) { lineStarts.push(off); off += l.length + 1; }
    const lineOf = (o) => {
        let lo = 0, hi = lineStarts.length - 1;
        while (lo < hi) {
            const mid = (lo + hi + 1) >> 1;
            if (lineStarts[mid] <= o) lo = mid; else hi = mid - 1;
        }
        return lo;
    };

    const { tokens } = lex(text);
    const lineTokens = lines.map(() => []);
    const startsInString = new Array(lines.length).fill(false);
    const endsInString = new Array(lines.length).fill(false);
    for (const t of tokens) {
        const l = lineOf(t.s);
        lineTokens[l].push(t);
        if (t.t === 'str' || t.t === 'fstr') {
            const le = lineOf(Math.max(t.s, t.e - 1));
            for (let k = l + 1; k <= le; k++) startsInString[k] = true;
            for (let k = l; k < le; k++) endsInString[k] = true;
        }
    }

    const changes = [];
    const stack = []; // line numbers of unclosed brackets
    const distinct = (arr) => {
        let n = 0;
        for (let k = 0; k < arr.length; k++) if (k === 0 || arr[k] !== arr[k - 1]) n++;
        return n;
    };
    for (let L = 0; L < lines.length; L++) {
        const toks = lineTokens[L];
        let leadingClosers = 0;
        while (leadingClosers < toks.length && toks[leadingClosers].t === 'op' && CLOSE[toks[leadingClosers].v]) leadingClosers++;
        const level = distinct(stack.slice(0, Math.max(0, stack.length - leadingClosers)));
        for (const t of toks) {
            if (t.t !== 'op') continue;
            if (OPEN[t.v]) stack.push(L);
            else if (CLOSE[t.v] && stack.length) stack.pop();
        }
        if (L < startLine || L > endLine || startsInString[L]) continue;

        const raw = lines[L];
        const cr = raw.endsWith('\r') ? '\r' : '';
        const body = cr ? raw.slice(0, -1) : raw;
        let content = endsInString[L] ? body.replace(/^\s+/, '') : body.trim();
        const out = content === '' ? '' : unit.repeat(level) + content;
        if (out !== body) changes.push({ line: L, text: out });
    }
    return changes;
}

module.exports = { formatLines };
