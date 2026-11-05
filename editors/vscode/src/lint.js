'use strict';
// Editor-only checks that the compiler doesn't do (it reports one syntax
// error and stops; name errors only show up at run time).

const BI = require('./builtins');

const SUGGEST = { true: 'True', false: 'False', null: 'None', none: 'None', nil: 'None', this: 'self', elseif: 'elif' };

/**
 * @param {import('./analyzer').Analysis} an
 * @param {{ undefinedNames?: boolean }} opts
 * @returns {{ s: number, e: number, message: string, severity: 'error'|'warning'|'info'|'hint', code: string, data?: any }[]}
 */
function lint(an, opts = {}) {
    const out = [];
    const T = an.tokens;

    // Python's __init__ is never called in Faston
    for (const sym of an.symbols) {
        if (sym.kind === 'method' && sym.name === '__init__') {
            out.push({
                s: sym.tok.s, e: sym.tok.e, severity: 'warning', code: 'faston.init',
                message: "Faston constructors are named 'init', not '__init__': this method won't run when the object is created.",
            });
        }
    }

    // super() works in methods of classes with a base class
    const superToks = new Set();
    for (let i = 0; i < T.length; i++) {
        const t = T[i];
        if (t.t === 'id' && t.v === 'super' && T[i + 1] && T[i + 1].v === '(' && !an.lookup('super', t.s)
            && (!T[i - 1] || T[i - 1].v !== '.')) {
            superToks.add(t);
            const cls = an.enclosingClass(t.s);
            if (cls && !cls.base) {
                out.push({
                    s: t.s, e: t.e, severity: 'error', code: 'faston.super',
                    message: `super() needs a base class: class ${cls.name}(Parent) { ... }`,
                });
            }
        }
    }

    // imports
    for (const imp of an.imports) {
        const bm = BI.MODULES[imp.module];
        if (bm) {
            for (const n of imp.names || []) {
                if (!bm.members[n.name]) {
                    out.push({ s: n.tok.s, e: n.tok.e, severity: 'error', code: 'faston.import', message: `Module '${imp.module}' has no '${n.name}'.` });
                }
            }
            continue;
        }
        if (!an.getModule) continue;
        const mod = an.getModule(imp.module);
        if (!mod) {
            out.push({
                s: imp.tok.s, e: imp.tok.e, severity: 'error', code: 'faston.import',
                message: `Module '${imp.module}' not found: Faston looks next to this file, in the project root and in .faston/packages (for ${imp.module.replace(/\./g, '/')}.fst or a ${imp.module.replace(/\./g, '/')}/ folder).`,
            });
            continue;
        }
        for (const n of imp.names || []) {
            if (!mod.module.get(n.name) && !an.getModule(imp.module + '.' + n.name)) {
                out.push({ s: n.tok.s, e: n.tok.e, severity: 'error', code: 'faston.import', message: `'${n.name}' is not defined at the top level of ${imp.module}.fst.` });
            }
        }
    }

    // undefined names
    if (opts.undefinedNames !== false) {
        for (const t of an.all) {
            if (t.t !== 'id' || t.skip || superToks.has(t)) continue;
            const g = t.grp;
            const prev = g[t.i - 1];
            const next = g[t.i + 1];
            if (prev && prev.v === '.') continue;
            if (next && next.v === '=' && prev && (prev.v === '(' || prev.v === ',')) continue; // keyword argument
            if (prev && prev.t === 'kw' && (prev.v === 'global' || prev.v === 'nonlocal')) continue;
            if (prev && prev.v === ',' && isGlobalList(g, t.i)) continue;
            if (an.lookup(t.v, t.s)) continue;
            if (BI.FUNCTIONS[t.v] || BI.EXCEPTIONS[t.v]) continue;
            const hint = SUGGEST[t.v];
            const isModule = !!BI.MODULES[t.v] || !!(an.getModule && an.getModule(t.v));
            let message = `'${t.v}' is not defined.`;
            if (hint) message += ` Did you mean '${hint}'?`;
            else if (isModule) message += ` Add 'import ${t.v};'.`;
            out.push({ s: t.s, e: t.e, severity: 'warning', code: 'faston.undefined', message, data: { name: t.v, hint, isModule } });
        }
    }
    return out;
}

function isGlobalList(g, i) {
    for (let k = i - 1; k >= 0; k--) {
        const v = g[k];
        if (v.t === 'kw' && (v.v === 'global' || v.v === 'nonlocal')) return true;
        if (!(v.t === 'id' || v.v === ',')) return false;
    }
    return false;
}

module.exports = { lint };
