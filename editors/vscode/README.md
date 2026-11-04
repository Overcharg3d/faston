# Faston for VS Code

Language support for [Faston](https://github.com/Overcharg3d/faston), a
Python-like language with braces and semicolons. Files ending in `.fst` are
recognized automatically.

## Features

**Writing code**
- Syntax highlighting, including f-string interpolations and format specs,
  plus semantic highlighting (classes, functions, parameters, constants,
  modules)
- Completion for names in scope, `self.` members, class instances, modules
  (`math`, `random`, `time`, `os`, `os.path`, `sys`, `json`, `shutil`, `gui`,
  and your own `.fst` modules), files (`with (open(p) as f)`), and methods of
  str/list/dict/set/tuple and of the gui `ui` and canvas objects. Unimported modules can be completed too; the
  `import` line is added for you.
- Hover docs for everything, including every built-in function, method and
  module, plus inferred types (`p: Player`, `words: list`)
- Signature help while typing arguments, including keyword arguments
- Snippets: `def`, `class`, `init`, `forr`, `fork`, `try`, `with`, `prop`,
  `gen`, `lambda`, `main`, `guiapp`, `guigame`, and more
- Auto-indent, bracket matching and auto-closing, comment toggling
  (`Ctrl+/`), folding (blocks, comment blocks, `# region`)

**Navigating**
- Go to definition (`F12`), also across `import`ed files. Ctrl+click works on
  module names.
- Find all references (`Shift+F12`) and rename (`F2`) across the workspace
- Outline, breadcrumbs and Go to Symbol (`Ctrl+Shift+O`), workspace symbol
  search (`Ctrl+T`)

**Errors**
- The real Faston compiler checks the file as you type and underlines syntax
  errors. A quick fix adds a missing `;`.
- Editor checks that the compiler can't do before run time:
  - undefined names, with "did you mean `True`?" and add-import fixes
  - missing modules or imported names
  - the Python habit `__init__` (quick fix renames it to `init`)
- Runtime tracebacks from **Run** are linked back to the source line

**Running**
- **F5** runs the current file in the interpreter, **Ctrl+F5** compiles it
  to native code first, and the ▶ menu in the editor title has
  **Build Standalone Executable**. `input()` works in the terminal.
- **Faston: Open Interactive Prompt (REPL)**. Select code and press
  **Shift+Enter** to send it there.
- **Faston: Show Bytecode** shows the compiled bytecode next to your code
- Tasks: `"type": "faston"` with `"command": "run"` or `"build"` in
  `tasks.json`

**Formatting**
- Format Document (`Shift+Alt+F`) and Format Selection re-indent from the
  brace structure and trim trailing whitespace. They never change code, and
  never touch the inside of multi-line strings.

## Finding the compiler

The extension looks for the `faston` executable in this order:

1. the `faston.executablePath` setting
2. `target/release/faston` (then `target/debug`) in the open folder, or in a
   folder above the file
3. `faston` on your `PATH`

If you have the Faston source open, **Faston: Build the Faston Compiler** runs
`cargo build --release` for you.

## Settings

| Setting | Default | |
|---|---|---|
| `faston.executablePath` | `""` | Path to `faston` |
| `faston.diagnostics.enable` | `true` | Check files with the compiler |
| `faston.diagnostics.onType` | `true` | Check while typing (not just on save) |
| `faston.diagnostics.delay` | `350` | Milliseconds to wait after typing |
| `faston.lint.enable` | `true` | Editor checks (undefined names, imports, Python habits) |
| `faston.run.native` | `false` | Make F5 compile to native code |
| `faston.run.saveBeforeRun` | `true` | Save before running |
| `faston.run.clearTerminal` | `true` | Clear the terminal on each run |
| `faston.inlayHints.parameterNames` | `false` | Show `name:` before literal arguments |

## Limitations

- Type inference is local and simple: assignments from constructors,
  literals and calls with known return types. Unknown types fall back to
  suggesting every method.
- There is no debugger (Faston has no debug protocol yet), so F5 runs the
  file without debugging.

## Building the extension

```sh
cd editors/vscode
npx @vscode/vsce package
code --install-extension faston-0.2.0.vsix
```
