'use strict';
// Faston for VS Code: wires the analyzer, the real compiler and the editor together.

const vscode = require('vscode');
const cp = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');

const A = require('./analyzer');
const BI = require('./builtins');
const { lint } = require('./lint');
const { formatLines } = require('./formatter');

const LANG = 'faston';
const SELECTOR = [{ language: LANG, scheme: 'file' }, { language: LANG, scheme: 'untitled' }];
const IS_WIN = process.platform === 'win32';
const EXE_NAME = IS_WIN ? 'faston.exe' : 'faston';

let output; // OutputChannel

// =================================================================== project
// Analyses of open documents and of files on disk (for imports), cached by
// version / mtime so that cross-file lookups see the same objects.

class Project {
    constructor() {
        this.cache = new Map();
    }
    key(uri) {
        if (uri.scheme !== 'file') return uri.toString();
        const p = path.normalize(uri.fsPath);
        return IS_WIN ? p.toLowerCase() : p;
    }
    forDocument(doc) {
        const k = this.key(doc.uri);
        const stamp = 'v' + doc.version;
        const c = this.cache.get(k);
        if (c && c.stamp === stamp) return c.an;
        const an = new A.Analysis(doc.getText(), doc.uri);
        this.attach(an);
        this.cache.set(k, { stamp, an });
        return an;
    }
    forPath(p) {
        const uri = vscode.Uri.file(p);
        const k = this.key(uri);
        const doc = vscode.workspace.textDocuments.find((d) => d.uri.scheme === 'file' && this.key(d.uri) === k);
        if (doc) return this.forDocument(doc);
        let st;
        try { st = fs.statSync(p); } catch { return null; }
        if (!st.isFile()) return null;
        const stamp = 'm' + st.mtimeMs + ':' + st.size;
        const c = this.cache.get(k);
        if (c && c.stamp === stamp) return c.an;
        let text;
        try { text = fs.readFileSync(p, 'utf8'); } catch { return null; }
        const an = new A.Analysis(text, uri);
        this.attach(an);
        this.cache.set(k, { stamp, an });
        return an;
    }
    forUri(uri) {
        const doc = vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri.toString());
        if (doc) return this.forDocument(doc);
        return uri.scheme === 'file' ? this.forPath(uri.fsPath) : null;
    }
    attach(an) {
        const memo = new Map();
        an.getModule = (name) => {
            if (!an.uri || an.uri.scheme !== 'file' || !/^[\p{L}_][\p{L}\p{N}_]*$/u.test(name)) return null;
            const now = Date.now();
            const m = memo.get(name);
            if (m && now - m.at < 1500) return m.an;
            const p = path.join(path.dirname(an.uri.fsPath), name + '.fst');
            const r = this.key(vscode.Uri.file(p)) === this.key(an.uri) ? an : this.forPath(p);
            memo.set(name, { at: now, an: r });
            return r;
        };
    }
    async workspaceFiles(limit = 3000) {
        const uris = await vscode.workspace.findFiles('**/*.fst', '{**/target/**,**/node_modules/**,**/.git/**}', limit);
        const out = [];
        const seen = new Set();
        for (const u of uris) {
            const an = this.forUri(u);
            if (an && !seen.has(an)) { seen.add(an); out.push(an); }
        }
        for (const d of vscode.workspace.textDocuments) {
            if (d.languageId !== LANG) continue;
            const an = this.forDocument(d);
            if (!seen.has(an)) { seen.add(an); out.push(an); }
        }
        return out;
    }
}

const project = new Project();

// =================================================================== helpers

function toPos(an, offset) {
    const p = an.posOf(offset);
    return new vscode.Position(p.line, p.character);
}
function toRange(an, s, e) {
    return new vscode.Range(toPos(an, s), toPos(an, e));
}
function tokRange(an, t) {
    return toRange(an, t.s, t.e);
}
function location(an, s, e) {
    return new vscode.Location(an.uri, toRange(an, s, e));
}
function fileLabel(an) {
    return an.uri ? path.basename(an.uri.fsPath || an.uri.path) : '';
}
function moduleFile(an, name) {
    if (!an.uri || an.uri.scheme !== 'file') return null;
    const p = path.join(path.dirname(an.uri.fsPath), name + '.fst');
    return fs.existsSync(p) ? p : null;
}
function siblingModules(an) {
    if (!an.uri || an.uri.scheme !== 'file') return [];
    try {
        const self = path.basename(an.uri.fsPath);
        return fs.readdirSync(path.dirname(an.uri.fsPath))
            .filter((f) => f.endsWith('.fst') && f !== self && /^[\p{L}_][\p{L}\p{N}_]*\.fst$/u.test(f))
            .map((f) => f.slice(0, -4));
    } catch {
        return [];
    }
}
function lastTokenBefore(grp, offset) {
    let lo = 0, hi = grp.length - 1, r = -1;
    while (lo <= hi) {
        const mid = (lo + hi) >> 1;
        if (grp[mid].e <= offset) { r = mid; lo = mid + 1; } else hi = mid - 1;
    }
    return r;
}
function typeName(t) {
    if (!t) return null;
    switch (t.t) {
        case 'instance': return t.sym.name;
        case 'class': return `type[${t.sym.name}]`;
        case 'builtin': return t.name;
        case 'module': return 'module';
        case 'exception': return t.name || 'Exception';
        case 'exceptionClass': return `type[${t.name}]`;
        case 'function': return 'function';
    }
    return null;
}
function codeBlock(s) {
    return '```faston\n' + s + '\n```';
}

// =================================================================== describing entities

function symSignature(sym) {
    const ps = (sym.params || []).map((p) => (p.star || '') + p.name + (p.def != null ? ' = ' + p.def : ''));
    const owner = sym.kind === 'method' && sym.classSym ? sym.classSym.name + '.' : '';
    return `def ${owner}${sym.name}(${ps.join(', ')})`;
}

function classHeader(an, sym) {
    let s = `class ${sym.name}${sym.base ? '(' + sym.base + ')' : ''}`;
    const init = A.classMembers(an, sym).get('init');
    if (init && init.kind === 'symbol' && init.sym.params) {
        s += `\n# ${sym.name}(${init.sym.params.slice(1).map((p) => (p.star || '') + p.name + (p.def != null ? ' = ' + p.def : '')).join(', ')})`;
    }
    return s;
}

function declarationLine(an, sym) {
    const line = an.lineText(an.lineOf(sym.tok.s)).trim();
    return line.length > 160 ? line.slice(0, 157) + '...' : line;
}

/** Markdown for hover / completion docs. */
function describe(ent, fromAn) {
    if (!ent) return null;
    const md = new vscode.MarkdownString();
    md.supportHtml = false;
    const where = (an) => (fromAn && an !== fromAn ? `\n\n*${fileLabel(an)}*` : '');
    switch (ent.kind) {
        case 'builtinFn': {
            const f = BI.FUNCTIONS[ent.name];
            md.appendMarkdown(codeBlock(BI.signature(ent.name, f)) + '\n\n' + f.doc + '\n\n*built-in function*');
            return md;
        }
        case 'exception': {
            const e = BI.EXCEPTIONS[ent.name];
            md.appendMarkdown(codeBlock(`class ${ent.name}${e.base ? '(' + e.base + ')' : ''}`) + '\n\n' + e.doc + '\n\n*built-in exception*');
            return md;
        }
        case 'moduleMember': {
            const m = BI.MODULES[ent.module].members[ent.name];
            md.appendMarkdown(codeBlock(m.value ? `${ent.module}.${ent.name}: float` : BI.signature(`${ent.module}.${ent.name}`, m)) + '\n\n' + m.doc);
            return md;
        }
        case 'method': {
            const m = BI.METHODS[ent.type][ent.name];
            if (m.field) md.appendMarkdown(codeBlock(`(field) ${ent.name}: str`) + '\n\n' + m.doc);
            else md.appendMarkdown(codeBlock(BI.signature(`${ent.type}.${ent.name}`, m)) + '\n\n' + m.doc);
            return md;
        }
        case 'moduleRef': {
            return describeModule(md, ent.an, ent.name);
        }
        case 'symbol': {
            const { an, sym } = ent;
            const doc = sym.doc ? '\n\n' + sym.doc : '';
            switch (sym.kind) {
                case 'class':
                    md.appendMarkdown(codeBlock(classHeader(an, sym)) + doc + where(an));
                    return md;
                case 'function':
                case 'method':
                    md.appendMarkdown(codeBlock(symSignature(sym)) + doc + where(an));
                    return md;
                case 'module':
                    return describeModule(md, an, sym.module);
                case 'parameter': {
                    const ty = typeName(A.typeOfEntity(ent));
                    md.appendMarkdown(codeBlock(`(parameter) ${sym.star || ''}${sym.name}${ty ? ': ' + ty : ''}${sym.def != null ? ' = ' + sym.def : ''}`));
                    return md;
                }
                case 'field': {
                    const ty = typeName(A.typeOfEntity(ent));
                    const owner = sym.classSym ? sym.classSym.name + '.' : '';
                    md.appendMarkdown(codeBlock(`(field) ${owner}${sym.name}${ty ? ': ' + ty : ''}`) + '\n' + codeBlock(declarationLine(an, sym)) + doc + where(an));
                    return md;
                }
                case 'variable':
                case 'constant': {
                    const ty = typeName(A.typeOfEntity(ent));
                    const label = sym.kind === 'constant' ? 'constant' : sym.scope && sym.scope.kind === 'module' ? 'global' : 'local';
                    md.appendMarkdown(codeBlock(`(${label}) ${sym.name}${ty ? ': ' + ty : ''}`) + '\n' + codeBlock(declarationLine(an, sym)) + doc + where(an));
                    return md;
                }
                case 'import':
                    md.appendMarkdown(codeBlock(`(import) from ${sym.module} import ${sym.imported}`) + `\n\n'${sym.imported}' was not found in ${sym.module}.`);
                    return md;
            }
        }
    }
    return null;
}

function describeModule(md, an, name) {
    if (BI.MODULES[name]) {
        const m = BI.MODULES[name];
        md.appendMarkdown(codeBlock(`(module) ${name}`) + '\n\n' + m.doc + '\n\n' +
            Object.keys(m.members).map((k) => '`' + k + '`').join(' '));
        return md;
    }
    const mod = an.getModule && an.getModule(name);
    md.appendMarkdown(codeBlock(`(module) ${name}`));
    if (mod) {
        const head = [];
        for (let l = 0; l < mod.lineStarts.length; l++) {
            const c = mod.commentByLine.get(l);
            if (!c || !c.full) break;
            head.push(c.c.v.replace(/^#\s?/, ''));
        }
        if (head.length) md.appendMarkdown('\n\n' + head.join('\n'));
        const top = mod.module.order.filter((s) => s.kind === 'class' || s.kind === 'function').map((s) => '`' + s.name + '`');
        if (top.length) md.appendMarkdown('\n\n' + top.join(' '));
        md.appendMarkdown(`\n\n*${fileLabel(mod)}*`);
    } else {
        md.appendMarkdown(`\n\nNot found: ${name}.fst should be next to this file.`);
    }
    return md;
}

// =================================================================== completion

const CK = vscode.CompletionItemKind;
function completionKind(ent) {
    if (!ent) return CK.Text;
    switch (ent.kind) {
        case 'builtinFn': return CK.Function;
        case 'exception': return CK.Class;
        case 'moduleMember': return BI.MODULES[ent.module].members[ent.name].value ? CK.Constant : CK.Function;
        case 'method': return BI.METHODS[ent.type][ent.name].field ? CK.Field : CK.Method;
        case 'symbol':
            switch (ent.sym.kind) {
                case 'class': return CK.Class;
                case 'function': return CK.Function;
                case 'method': return CK.Method;
                case 'field': return CK.Field;
                case 'constant': return CK.Constant;
                case 'module': return CK.Module;
                case 'parameter': return CK.Variable;
                default: return CK.Variable;
            }
    }
    return CK.Text;
}
function isCallable(ent) {
    if (!ent) return false;
    if (ent.kind === 'builtinFn' || ent.kind === 'exception') return true;
    if (ent.kind === 'moduleMember') return !BI.MODULES[ent.module].members[ent.name].value;
    if (ent.kind === 'method') return !BI.METHODS[ent.type][ent.name].field;
    if (ent.kind === 'symbol') return ['class', 'function', 'method'].includes(ent.sym.kind);
    return false;
}
function detailOf(ent) {
    if (!ent) return '';
    switch (ent.kind) {
        case 'builtinFn': return BI.signature(ent.name, BI.FUNCTIONS[ent.name]);
        case 'exception': return 'built-in exception';
        case 'moduleMember': {
            const m = BI.MODULES[ent.module].members[ent.name];
            return m.value ? `${ent.module}.${ent.name}: float` : BI.signature(ent.name, m);
        }
        case 'method': {
            const m = BI.METHODS[ent.type][ent.name];
            return m.field ? `${ent.name}: str` : BI.signature(ent.name, m);
        }
        case 'symbol': {
            const s = ent.sym;
            if (s.kind === 'function' || s.kind === 'method') return symSignature(s);
            if (s.kind === 'class') return `class ${s.name}${s.base ? '(' + s.base + ')' : ''}`;
            if (s.kind === 'module') return `module ${s.module}`;
            const ty = typeName(A.typeOfEntity(ent));
            return `${s.kind}${ty ? ': ' + ty : ''}`;
        }
    }
    return '';
}

function makeItem(name, ent, sort, an, opts = {}) {
    const item = new vscode.CompletionItem(name, opts.kind !== undefined ? opts.kind : completionKind(ent));
    item.sortText = sort + name;
    item.detail = opts.detail !== undefined ? opts.detail : detailOf(ent);
    const doc = describe(ent, an);
    if (doc) item.documentation = doc;
    if (opts.parens && isCallable(ent)) {
        item.insertText = new vscode.SnippetString(name.replace(/\$/g, '\\$') + '($0)');
        item.command = { command: 'editor.action.triggerParameterHints', title: 'Parameter hints' };
    }
    return item;
}

function importInsertEdit(an, doc, name) {
    let line = 0;
    for (const imp of an.imports) line = Math.max(line, an.lineOf(imp.tok.s) + 1);
    if (line === 0) {
        // after a leading comment block
        while (line < doc.lineCount && /^\s*#/.test(doc.lineAt(line).text)) line++;
    }
    return vscode.TextEdit.insert(new vscode.Position(line, 0), `import ${name};\n`);
}

const completionProvider = {
    provideCompletionItems(doc, pos) {
        const an = project.forDocument(doc);
        const off = doc.offsetAt(pos);
        if (an.inCommentOrString(off)) return undefined;
        const grp = an.groupAt(off);
        let k = lastTokenBefore(grp, off);
        if (k >= 0 && (grp[k].t === 'id' || grp[k].t === 'kw') && grp[k].e === off) k--;
        const prev = grp[k];
        const nextChar = doc.getText(new vscode.Range(pos, pos.translate(0, 1)));
        const wantParens = nextChar !== '(';

        // ---- member access: x.
        if (prev && prev.v === '.' && grp[k - 1] && grp[k - 1].t !== 'num') {
            const type = A.typeBefore(an, grp, k);
            const ms = A.members(type);
            const items = [];
            if (ms) {
                for (const [name, ent] of ms) {
                    if (type.t === 'instance' && ent.kind === 'symbol' && ent.sym.name === 'init') continue;
                    items.push(makeItem(name, ent, name.startsWith('_') ? '9' : '0', an, { parens: wantParens }));
                }
                return items;
            }
            // unknown receiver: every method we know of
            const seen = new Set();
            for (const [type, ms2] of Object.entries(BI.METHODS)) {
                for (const name of Object.keys(ms2)) {
                    if (seen.has(name)) continue;
                    seen.add(name);
                    const it = makeItem(name, { kind: 'method', type, name }, '1', an, { parens: wantParens });
                    it.detail = `(${Object.keys(BI.METHODS).filter((t) => BI.METHODS[t][name]).join(', ')}) ` + it.detail;
                    items.push(it);
                }
            }
            for (const sym of an.symbols) {
                if ((sym.kind === 'method' || sym.kind === 'field') && sym.name !== 'init' && !seen.has(sym.name)) {
                    seen.add(sym.name);
                    items.push(makeItem(sym.name, { kind: 'symbol', an, sym }, '0', an, { parens: wantParens }));
                }
            }
            return items;
        }

        // ---- statement context
        let st = k;
        while (st >= 0 && ![';', '{', '}'].includes(grp[st].v)) st--;
        st++;
        const first = grp[st];
        if (first && first.t === 'kw' && first.v === 'import' && grp === an.tokens) {
            return moduleItems(an);
        }
        if (first && first.t === 'kw' && first.v === 'from' && grp === an.tokens) {
            const importIdx = grp.slice(st, k + 1).findIndex((t) => t.t === 'kw' && t.v === 'import');
            if (importIdx < 0) return k === st ? moduleItems(an) : undefined;
            const modName = grp[st + 1] && grp[st + 1].v;
            const type = BI.MODULES[modName] ? { t: 'module', builtin: modName } : (() => {
                const m = an.getModule && an.getModule(modName);
                return m ? { t: 'module', an: m } : null;
            })();
            const ms = A.members(type);
            return ms ? [...ms].filter(([, e]) => !(e.kind === 'symbol' && e.sym.kind === 'module')).map(([n, e]) => makeItem(n, e, '0', an)) : undefined;
        }
        if (prev && prev.t === 'kw' && prev.v === 'def') {
            const sc = an.scopeAt(off);
            if (sc.kind !== 'class') return [];
            return Object.entries(BI.DUNDERS).map(([name, d]) => {
                const it = new vscode.CompletionItem(name, CK.Method);
                it.detail = `def ${name}(${d.params.join(', ')})`;
                it.documentation = new vscode.MarkdownString(d.doc);
                const ps = d.params.map((p, i) => (i === 0 ? p : '${' + i + ':' + p.replace(/[$}\\]/g, '') + '}')).join(', ');
                it.insertText = new vscode.SnippetString(`${name}(${ps}) {\n\t$0\n}`);
                it.sortText = (name === 'init' ? '0' : '1') + name;
                return it;
            });
        }
        if (prev && prev.t === 'kw' && ['class', 'let', 'const', 'as', 'global', 'nonlocal'].includes(prev.v)) {
            if (prev.v === 'global') {
                return an.module.order.filter((s) => s.kind === 'variable' || s.kind === 'constant')
                    .map((s) => makeItem(s.name, { kind: 'symbol', an, sym: s }, '0', an));
            }
            return [];
        }

        // ---- names
        const items = [];
        const seen = new Set();
        const vis = an.visible(off);
        for (const [name, sym] of vis) {
            if (seen.has(name)) continue;
            seen.add(name);
            const ent = A.followSym(an, sym);
            items.push(makeItem(name, ent, sym.scope.kind === 'module' ? '1' : '0', an, { parens: wantParens }));
        }
        for (const name of Object.keys(BI.FUNCTIONS)) {
            if (seen.has(name)) continue;
            seen.add(name);
            items.push(makeItem(name, { kind: 'builtinFn', name }, '2', an, { parens: wantParens }));
        }
        for (const name of Object.keys(BI.EXCEPTIONS)) {
            if (seen.has(name)) continue;
            seen.add(name);
            items.push(makeItem(name, { kind: 'exception', name }, '3', an, { parens: wantParens }));
        }
        for (const kw of BI.KEYWORDS) {
            if (seen.has(kw)) continue;
            const it = new vscode.CompletionItem(kw, CK.Keyword);
            it.sortText = '4' + kw;
            if (BI.KEYWORD_DOCS[kw]) it.documentation = new vscode.MarkdownString(BI.KEYWORD_DOCS[kw]);
            items.push(it);
        }
        // modules that aren't imported yet: completing them adds the import
        for (const name of Object.keys(BI.MODULES).concat(siblingModules(an))) {
            if (seen.has(name)) continue;
            seen.add(name);
            const it = new vscode.CompletionItem(name, CK.Module);
            it.sortText = '5' + name;
            it.detail = `import ${name}`;
            it.documentation = new vscode.MarkdownString(`Adds \`import ${name};\` to this file.`);
            it.additionalTextEdits = [importInsertEdit(an, doc, name)];
            items.push(it);
        }
        return items;
    },
};

function moduleItems(an) {
    const items = [];
    for (const [name, m] of Object.entries(BI.MODULES)) {
        const it = new vscode.CompletionItem(name, CK.Module);
        it.detail = 'built-in module';
        it.documentation = new vscode.MarkdownString(m.doc);
        it.sortText = '1' + name;
        items.push(it);
    }
    for (const name of siblingModules(an)) {
        const it = new vscode.CompletionItem(name, CK.Module);
        it.detail = name + '.fst';
        it.sortText = '0' + name;
        items.push(it);
    }
    return items;
}

// =================================================================== hover / definition

function entityAt(an, offset) {
    const tok = an.tokenAt(offset);
    if (!tok) return {};
    if (tok.t === 'kw') return { tok, keyword: tok.v };
    if (tok.t !== 'id') return { tok };
    return { tok, ent: A.entityOfIdent(an, tok.grp, tok.i) };
}

const hoverProvider = {
    provideHover(doc, pos) {
        const an = project.forDocument(doc);
        const off = doc.offsetAt(pos);
        if (an.inCommentOrString(off)) return undefined;
        const { tok, ent, keyword } = entityAt(an, off);
        if (!tok) return undefined;
        if (keyword) {
            const d = BI.KEYWORD_DOCS[keyword];
            return d ? new vscode.Hover(new vscode.MarkdownString(d), tokRange(an, tok)) : undefined;
        }
        const md = describe(ent, an);
        return md ? new vscode.Hover(md, tokRange(an, tok)) : undefined;
    },
};

function definitionOf(ent) {
    if (!ent) return null;
    if (ent.kind === 'moduleRef') {
        const p = moduleFile(ent.an, ent.name);
        return p ? new vscode.Location(vscode.Uri.file(p), new vscode.Position(0, 0)) : null;
    }
    if (ent.kind !== 'symbol') return null;
    const { an, sym } = ent;
    if (sym.kind === 'module') {
        const p = moduleFile(an, sym.module);
        return p ? new vscode.Location(vscode.Uri.file(p), new vscode.Position(0, 0)) : null;
    }
    if (!an.uri) return null;
    return location(an, sym.tok.s, sym.tok.e);
}

const definitionProvider = {
    provideDefinition(doc, pos) {
        const an = project.forDocument(doc);
        const { tok, ent } = entityAt(an, doc.offsetAt(pos));
        if (!tok || !ent) return undefined;
        const loc = definitionOf(ent);
        if (!loc) return undefined;
        return [{
            originSelectionRange: tokRange(an, tok),
            targetUri: loc.uri,
            targetRange: ent.kind === 'symbol' && ent.sym.fullStart !== undefined
                ? toRange(ent.an, ent.sym.fullStart, ent.sym.fullEnd) : loc.range,
            targetSelectionRange: loc.range,
        }];
    },
};

const typeDefinitionProvider = {
    provideTypeDefinition(doc, pos) {
        const an = project.forDocument(doc);
        const { ent } = entityAt(an, doc.offsetAt(pos));
        const t = A.typeOfEntity(ent);
        if (t && (t.t === 'instance' || t.t === 'class')) return location(t.an, t.sym.tok.s, t.sym.tok.e);
        return undefined;
    },
};

// =================================================================== references / rename / highlights

/** Every identifier token (in `ans`) that refers to `target` ({ an, sym }). */
function referencesTo(target, ans) {
    const { sym } = target;
    const isMember = sym.kind === 'method' || sym.kind === 'field';
    let uniqueMember = false;
    if (isMember) {
        let n = 0;
        for (const an of ans) for (const s of an.symbols) if ((s.kind === 'method' || s.kind === 'field') && s.name === sym.name) n++;
        uniqueMember = n <= 1;
    }
    const out = [];
    for (const an of ans) {
        for (const t of an.all) {
            if (t.t !== 'id' || t.v !== sym.name) continue;
            const e = A.entityOfIdent(an, t.grp, t.i);
            if (e && e.kind === 'symbol' && e.sym === sym) out.push({ an, t });
            else if (!e && isMember && uniqueMember && t.grp[t.i - 1] && t.grp[t.i - 1].v === '.') out.push({ an, t });
        }
    }
    return out;
}

async function filesFor(target, an) {
    const { sym } = target;
    const local = sym.scope && (sym.scope.kind === 'function' || sym.scope.kind === 'lambda') && sym.kind !== 'field';
    if (local) return [target.an];
    const files = await project.workspaceFiles();
    if (!files.includes(target.an)) files.push(target.an);
    if (!files.includes(an)) files.push(an);
    return files;
}

function targetAt(an, offset) {
    const { tok, ent } = entityAt(an, offset);
    if (!tok || !ent || ent.kind !== 'symbol' || !ent.an.uri) return null;
    return { tok, target: ent };
}

const referenceProvider = {
    async provideReferences(doc, pos, ctx) {
        const an = project.forDocument(doc);
        const r = targetAt(an, doc.offsetAt(pos));
        if (!r) return undefined;
        const refs = referencesTo(r.target, await filesFor(r.target, an));
        return refs
            .filter(({ t }) => ctx.includeDeclaration || t !== r.target.sym.tok)
            .map(({ an: a, t }) => location(a, t.s, t.e));
    },
};

const highlightProvider = {
    provideDocumentHighlights(doc, pos) {
        const an = project.forDocument(doc);
        const r = targetAt(an, doc.offsetAt(pos));
        if (!r) return undefined;
        return referencesTo(r.target, [an]).filter(({ an: a }) => a === an).map(({ t }) => {
            const next = t.grp[t.i + 1];
            const write = t === r.target.sym.tok || (next && /^(=|\+=|-=|\*=|\/=|%=|&=|\|=|\^=)$/.test(next.v));
            return new vscode.DocumentHighlight(tokRange(an, t), write ? vscode.DocumentHighlightKind.Write : vscode.DocumentHighlightKind.Read);
        });
    },
};

const renameProvider = {
    prepareRename(doc, pos) {
        const an = project.forDocument(doc);
        const off = doc.offsetAt(pos);
        const { tok, ent } = entityAt(an, off);
        if (!tok || tok.t !== 'id') throw new Error('Place the cursor on a name to rename it.');
        if (!ent || ent.kind !== 'symbol') throw new Error(`'${tok.v}' is built in or unknown, so it can't be renamed.`);
        if (ent.sym.kind === 'module') throw new Error('To rename a module, rename its .fst file.');
        if (ent.sym.name === 'init' && ent.sym.kind === 'method') throw new Error("'init' is the constructor; renaming it would break the class.");
        if (/^__\w+__$/.test(ent.sym.name)) throw new Error('Operator methods have fixed names.');
        return tokRange(an, tok);
    },
    async provideRenameEdits(doc, pos, newName) {
        if (!/^[\p{L}_][\p{L}\p{N}_]*$/u.test(newName) || BI.KEYWORDS.includes(newName)) {
            throw new Error(`'${newName}' is not a valid Faston name.`);
        }
        const an = project.forDocument(doc);
        const r = targetAt(an, doc.offsetAt(pos));
        if (!r) return undefined;
        const edit = new vscode.WorkspaceEdit();
        for (const { an: a, t } of referencesTo(r.target, await filesFor(r.target, an))) {
            edit.replace(a.uri, tokRange(a, t), newName);
        }
        return edit;
    },
};

// =================================================================== symbols

const SK = vscode.SymbolKind;
function symbolKind(sym) {
    switch (sym.kind) {
        case 'class': return SK.Class;
        case 'function': return SK.Function;
        case 'method': return sym.name === 'init' ? SK.Constructor : /^__\w+__$/.test(sym.name) ? SK.Operator : SK.Method;
        case 'field': return SK.Field;
        case 'constant': return SK.Constant;
        case 'module': return SK.Module;
        default: return SK.Variable;
    }
}

function documentSymbols(an) {
    const full = (sym) => {
        if (sym.fullStart !== undefined) return toRange(an, sym.fullStart, sym.fullEnd);
        const line = an.lineOf(sym.tok.s);
        return new vscode.Range(line, 0, line, an.lineText(line).length);
    };
    const make = (sym, detail) => {
        const sel = tokRange(an, sym.tok);
        let range = full(sym);
        if (!range.contains(sel)) range = sel;
        return new vscode.DocumentSymbol(sym.name, detail || '', symbolKind(sym), range, sel);
    };
    const children = (scope) => {
        const out = [];
        if (!scope) return out;
        if (scope.kind === 'class') {
            for (const f of scope.fields.values()) out.push(make(f, typeName(A.typeOfEntity({ kind: 'symbol', an, sym: f })) || ''));
        }
        for (const s of scope.order) {
            if (s.kind === 'function' || s.kind === 'method') {
                const d = make(s, '(' + (s.params || []).map((p) => (p.star || '') + p.name).join(', ') + ')');
                d.children = children(s.inner).filter((c) => c.kind !== SK.Variable);
                out.push(d);
            } else if (s.kind === 'class') {
                const d = make(s, s.base ? '(' + s.base + ')' : '');
                d.children = children(s.members);
                out.push(d);
            } else if (scope.kind === 'module' && (s.kind === 'variable' || s.kind === 'constant') && !s.comp && !s.loopVar) {
                out.push(make(s, typeName(A.typeOfEntity({ kind: 'symbol', an, sym: s })) || ''));
            } else if (scope.kind === 'class' && s.kind === 'field') {
                out.push(make(s, ''));
            }
        }
        return out.sort((a, b) => a.range.start.compareTo(b.range.start));
    };
    return children(an.module);
}

const documentSymbolProvider = {
    provideDocumentSymbols(doc) {
        return documentSymbols(project.forDocument(doc));
    },
};

function fuzzy(query, name) {
    if (!query) return true;
    const q = query.toLowerCase(), n = name.toLowerCase();
    let i = 0;
    for (const c of n) if (c === q[i]) i++;
    return i === q.length;
}

const workspaceSymbolProvider = {
    async provideWorkspaceSymbols(query) {
        const out = [];
        for (const an of await project.workspaceFiles()) {
            for (const s of an.symbols) {
                const top = s.scope === an.module;
                const member = s.kind === 'method' || (s.kind === 'field' && s.classSym);
                if (!(top && s.kind !== 'import' && s.kind !== 'module' && !s.comp && !s.loopVar) && !member) continue;
                if (!fuzzy(query, s.name)) continue;
                out.push(new vscode.SymbolInformation(s.name, symbolKind(s), s.classSym ? s.classSym.name : '', location(an, s.tok.s, s.tok.e)));
            }
        }
        return out;
    },
};

// =================================================================== signature help

const signatureProvider = {
    provideSignatureHelp(doc, pos) {
        const an = project.forDocument(doc);
        const off = doc.offsetAt(pos);
        if (an.inCommentOrString(off)) return undefined;
        const grp = an.groupAt(off);
        let k = lastTokenBefore(grp, off);
        let depth = 0, active = 0, paren = -1, argStart = k + 1;
        for (let j = k; j >= 0; j--) {
            const t = grp[j];
            if (t.t !== 'op') continue;
            if (A.CLOSE[t.v]) depth++;
            else if (A.OPEN[t.v]) {
                if (depth === 0) {
                    if (t.v === '(') { paren = j; break; }
                    return undefined;
                }
                depth--;
            } else if (depth === 0 && t.v === ',') {
                if (active === 0) argStart = j + 1;
                active++;
            } else if (depth === 0 && t.v === ';') return undefined;
        }
        if (paren < 0) return undefined;
        if (active === 0) argStart = paren + 1;
        const callee = A.calleeOf(an, grp, paren);
        if (!callee) return undefined;
        const sig = A.callSignature(callee.ent, callee.viaInstance);
        if (!sig) return undefined;

        let label = sig.name + '(';
        const params = [];
        sig.params.forEach((p, i) => {
            if (i) label += ', ';
            const s = label.length;
            label += p;
            params.push(new vscode.ParameterInformation([s, label.length]));
        });
        label += ')' + (sig.returns ? ' -> ' + sig.returns : '');
        const info = new vscode.SignatureInformation(label, sig.doc ? new vscode.MarkdownString(sig.doc) : undefined);
        info.parameters = params;

        const bare = sig.params.map((p) => p.replace(/^\*+/, '').replace(/\s*=.*$/, ''));
        let index = active;
        const a = grp[argStart], b = grp[argStart + 1];
        if (a && a.t === 'id' && b && b.v === '=' && b.e <= off) {
            const kw = bare.indexOf(a.v);
            if (kw >= 0) index = kw;
            else {
                const kwargs = sig.params.findIndex((p) => p.startsWith('**'));
                if (kwargs >= 0) index = kwargs;
            }
        } else {
            const star = sig.params.findIndex((p) => p.startsWith('*') && !p.startsWith('**'));
            if (star >= 0 && index > star) index = star;
        }
        const help = new vscode.SignatureHelp();
        help.signatures = [info];
        help.activeSignature = 0;
        help.activeParameter = Math.min(index, Math.max(0, params.length - 1));
        info.activeParameter = help.activeParameter;
        return help;
    },
};

// =================================================================== semantic tokens

const LEGEND = new vscode.SemanticTokensLegend(
    ['namespace', 'class', 'function', 'method', 'parameter', 'variable', 'property'],
    ['declaration', 'readonly', 'defaultLibrary'],
);
const TT = Object.fromEntries(LEGEND.tokenTypes.map((t, i) => [t, i]));
const MOD = { declaration: 1, readonly: 2, defaultLibrary: 4 };

function classify(ent, t) {
    if (!ent) return null;
    switch (ent.kind) {
        case 'builtinFn': return [TT.function, MOD.defaultLibrary];
        case 'exception': return [TT.class, MOD.defaultLibrary];
        case 'moduleMember':
            return BI.MODULES[ent.module].members[ent.name].value ? [TT.variable, MOD.readonly | MOD.defaultLibrary] : [TT.function, MOD.defaultLibrary];
        case 'method': return BI.METHODS[ent.type][ent.name].field ? [TT.property, MOD.defaultLibrary] : [TT.method, MOD.defaultLibrary];
        case 'moduleRef': return [TT.namespace, 0];
        case 'symbol': {
            const s = ent.sym;
            const decl = s.tok === t ? MOD.declaration : 0;
            switch (s.kind) {
                case 'class': return [TT.class, decl];
                case 'function': return [TT.function, decl];
                case 'method': return [TT.method, decl];
                case 'parameter': return s.isSelf && t.v === 'self' ? null : [TT.parameter, decl];
                case 'field': return [TT.property, decl];
                case 'constant': return [TT.variable, decl | MOD.readonly];
                case 'module': return [TT.namespace, decl];
                case 'variable': return [TT.variable, decl];
                default: return [TT.variable, decl];
            }
        }
    }
    return null;
}

const semanticProvider = {
    provideDocumentSemanticTokens(doc) {
        const an = project.forDocument(doc);
        const b = new vscode.SemanticTokensBuilder(LEGEND);
        for (const t of an.all) {
            if (t.t !== 'id') continue;
            let c = classify(A.entityOfIdent(an, t.grp, t.i), t);
            if (!c) {
                const prev = t.grp[t.i - 1];
                if (prev && prev.v === '.') {
                    const next = t.grp[t.i + 1];
                    c = [next && next.v === '(' ? TT.method : TT.property, 0];
                }
            }
            if (!c) continue;
            const p = an.posOf(t.s);
            b.push(p.line, p.character, t.e - t.s, c[0], c[1]);
        }
        return b.build();
    },
};

// =================================================================== folding / formatting

const foldingProvider = {
    provideFoldingRanges(doc) {
        const an = project.forDocument(doc);
        const out = [];
        const M = an.tokens.match;
        an.tokens.forEach((t, i) => {
            if (t.t !== 'op' || !A.OPEN[t.v] || M[i] <= i) return;
            const s = an.lineOf(t.s), e = an.lineOf(an.tokens[M[i]].s);
            if (e > s) out.push(new vscode.FoldingRange(s, e - 1));
        });
        for (const t of an.tokens) {
            if (t.t === 'str' || t.t === 'fstr') {
                const s = an.lineOf(t.s), e = an.lineOf(t.e);
                if (e > s) out.push(new vscode.FoldingRange(s, e));
            }
        }
        // comment blocks and # region markers
        const regions = [];
        let blockStart = -1, last = -2;
        const lines = [...an.commentByLine.entries()].filter(([, c]) => c.full).sort((a, b) => a[0] - b[0]);
        for (const [line, c] of lines) {
            if (/^#\s*region\b/.test(c.c.v)) regions.push(line);
            else if (/^#\s*endregion\b/.test(c.c.v) && regions.length) out.push(new vscode.FoldingRange(regions.pop(), line, vscode.FoldingRangeKind.Region));
            if (line !== last + 1) {
                if (blockStart >= 0 && last > blockStart) out.push(new vscode.FoldingRange(blockStart, last, vscode.FoldingRangeKind.Comment));
                blockStart = line;
            }
            last = line;
        }
        if (blockStart >= 0 && last > blockStart) out.push(new vscode.FoldingRange(blockStart, last, vscode.FoldingRangeKind.Comment));
        if (an.imports.length > 1) {
            const s = an.lineOf(an.imports[0].tok.s), e = an.lineOf(an.imports[an.imports.length - 1].tok.s);
            if (e > s) out.push(new vscode.FoldingRange(s, e, vscode.FoldingRangeKind.Imports));
        }
        return out;
    },
};

function formatEdits(doc, options, range) {
    const text = doc.getText();
    const start = range ? range.start.line : 0;
    const end = range ? range.end.line : Infinity;
    const changes = formatLines(text, options, start, end);
    const edits = changes.map((c) => vscode.TextEdit.replace(doc.lineAt(c.line).range, c.text));
    if (!range) {
        // a single trailing newline
        const m = /(\r?\n)*$/.exec(text);
        const eol = doc.eol === vscode.EndOfLine.CRLF ? '\r\n' : '\n';
        if (text.length && (m[0] === '' || m[0].length > eol.length)) {
            const from = doc.positionAt(text.length - m[0].length);
            const lastLine = doc.lineAt(doc.lineCount - 1);
            if (m[0] === '') edits.push(vscode.TextEdit.insert(lastLine.range.end, eol));
            else edits.push(vscode.TextEdit.replace(new vscode.Range(from, lastLine.range.end), eol));
        }
    }
    return edits;
}

const formattingProvider = {
    provideDocumentFormattingEdits(doc, options) {
        return formatEdits(doc, options, null);
    },
    provideDocumentRangeFormattingEdits(doc, range, options) {
        return formatEdits(doc, options, range);
    },
};

const onTypeFormattingProvider = {
    provideOnTypeFormattingEdits(doc, pos, ch, options) {
        const line = pos.line;
        return formatEdits(doc, options, new vscode.Range(line, 0, line, 0));
    },
};

// =================================================================== inlay hints

const inlayProvider = {
    provideInlayHints(doc, range) {
        if (!vscode.workspace.getConfiguration('faston', doc.uri).get('inlayHints.parameterNames')) return [];
        const an = project.forDocument(doc);
        const out = [];
        const s = doc.offsetAt(range.start), e = doc.offsetAt(range.end);
        for (const grp of [an.tokens, ...an.groups.map((g) => g.tokens)]) {
            const M = grp.match;
            for (let i = 1; i < grp.length; i++) {
                const t = grp[i];
                if (t.v !== '(' || t.s < s || t.s > e || grp[i - 1].t !== 'id') continue;
                if (grp[i - 2] && grp[i - 2].t === 'kw' && grp[i - 2].v === 'def') continue;
                const callee = A.calleeOf(an, grp, i);
                const sig = callee && A.callSignature(callee.ent, callee.viaInstance);
                if (!sig || !sig.params.length) continue;
                let arg = 0, depth = 0, argStart = i + 1;
                for (let j = i + 1; j <= M[i] && j < grp.length; j++) {
                    const v = grp[j];
                    const end = j === M[i];
                    if (v.t === 'op' && A.OPEN[v.v]) depth++;
                    else if (v.t === 'op' && A.CLOSE[v.v] && !end) depth--;
                    if ((depth === 0 && v.v === ',') || end) {
                        const first = grp[argStart];
                        const p = sig.params[arg];
                        if (!p || p.startsWith('*')) break;
                        const single = j - argStart === 1 || (j - argStart === 2 && first.v === '-');
                        const lit = first && (first.t === 'num' || first.t === 'str' || first.t === 'fstr' || ['True', 'False', 'None'].includes(first.v) || first.v === '-');
                        const isKw = grp[argStart + 1] && grp[argStart + 1].v === '=';
                        if (first && single && lit && !isKw) {
                            const name = p.replace(/\s*=.*$/, '');
                            const hint = new vscode.InlayHint(toPos(an, first.s), name + ':', vscode.InlayHintKind.Parameter);
                            hint.paddingRight = true;
                            out.push(hint);
                        }
                        arg++;
                        argStart = j + 1;
                    }
                }
            }
        }
        return out;
    },
};

// =================================================================== code actions

const codeActionProvider = {
    provideCodeActions(doc, range, ctx) {
        const out = [];
        const an = project.forDocument(doc);
        for (const d of ctx.diagnostics) {
            const code = typeof d.code === 'object' ? d.code.value : d.code;
            if (code === 'missing-semicolon') {
                const line = d.range.start.line;
                const toks = an.tokens.filter((t) => an.lineOf(t.s) === line);
                const last = toks[toks.length - 1];
                if (!last) continue;
                const fix = new vscode.CodeAction("Add ';'", vscode.CodeActionKind.QuickFix);
                fix.edit = new vscode.WorkspaceEdit();
                fix.edit.insert(doc.uri, toPos(an, last.e), ';');
                fix.diagnostics = [d];
                fix.isPreferred = true;
                out.push(fix);
            } else if (code === 'colon-block') {
                const line = doc.lineAt(d.range.start.line);
                const m = /:\s*(#.*)?$/.exec(line.text);
                if (!m) continue;
                const fix = new vscode.CodeAction("Replace ':' with '{'", vscode.CodeActionKind.QuickFix);
                fix.edit = new vscode.WorkspaceEdit();
                fix.edit.replace(doc.uri, new vscode.Range(line.lineNumber, m.index, line.lineNumber, m.index + 1), ' {');
                fix.diagnostics = [d];
                out.push(fix);
            } else if (code === 'faston.init') {
                const fix = new vscode.CodeAction("Rename to 'init'", vscode.CodeActionKind.QuickFix);
                fix.edit = new vscode.WorkspaceEdit();
                fix.edit.replace(doc.uri, d.range, 'init');
                fix.diagnostics = [d];
                fix.isPreferred = true;
                out.push(fix);
            } else if (code === 'faston.super') {
                const off = doc.offsetAt(d.range.start);
                const tok = an.tokenAt(off);
                const T = an.tokens;
                if (!tok || tok.grp !== T) continue;
                const i = tok.i;
                const cls = an.enclosingClass(off);
                // super ( ) . name (
                if (!cls || !cls.base || !(T[i + 3] && T[i + 3].v === '.' && T[i + 4] && T[i + 4].t === 'id' && T[i + 5] && T[i + 5].v === '(')) continue;
                const empty = T[i + 6] && T[i + 6].v === ')';
                const fix = new vscode.CodeAction(`Call ${cls.base}.${T[i + 4].v}(self, ...) directly`, vscode.CodeActionKind.QuickFix);
                fix.edit = new vscode.WorkspaceEdit();
                fix.edit.replace(doc.uri, toRange(an, T[i].s, T[i + 5].e), `${cls.base}.${T[i + 4].v}(self${empty ? '' : ', '}`);
                fix.diagnostics = [d];
                fix.isPreferred = true;
                out.push(fix);
            } else if (code === 'faston.undefined') {
                const name = doc.getText(d.range);
                const SUG = { true: 'True', false: 'False', null: 'None', none: 'None', nil: 'None', this: 'self' };
                if (SUG[name]) {
                    const fix = new vscode.CodeAction(`Change to '${SUG[name]}'`, vscode.CodeActionKind.QuickFix);
                    fix.edit = new vscode.WorkspaceEdit();
                    fix.edit.replace(doc.uri, d.range, SUG[name]);
                    fix.diagnostics = [d];
                    fix.isPreferred = true;
                    out.push(fix);
                }
                if (BI.MODULES[name] || moduleFile(an, name)) {
                    const fix = new vscode.CodeAction(`Add 'import ${name};'`, vscode.CodeActionKind.QuickFix);
                    fix.edit = new vscode.WorkspaceEdit();
                    fix.edit.set(doc.uri, [importInsertEdit(an, doc, name)]);
                    fix.diagnostics = [d];
                    fix.isPreferred = true;
                    out.push(fix);
                }
                // `from m import name` for top-level names of sibling modules
                for (const m of siblingModules(an)) {
                    const mod = an.getModule(m);
                    const s = mod && mod.module.get(name);
                    if (!s || s.kind === 'import' || s.kind === 'module') continue;
                    const fix = new vscode.CodeAction(`Add 'from ${m} import ${name};'`, vscode.CodeActionKind.QuickFix);
                    fix.edit = new vscode.WorkspaceEdit();
                    const e = importInsertEdit(an, doc, name);
                    fix.edit.insert(doc.uri, e.range.start, `from ${m} import ${name};\n`);
                    fix.diagnostics = [d];
                    out.push(fix);
                }
            }
        }
        return out;
    },
};

// =================================================================== executable

function expandVars(p, uri) {
    const folder = (uri && vscode.workspace.getWorkspaceFolder(uri)) || (vscode.workspace.workspaceFolders || [])[0];
    return p
        .replace(/\$\{workspaceFolder\}/g, folder ? folder.uri.fsPath : '')
        .replace(/\$\{userHome\}/g, os.homedir())
        .replace(/^~(?=[\\/])/, os.homedir());
}

function onPath(name) {
    const exts = IS_WIN ? (process.env.PATHEXT || '.EXE;.CMD;.BAT').split(';') : [''];
    for (const dir of (process.env.PATH || '').split(path.delimiter)) {
        if (!dir) continue;
        for (const ext of IS_WIN && !path.extname(name) ? exts : ['']) {
            const p = path.join(dir, name + ext);
            try { if (fs.statSync(p).isFile()) return p; } catch { /* keep looking */ }
        }
    }
    return null;
}

/** Finds the faston executable for a file (setting, workspace build, PATH). */
function findExecutable(uri) {
    const cfg = vscode.workspace.getConfiguration('faston', uri);
    const configured = (cfg.get('executablePath') || '').trim();
    if (configured) {
        const p = expandVars(configured, uri);
        if (path.isAbsolute(p) || p.includes('/') || p.includes('\\')) return fs.existsSync(p) ? p : null;
        return onPath(p);
    }
    const roots = [];
    for (const f of vscode.workspace.workspaceFolders || []) roots.push(f.uri.fsPath);
    if (uri && uri.scheme === 'file') {
        // also walk up from the file, for files outside the workspace
        let d = path.dirname(uri.fsPath);
        for (let i = 0; i < 8; i++) {
            roots.push(d);
            const up = path.dirname(d);
            if (up === d) break;
            d = up;
        }
    }
    for (const r of roots) {
        for (const sub of ['target/release', 'target/debug']) {
            const p = path.join(r, sub, EXE_NAME);
            if (fs.existsSync(p)) return p;
        }
    }
    return onPath('faston');
}

function compilerProjectDir() {
    for (const f of vscode.workspace.workspaceFolders || []) {
        const p = path.join(f.uri.fsPath, 'Cargo.toml');
        try {
            if (/^\s*name\s*=\s*"faston"/m.test(fs.readFileSync(p, 'utf8'))) return f.uri.fsPath;
        } catch { /* not this one */ }
    }
    return null;
}

let warnedMissing = false;
async function missingExecutable(force) {
    if (warnedMissing && !force) return;
    warnedMissing = true;
    const actions = [];
    if (compilerProjectDir()) actions.push('Build Faston');
    actions.push('Select Executable', 'Open Settings');
    const pick = await vscode.window.showWarningMessage(
        'The Faston executable was not found, so files can\'t be run or checked for errors. ' +
        'Build it (cargo build --release), put it on your PATH, or set "faston.executablePath".', ...actions);
    if (pick === 'Build Faston') vscode.commands.executeCommand('faston.buildCompiler');
    else if (pick === 'Select Executable') vscode.commands.executeCommand('faston.selectExecutable');
    else if (pick === 'Open Settings') vscode.commands.executeCommand('workbench.action.openSettings', 'faston.executablePath');
}

const versionCache = new Map();
function exeVersion(exe) {
    if (versionCache.has(exe)) return versionCache.get(exe);
    let v = null;
    try {
        const r = cp.spawnSync(exe, ['--version'], { encoding: 'utf8', timeout: 3000, windowsHide: true });
        const m = /Faston\s+(\S+)/.exec(r.stdout || '');
        v = m ? m[1] : null;
    } catch { /* ignore */ }
    versionCache.set(exe, v);
    return v;
}

// =================================================================== compiler diagnostics

class Checker {
    constructor() {
        this.compilerDiags = vscode.languages.createDiagnosticCollection('faston');
        this.lintDiags = vscode.languages.createDiagnosticCollection('faston-lint');
        this.timers = new Map();
        this.running = new Map();
        this.tmpDir = path.join(os.tmpdir(), 'faston-vscode');
        try { fs.mkdirSync(this.tmpDir, { recursive: true }); } catch { /* ignore */ }
    }
    dispose() {
        for (const t of this.timers.values()) clearTimeout(t);
        for (const p of this.running.values()) try { p.kill(); } catch { /* gone */ }
        this.compilerDiags.dispose();
        this.lintDiags.dispose();
    }
    schedule(doc, delay) {
        if (doc.languageId !== LANG) return;
        const key = doc.uri.toString();
        clearTimeout(this.timers.get(key));
        this.timers.set(key, setTimeout(() => {
            this.timers.delete(key);
            this.run(doc);
        }, delay));
    }
    clear(doc) {
        const key = doc.uri.toString();
        clearTimeout(this.timers.get(key));
        this.timers.delete(key);
        this.compilerDiags.delete(doc.uri);
        this.lintDiags.delete(doc.uri);
        try { fs.unlinkSync(this.tmpFile(doc)); } catch { /* not there */ }
    }
    tmpFile(doc) {
        const h = crypto.createHash('sha1').update(doc.uri.toString()).digest('hex').slice(0, 16);
        return path.join(this.tmpDir, h + '.fst');
    }
    run(doc) {
        if (doc.isClosed) return;
        const cfg = vscode.workspace.getConfiguration('faston', doc.uri);
        // editor lint
        if (cfg.get('lint.enable')) {
            const an = project.forDocument(doc);
            const diags = lint(an, { undefinedNames: true }).map((l) => {
                const sev = { error: 0, warning: 1, info: 2, hint: 3 }[l.severity];
                const d = new vscode.Diagnostic(toRange(an, l.s, l.e), l.message, sev);
                d.source = 'faston';
                d.code = l.code;
                return d;
            });
            this.lintDiags.set(doc.uri, diags);
        } else {
            this.lintDiags.delete(doc.uri);
        }
        // the real compiler
        if (!cfg.get('diagnostics.enable')) {
            this.compilerDiags.delete(doc.uri);
            return;
        }
        const exe = findExecutable(doc.uri);
        if (!exe) {
            this.compilerDiags.delete(doc.uri);
            missingExecutable(false);
            return;
        }
        const key = doc.uri.toString();
        const prev = this.running.get(key);
        if (prev) try { prev.kill(); } catch { /* gone */ }
        const version = doc.version;
        const file = this.tmpFile(doc);
        try {
            fs.writeFileSync(file, doc.getText());
        } catch (e) {
            output.appendLine(`can't write ${file}: ${e.message}`);
            return;
        }
        let stderr = '';
        let proc;
        try {
            proc = cp.spawn(exe, ['--dis', file], { stdio: ['ignore', 'ignore', 'pipe'], windowsHide: true });
        } catch (e) {
            output.appendLine(`can't start ${exe}: ${e.message}`);
            return;
        }
        this.running.set(key, proc);
        const timer = setTimeout(() => { try { proc.kill(); } catch { /* gone */ } }, 10000);
        proc.stderr.on('data', (d) => { stderr += d; });
        proc.on('error', (e) => {
            clearTimeout(timer);
            if (this.running.get(key) === proc) this.running.delete(key);
            output.appendLine(`faston check failed: ${e.message}`);
        });
        proc.on('close', (code, signal) => {
            clearTimeout(timer);
            if (this.running.get(key) !== proc) return; // superseded
            this.running.delete(key);
            if (signal || doc.isClosed || doc.version !== version) return;
            this.compilerDiags.set(doc.uri, code === 0 ? [] : parseCompilerErrors(stderr, doc));
        });
    }
}

function parseCompilerErrors(stderr, doc) {
    const out = [];
    for (const raw of stderr.split(/\r?\n/)) {
        const lineText = raw.trim();
        const m = /^\[line (\d+)\]\s*(.*)$/.exec(lineText);
        if (!m) continue;
        const ln = Math.min(Math.max(0, parseInt(m[1], 10) - 1), doc.lineCount - 1);
        const msg = m[2];
        const text = doc.lineAt(ln).text;
        let range = null;
        let code;
        const at = /^SyntaxError at '((?:[^']|'(?!:))*)':/.exec(msg);
        if (/missing ';'/.test(msg)) {
            code = 'missing-semicolon';
            const end = codeEnd(text);
            range = new vscode.Range(ln, Math.max(0, end - 1), ln, end);
        } else if (/at end of file/.test(msg)) {
            let l = doc.lineCount - 1;
            while (l > 0 && doc.lineAt(l).isEmptyOrWhitespace) l--;
            const t = doc.lineAt(l).text;
            range = new vscode.Range(l, Math.max(0, t.length - 1), l, t.length);
        } else if (at) {
            let tok = at[1];
            if (tok.startsWith('"') && tok.endsWith('"') && tok.length > 1) tok = tok.slice(1, -1);
            const idx = tok ? text.indexOf(tok) : -1;
            if (idx >= 0) range = new vscode.Range(ln, idx, ln, idx + tok.length);
        }
        if (/instead of ':'/.test(msg)) code = 'colon-block';
        if (!range) {
            const s = text.length - text.trimStart().length;
            range = new vscode.Range(ln, s, ln, Math.max(s + 1, codeEnd(text)));
        }
        const d = new vscode.Diagnostic(range, msg, vscode.DiagnosticSeverity.Error);
        d.source = 'faston';
        if (code) d.code = code;
        out.push(d);
        break; // the compiler stops at the first error
    }
    if (!out.length && stderr.trim()) {
        const d = new vscode.Diagnostic(new vscode.Range(0, 0, 0, 0), stderr.trim().split(/\r?\n/)[0], vscode.DiagnosticSeverity.Error);
        d.source = 'faston';
        out.push(d);
    }
    return out;
}

/** Column after the last code character of a line (ignoring a trailing comment). */
function codeEnd(text) {
    const toks = A.lex(text).tokens;
    return toks.length ? toks[toks.length - 1].e : text.trimEnd().length;
}

// =================================================================== running

async function targetDocument(uri) {
    if (uri instanceof vscode.Uri) return vscode.workspace.openTextDocument(uri);
    const ed = vscode.window.activeTextEditor;
    if (ed && ed.document.languageId === LANG) return ed.document;
    vscode.window.showErrorMessage('Open a Faston (.fst) file first.');
    return null;
}

async function prepare(uri) {
    const doc = await targetDocument(uri);
    if (!doc) return null;
    const exe = findExecutable(doc.uri);
    if (!exe) {
        missingExecutable(true);
        return null;
    }
    if (vscode.workspace.getConfiguration('faston', doc.uri).get('run.saveBeforeRun')) {
        for (const d of vscode.workspace.textDocuments) {
            if (d.isDirty && d.languageId === LANG && !d.isUntitled) await d.save();
        }
    }
    let file;
    if (doc.isUntitled) {
        file = path.join(os.tmpdir(), 'faston-vscode', 'untitled.fst');
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, doc.getText());
    } else {
        file = doc.uri.fsPath;
    }
    return { doc, exe, file };
}

function taskFor(def, exe, file, scope) {
    const args = [];
    let name;
    if (def.command === 'build') {
        args.push('build', file);
        if (def.output) args.push('-o', def.output);
        name = `build ${path.basename(file)}`;
    } else {
        args.push('run');
        if (def.native) args.push('--native');
        args.push(file);
        name = `run ${path.basename(file)}${def.native ? ' (native)' : ''}`;
    }
    if (Array.isArray(def.args)) args.push(...def.args);
    const cwd = file.includes('${') ? '${fileDirname}' : path.dirname(file);
    const task = new vscode.Task(def, scope || vscode.TaskScope.Workspace, name, 'faston',
        new vscode.ProcessExecution(exe, args, { cwd }), ['$faston', '$faston-import']);
    task.presentationOptions = {
        reveal: vscode.TaskRevealKind.Always,
        focus: def.command !== 'build',
        panel: vscode.TaskPanelKind.Shared,
        clear: vscode.workspace.getConfiguration('faston').get('run.clearTerminal'),
        echo: true,
        showReuseMessage: false,
    };
    if (def.command === 'build') task.group = vscode.TaskGroup.Build;
    return task;
}

function quoteFor(shell, s) {
    if (/pwsh|powershell/i.test(shell)) return `'${s.replace(/'/g, "''")}'`;
    if (/cmd\.exe$/i.test(shell)) return `"${s}"`;
    return `'${s.replace(/'/g, `'\\''`)}'`;
}

let fallbackTerminal = null;
function runInTerminal(exe, args, cwd) {
    if (!fallbackTerminal || fallbackTerminal.exitStatus) {
        fallbackTerminal = vscode.window.createTerminal({ name: 'Faston', cwd });
    }
    const shell = vscode.env.shell || '';
    const call = /pwsh|powershell/i.test(shell) ? '& ' : '';
    fallbackTerminal.show();
    fallbackTerminal.sendText(call + [exe, ...args].map((a) => quoteFor(shell, a)).join(' '));
}

async function runCommand(uri, def) {
    const p = await prepare(uri);
    if (!p) return;
    const folder = p.doc.uri.scheme === 'file' ? vscode.workspace.getWorkspaceFolder(p.doc.uri) : undefined;
    // stop a previous run of the same kind (a game loop that's still going, say)
    for (const ex of vscode.tasks.taskExecutions) {
        if (ex.task.source === 'faston' && ex.task.definition.command === def.command) ex.terminate();
    }
    if (!vscode.workspace.workspaceFolders || !vscode.workspace.workspaceFolders.length) {
        const t = taskFor(def, p.exe, p.file);
        runInTerminal(p.exe, t.execution.args, path.dirname(p.file));
        return;
    }
    try {
        await vscode.tasks.executeTask(taskFor(def, p.exe, p.file, folder || vscode.TaskScope.Workspace));
    } catch (e) {
        output.appendLine(`task failed (${e.message}); using a terminal`);
        const t = taskFor(def, p.exe, p.file);
        runInTerminal(p.exe, t.execution.args, path.dirname(p.file));
    }
}

const taskProvider = {
    provideTasks() {
        const ed = vscode.window.activeTextEditor;
        const exe = findExecutable(ed && ed.document.uri);
        if (!exe) return [];
        const file = '${file}';
        return [
            taskFor({ type: 'faston', command: 'run' }, exe, file),
            taskFor({ type: 'faston', command: 'run', native: true }, exe, file),
            taskFor({ type: 'faston', command: 'build' }, exe, file),
        ];
    },
    resolveTask(task) {
        const def = task.definition;
        if (def.type !== 'faston') return undefined;
        const ed = vscode.window.activeTextEditor;
        const exe = findExecutable(ed && ed.document.uri);
        if (!exe) {
            missingExecutable(true);
            return undefined;
        }
        const t = taskFor(def, exe, def.file || '${file}', task.scope);
        return new vscode.Task(def, task.scope || vscode.TaskScope.Workspace, task.name, 'faston', t.execution, task.problemMatchers.length ? task.problemMatchers : ['$faston', '$faston-import']);
    },
};

// ---- REPL

let repl = null;
function ensureRepl(cwd) {
    if (repl && !repl.exitStatus) return repl;
    const ed = vscode.window.activeTextEditor;
    const exe = findExecutable(ed && ed.document.uri);
    if (!exe) {
        missingExecutable(true);
        return null;
    }
    repl = vscode.window.createTerminal({ name: 'Faston REPL', shellPath: exe, cwd, iconPath: new vscode.ThemeIcon('zap') });
    return repl;
}

async function sendToRepl() {
    const ed = vscode.window.activeTextEditor;
    if (!ed) return;
    let text = ed.selections.map((s) => ed.document.getText(s)).join('\n');
    if (!text.trim()) text = ed.document.lineAt(ed.selection.active.line).text;
    const cwd = ed.document.uri.scheme === 'file' ? path.dirname(ed.document.uri.fsPath) : undefined;
    const fresh = !repl || !!repl.exitStatus;
    const term = ensureRepl(cwd);
    if (!term) return;
    term.show(true);
    if (fresh) await new Promise((r) => setTimeout(r, 400));
    // the prompt reads line by line and waits for brackets to balance
    const lines = text.replace(/\r\n/g, '\n').split('\n').filter((l, i, a) => l.trim() !== '' || i < a.length - 1);
    term.sendText(lines.join('\n'), true);
    if (ed.selection.isEmpty && ed.selection.active.line < ed.document.lineCount - 1) {
        const next = new vscode.Position(ed.selection.active.line + 1, 0);
        ed.selection = new vscode.Selection(next, next);
    }
}

// ---- bytecode view

const BYTECODE_SCHEME = 'faston-bytecode';
const bytecodeContent = new Map();
const bytecodeEmitter = new vscode.EventEmitter();
const bytecodeProvider = {
    onDidChange: bytecodeEmitter.event,
    provideTextDocumentContent(uri) {
        return bytecodeContent.get(uri.toString()) || '';
    },
};

async function showBytecode(uri) {
    const doc = await targetDocument(uri);
    if (!doc) return;
    const exe = findExecutable(doc.uri);
    if (!exe) return missingExecutable(true);
    const file = path.join(os.tmpdir(), 'faston-vscode', 'bytecode-' + crypto.createHash('sha1').update(doc.uri.toString()).digest('hex').slice(0, 12) + '.fst');
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, doc.getText());
    const r = cp.spawnSync(exe, ['--dis', file], { encoding: 'utf8', timeout: 20000, maxBuffer: 256 << 20, windowsHide: true });
    const text = r.status === 0 ? r.stdout : `# compile error\n${r.stderr || r.error || ''}`;
    const name = path.basename(doc.uri.fsPath || doc.uri.path || 'untitled').replace(/\.fst$/, '');
    const vuri = vscode.Uri.from({ scheme: BYTECODE_SCHEME, path: `/${name}.bytecode`, query: doc.uri.toString() });
    bytecodeContent.set(vuri.toString(), text);
    bytecodeEmitter.fire(vuri);
    const vdoc = await vscode.workspace.openTextDocument(vuri);
    await vscode.window.showTextDocument(vdoc, { viewColumn: vscode.ViewColumn.Beside, preview: true, preserveFocus: true });
}

// ---- compiler build

async function buildCompiler() {
    const dir = compilerProjectDir();
    if (!dir) {
        vscode.window.showErrorMessage('No Faston compiler project (Cargo.toml with name = "faston") in this workspace.');
        return;
    }
    const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(dir));
    const task = new vscode.Task({ type: 'faston-cargo' }, folder || vscode.TaskScope.Workspace, 'cargo build --release', 'faston',
        new vscode.ProcessExecution('cargo', ['build', '--release'], { cwd: dir }), ['$rustc']);
    task.presentationOptions = { reveal: vscode.TaskRevealKind.Always, panel: vscode.TaskPanelKind.Dedicated, clear: true };
    const exec = await vscode.tasks.executeTask(task);
    const sub = vscode.tasks.onDidEndTaskProcess((e) => {
        if (e.execution !== exec) return;
        sub.dispose();
        versionCache.clear();
        if (e.exitCode === 0) {
            vscode.window.showInformationMessage('Faston built. Error checking and Run are ready.');
            warnedMissing = false;
            recheckAll();
            updateStatus();
        }
    });
}

async function selectExecutable() {
    const picked = await vscode.window.showOpenDialog({
        canSelectMany: false,
        openLabel: 'Use this Faston',
        filters: IS_WIN ? { Executables: ['exe'] } : undefined,
    });
    if (!picked || !picked.length) return;
    await vscode.workspace.getConfiguration('faston').update('executablePath', picked[0].fsPath, vscode.ConfigurationTarget.Global);
    versionCache.clear();
    recheckAll();
    updateStatus();
}

// =================================================================== status bar

let status;
function updateStatus() {
    const ed = vscode.window.activeTextEditor;
    if (!ed || ed.document.languageId !== LANG) {
        status.hide();
        return;
    }
    const exe = findExecutable(ed.document.uri);
    if (!exe) {
        status.text = '$(warning) Faston';
        status.tooltip = 'Faston executable not found: click to fix';
        status.command = 'faston.selectExecutable';
        if (compilerProjectDir()) status.command = 'faston.buildCompiler';
    } else {
        const v = exeVersion(exe);
        status.text = `$(zap) Faston${v ? ' ' + v : ''}`;
        status.tooltip = new vscode.MarkdownString(`**Faston** ${v || ''}\n\n\`${exe}\`\n\nClick to run this file (F5).`);
        status.command = 'faston.run';
    }
    status.show();
}

// =================================================================== activation

let checker;
function recheckAll() {
    for (const d of vscode.workspace.textDocuments) if (d.languageId === LANG) checker.schedule(d, 0);
}

function activate(context) {
    output = vscode.window.createOutputChannel('Faston');
    checker = new Checker();
    status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    const L = vscode.languages;
    const sub = (...d) => context.subscriptions.push(...d);

    sub(output, checker, status,
        L.registerCompletionItemProvider(SELECTOR, completionProvider, '.'),
        L.registerHoverProvider(SELECTOR, hoverProvider),
        L.registerDefinitionProvider(SELECTOR, definitionProvider),
        L.registerTypeDefinitionProvider(SELECTOR, typeDefinitionProvider),
        L.registerReferenceProvider(SELECTOR, referenceProvider),
        L.registerDocumentHighlightProvider(SELECTOR, highlightProvider),
        L.registerRenameProvider(SELECTOR, renameProvider),
        L.registerDocumentSymbolProvider(SELECTOR, documentSymbolProvider, { label: 'Faston' }),
        L.registerWorkspaceSymbolProvider(workspaceSymbolProvider),
        L.registerSignatureHelpProvider(SELECTOR, signatureProvider, { triggerCharacters: ['(', ','], retriggerCharacters: [',', '='] }),
        L.registerDocumentSemanticTokensProvider(SELECTOR, semanticProvider, LEGEND),
        L.registerFoldingRangeProvider(SELECTOR, foldingProvider),
        L.registerDocumentFormattingEditProvider(SELECTOR, formattingProvider),
        L.registerDocumentRangeFormattingEditProvider(SELECTOR, formattingProvider),
        L.registerOnTypeFormattingEditProvider(SELECTOR, onTypeFormattingProvider, '}'),
        L.registerInlayHintsProvider(SELECTOR, inlayProvider),
        L.registerCodeActionsProvider(SELECTOR, codeActionProvider, { providedCodeActionKinds: [vscode.CodeActionKind.QuickFix] }),
        vscode.workspace.registerTextDocumentContentProvider(BYTECODE_SCHEME, bytecodeProvider),
        vscode.tasks.registerTaskProvider('faston', taskProvider),

        vscode.commands.registerCommand('faston.run', (uri) => {
            const native = vscode.workspace.getConfiguration('faston').get('run.native');
            return runCommand(uri, { type: 'faston', command: 'run', native });
        }),
        vscode.commands.registerCommand('faston.runNative', (uri) => runCommand(uri, { type: 'faston', command: 'run', native: true })),
        vscode.commands.registerCommand('faston.build', (uri) => runCommand(uri, { type: 'faston', command: 'build' })),
        vscode.commands.registerCommand('faston.showBytecode', showBytecode),
        vscode.commands.registerCommand('faston.openRepl', () => {
            const ed = vscode.window.activeTextEditor;
            const cwd = ed && ed.document.uri.scheme === 'file' ? path.dirname(ed.document.uri.fsPath) : undefined;
            const t = ensureRepl(cwd);
            if (t) t.show();
        }),
        vscode.commands.registerCommand('faston.sendToRepl', sendToRepl),
        vscode.commands.registerCommand('faston.buildCompiler', buildCompiler),
        vscode.commands.registerCommand('faston.selectExecutable', selectExecutable),

        vscode.workspace.onDidOpenTextDocument((d) => checker.schedule(d, 0)),
        vscode.workspace.onDidChangeTextDocument((e) => {
            const d = e.document;
            if (d.languageId !== LANG || !e.contentChanges.length) return;
            const cfg = vscode.workspace.getConfiguration('faston', d.uri);
            if (cfg.get('diagnostics.onType')) checker.schedule(d, cfg.get('diagnostics.delay'));
        }),
        vscode.workspace.onDidSaveTextDocument((d) => {
            if (d.languageId !== LANG) return;
            checker.schedule(d, 0);
            // files that import this one may have new / missing names now
            for (const o of vscode.workspace.textDocuments) if (o !== d && o.languageId === LANG) checker.schedule(o, 200);
        }),
        vscode.workspace.onDidCloseTextDocument((d) => {
            if (d.languageId === LANG) checker.clear(d);
            if (d.uri.scheme === BYTECODE_SCHEME) bytecodeContent.delete(d.uri.toString());
        }),
        vscode.workspace.onDidChangeConfiguration((e) => {
            if (e.affectsConfiguration('faston')) {
                versionCache.clear();
                warnedMissing = false;
                recheckAll();
                updateStatus();
            }
        }),
        vscode.window.onDidChangeActiveTextEditor(updateStatus),
        vscode.window.onDidCloseTerminal((t) => {
            if (t === repl) repl = null;
            if (t === fallbackTerminal) fallbackTerminal = null;
        }),
    );

    // a newly built executable shows up without reloading
    const watcher = vscode.workspace.createFileSystemWatcher(`**/target/{release,debug}/${EXE_NAME}`);
    const refresh = () => { versionCache.clear(); warnedMissing = false; updateStatus(); recheckAll(); };
    sub(watcher, watcher.onDidCreate(refresh), watcher.onDidChange(() => versionCache.clear()));

    recheckAll();
    updateStatus();
}

function deactivate() {}

module.exports = { activate, deactivate, _test: { parseCompilerErrors, findExecutable, describe, documentSymbols } };
