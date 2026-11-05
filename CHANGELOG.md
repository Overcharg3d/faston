# Changelog

## 0.4.1

- Compiled modules (`name.fbc`) count as importable modules
- f-string conversions (`{x!r}`, `!s`, `!a`) and `ascii()`
- `**rest` captures in mapping patterns

## 0.4.0

- `match` / `case` statements: highlighting, hover docs, completions; capture
  names in patterns are known variables (no false "undefined" warnings)
- `enum Name { ... }` and `class X(Enum)`: members show up in completion and hover
- Bytes: `b"..."` / `rb"..."` literals, `bytes`, `bytearray` and their methods,
  `str.encode`, `\x` / `\u` escapes
- The `bytecode` and `enum` modules

## 0.3.0

- Packages: dotted imports (`import game.enemies;`, `from a.b import c as d;`)
  resolve like the runtime does: next to the file, the project root
  (`faston.toml`), `.faston/packages`, and `FASTON_PATH`; go to definition,
  hover and completion work through them
- The new modules `re`, `collections`, `datetime`, `subprocess` and `sys.path`
- Raw strings (`r"\d+"`)

## 0.2.0

- New syntax: `is`, `del`, `with`, `yield`, decorators (`@property`, ...)
- Completion, hover and parameter hints for the new modules (`os`, `os.path`,
  `sys`, `json`, `shutil`, `gui`), file objects (`with (open(p) as f)`),
  generators, the gui `ui` / canvas objects, and the new built-ins and string /
  dict methods
- `super()` is no longer flagged (Faston supports it now)
- Snippets: `with`, `prop`, `gen`, `guiapp`, `guigame`

## 0.1.0

First release: syntax highlighting, live compiler errors, editor checks with
quick fixes, completion, hover, signature help, go to definition, references,
rename, outline, semantic highlighting, folding, formatting, inlay hints,
snippets, run / native run / build / bytecode / REPL commands and tasks.
