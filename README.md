# Faston

A Python-like language with braces, parentheses and semicolons. Programs are
**compiled to native machine code** (with Cranelift) or run on a fast bytecode
interpreter — same language, same results. Written in Rust.

```faston
import math;

class Player {
    def init(self, name, hp = 100) {
        self.name = name;
        self.hp = hp;
    }

    def hit(self, dmg) {
        self.hp -= dmg;
        return self.hp > 0;
    }
}

let p = Player("Overcharg3d");
for (i in range(3)) {
    if (not p.hit(40)) {
        print(f"{p.name} is down after {i + 1} hits");
        break;
    }
}
print(f"sqrt(2) = {math.sqrt(2):.4f}");
```

It also has what real programs need: files, `os` / `sys` / `json` / `shutil`
modules, generators, `with`, decorators, properties, and a built-in `gui`
module for desktop apps and games:

```faston
import gui;

let clicks = 0;
def draw(ui) {
    global clicks;
    ui.heading("Hello from Faston");
    if (ui.button("Click me")) { clicks += 1; }
    ui.label(f"clicked {clicks} times");
    let c = ui.canvas(300, 100, background="#101820");
    c.circle(50 + clicks * 10, 50, 20, "orange");
}
gui.run(draw, title="Demo", width=400, height=260);
```

## Build and run

Requires a recent stable Rust toolchain (`rustup`). The `gui` module is
included by default; `cargo build --release --no-default-features` builds a
smaller `faston` without it. `scripts/build.ps1` (Windows) or
`scripts/build.sh` builds both: `faston` and `faston-slim` (see *Executable
size* below).

```sh
cargo build --release
./target/release/faston new mygame            # a new project (faston.toml + main.fst)
./target/release/faston build game.fst        # -> ./game, a standalone native executable
./target/release/faston run --native game.fst # compile to machine code and run now
./target/release/faston game.fst              # run in the interpreter (instant start)
./target/release/faston                       # interactive prompt (interpreter)
./target/release/faston --dis file.fst        # show the bytecode
cargo test --release                          # test suite (interpreted + native)
tests/native.sh                               # quick native-only test run
python3 tests/fuzz.py 500                     # differential fuzzing vs CPython
FASTON_NATIVE=1 python3 tests/fuzz.py 500     # ...with native code
FASTON_GC_STRESS=1 ./target/release/faston f.fst   # collect garbage at every safe point
bench/run.sh                                  # benchmarks: interpreted / native / CPython
```

Source files use the `.fst` extension.

## Native compilation

`faston build game.fst` produces `game` (`game.exe` on Windows): one file
containing the Faston runtime and your program, including every `.fst` module
it imports. It needs nothing else installed — no Faston, no Python, no C
compiler. `-o name` picks the output name. Mistakes that the compiler can see
(syntax errors, bad `let`/`const` use, ...) are reported at build time.

**Executable size.** A built program is a Faston runtime plus your sources, so
its size is mostly the runtime: about 16 MB with the `gui` module and 7 MB
without it. `faston build` uses `faston-slim` (the runtime without `gui`, made
by `scripts/build.ps1` / `build.sh` next to `faston`) for any program that
doesn't import `gui`, and prints which runtime it used; `--full` forces the
full one.

When the executable starts, every function is compiled to machine code for the
CPU it is running on, the first time it is called (the whole startup is ~10 ms).
No bytecode is interpreted. `faston run --native file.fst` does the same
without writing an executable.

What the compiler does with your code:

- **Infers types.** For every instruction it works out what is known about each
  local variable and temporary: int, float, bool, None, or "anything". A local
  that only ever holds one kind of number or a bool — loop counters,
  accumulators, flags — lives in a CPU register as a raw 64-bit int, float or
  bool, and arithmetic on it is a single machine instruction (plus an overflow
  check: Faston ints are 64-bit and overflow raises `OverflowError`).
- **Stays dynamic everywhere else.** Values whose type isn't known are handled
  exactly as in the interpreter, with inline machine-code fast paths for
  int/float arithmetic and comparisons, list/tuple indexing, `list.append`,
  attribute reads/writes (inline caches keyed on the class layout) and method
  calls. Strings, dicts, operator methods (`__add__`, ...) and errors go through
  the shared runtime.
- **Calls compiled functions directly.** A call to a Faston function, method
  or class constructor jumps straight to the callee's machine code.
- **Speculates, safely.** `for (i in range(...))` is compiled assuming `range`
  is the built-in, so `i` is a register int. If a program rebinds `range`, that
  call *deoptimizes*: its state is written back and the interpreter finishes it.
- **Shares everything with the interpreter**: values, stack, frames, garbage
  collector, built-ins, exceptions and tracebacks. Error messages and line
  numbers are identical, and the whole test suite runs both ways.

An executable runs on the same kind of system as the `faston` that built it
(build on Windows for Windows, on a Mac for macOS, and so on). The interactive
prompt always uses the interpreter.

`FASTON_JIT_DEBUG=1` prints, per function, which locals went to registers, the
Cranelift IR and the generated machine code.

## The language in one page

Faston keeps Python's semantics: the same numbers (`/` is true division,
`//` floors, `%` follows the divisor's sign), truthiness, `and`/`or` returning
operands, chained comparisons, f-strings, lists, dicts, classes. The syntax
changes are:

| Python                    | Faston                          |
|---------------------------|---------------------------------|
| `if x > 0:` + indent      | `if (x > 0) { ... }`            |
| `elif` / `else`           | `elif (...) { }` / `else { }` (`else if` also works) |
| `while x:`                | `while (x) { }`                 |
| `for k, v in d.items():`  | `for (k, v in d.items()) { }`   |
| `def f(a, b=1):`          | `def f(a, b = 1) { }`           |
| `class B(A):`             | `class B(A) { def init(self) { } }` |
| `__init__`                | `init`                          |
| `super().__init__(x)`     | `super().init(x);`              |
| `with open(p) as f:`      | `with (open(p) as f) { }`       |
| statements end at newline | statements end with `;`         |

Indentation is not significant. `&&`, `||`, `!` work as aliases for
`and`, `or`, `not`.

**Variables.** `let x = 1;` declares a variable, `const MAX = 10;` a constant
(reassigning it is a compile error), and plain `x = 1;` also works. At the top
level of a file these are globals. Inside a function, plain assignment makes a
local of the *whole function*, exactly like Python, even when it happens inside
an `if`/`try`/loop block. `let` is the exception: a `let` inside a block is
scoped to that block. Use `global name;` to assign to a global and
`nonlocal name;` to assign to a variable of an enclosing function.

**Closures and lambdas.** Nested functions capture variables from enclosing
functions, and closures share them, exactly like Python (including late binding
in loops). Lambdas use parentheses instead of Python's colon:

```faston
def make_counter() {
    let n = 0;
    def inc() { nonlocal n; n += 1; return n; }
    return inc;
}
let square = lambda (x) x * x;                        # expression body
let shout = lambda (s) { return s.upper() + "!"; };   # block body
let by_hp = sorted(enemies, lambda (e) e.hp);
```

A `{` right after a lambda's parameters always starts a block body.

**Modules.** `import enemies;` runs `enemies.fst` from the importing file's
directory once and binds it as a module; `from enemies import Slime, spawn;`
binds names directly. Each file has its own globals. Circular imports work like
Python's (the second importer sees a partially initialized module). Built-in
modules (`math`, `random`, `time`) are imported the same way.

**Exceptions** work like Python's, with parentheses around the `except` clause:

```faston
class OutOfMana(Exception) {
    def init(self, need) { Exception.init(self, f"need {need} mana"); self.need = need; }
}

try {
    cast("fireball");
} except (OutOfMana as e) {
    print("can't cast:", e);
} except (KeyError, IndexError) {
    print("bad lookup");
} except {                       # bare except: anything else
    raise;                       # re-raise
} else {
    print("cast ok");
} finally {
    cleanup();
}
raise ValueError("bad input");
```

`finally` runs on every exit (normal, exception, `return`, `break`, `continue`).
Errors raised by built-ins are ordinary exception objects, so
`except (ZeroDivisionError)` and friends work. Built-in classes follow
Python's hierarchy: `BaseException > Exception > ArithmeticError >
ZeroDivisionError`, `LookupError > IndexError / KeyError`, plus `TypeError`,
`ValueError`, `NameError`, `AttributeError`, `RuntimeError > RecursionError`,
`AssertionError`, `ImportError`, `OSError > IOError`, `SyntaxError`,
`NotImplementedError`, `StopIteration`. Subclass them for your own errors.

**Operator methods.** Classes can define `__add__ __sub__ __mul__ __truediv__
__floordiv__ __mod__ __pow__` (and the reflected `__radd__`... versions),
`__and__ __or__ __xor__`, `__neg__`, `__eq__ __ne__ __lt__ __le__ __gt__ __ge__`,
`__str__ __repr__`, `__len__ __bool__`, `__getitem__ __setitem__ __contains__`,
`__iter__` (return a list or other iterable) and `__call__`. They are used by
operators, `print`, f-strings, `str`, `len`, `sorted`, `min`/`max`, `sum`, `in`,
`for` loops and unpacking:

```faston
class Vec2 {
    def init(self, x, y) { self.x = x; self.y = y; }
    def __add__(self, o) { return Vec2(self.x + o.x, self.y + o.y); }
    def __mul__(self, k) { return Vec2(self.x * k, self.y * k); }
    def __eq__(self, o) { return self.x == o.x and self.y == o.y; }
    def __repr__(self) { return f"Vec2({self.x}, {self.y})"; }
}
print(Vec2(1, 2) + Vec2(3, 4) * 2);    # Vec2(7, 10)
```

**Tuples and sets.** `(1, 2)`, `(x,)`, `()` are immutable tuples that can be
dict keys (`grid[(x, y)]`); `return a, b` returns a tuple, and `dict.items()`,
`enumerate` and `zip` produce tuples, like Python. `{1, 2, 3}` is a set (`{}`
is still an empty dict; use `set()`), with `| & - ^` and `<=` (subset),
plus methods `add remove discard pop clear copy update union intersection
difference symmetric_difference issubset issuperset isdisjoint`.

**Keyword arguments**, `*args` and `**kwargs` work as in Python:

```faston
def spawn(name, x = 0, y = 0, hp = 100) { ... }
spawn("boss", hp=500, x=3);
def log(msg, *values, **opts) { ... }
log("hit", 1, 2, color="red");
spawn(*position, **stats);
sorted(enemies, key=lambda (e) e.hp, reverse=True);
print(a, b, sep=", ", end="");
```

**Bitwise operators** `& | ^ ~ << >>` (and `&= |= ^=`) work on ints, handy for
flags and tile masks.

**Everything else that works:**

- `a if cond else b`, `0 <= x < 10`, `x in xs`, `x not in xs`
- multiple assignment and unpacking: `a, b = b, a;` `let x, y = point;`
  `return lo, hi;` `for (k, v in pairs) { }`
- comprehensions: `[x * x for x in xs if x > 0]`, `{k: v for k, v in pairs}`,
  `{x % 3 for x in xs}`, and generator arguments like `sum(x for x in xs)`
  (evaluated eagerly)
- slicing on lists, tuples and strings: `xs[1:3]`, `s[::-1]`, `xs[-2:]`
- f-strings with format specs: `f"{fps:.1f}"`, `f"{n:>5}"`, `f"{gold:,}"`,
  `f"{ratio:.0%}"`, `f"{n:05d}"`, `f"{color:x}"`
- `break`, `continue`, `pass;`, `assert cond, "msg";`
- classes: methods, single inheritance, calling parent methods, fields added
  at any time, bound methods (`f = obj.method;`), `isinstance(obj, Class)`
- functions are values; default arguments are evaluated when the `def` runs
  (like Python), so `def f(hp = MAX_HP, items = [])` works
- comments start with `#`; hex literals `0xFF`; underscores `1_000_000`

- `x is None`, `a is not b`; `del name`, `del d[key]`, `del xs[1:3]`,
  `del obj.attr`; slice assignment `xs[1:3] = [7, 8, 9]`

**Generators.** A function containing `yield` returns a generator; `for`
loops, `list()`, `sum()`, `next()`, `in` and friends consume it lazily, so
infinite generators work. `yield from it;` yields everything from another
iterable, and `gen.send(v)` resumes it with a value. Classes can implement
Python's iterator protocol with `__iter__` / `__next__`.

```faston
def fib() {
    let a, b = 0, 1;
    while (True) { yield a; a, b = b, a + b; }
}
for (n in fib()) {
    if (n > 100) { break; }
    print(n);
}
```

**with** calls `__enter__`, runs the block, and always calls `__exit__` (on
exceptions, `return` and `break` too); several managers can be listed.

```faston
with (open("scores.json") as f) {
    let scores = json.load(f);
}
```

**Decorators** work on functions and classes. Class bodies can hold methods,
`@staticmethod`, `@classmethod`, `@property` (with `@name.setter`) and class
attributes (`count = 0;`), and methods can call `super().method(...)`:

```faston
class Temperature {
    unit = "C";
    def init(self, c) { self._c = c; }
    @property
    def celsius(self) { return self._c; }
    @celsius.setter
    def celsius(self, v) {
        if (v < -273.15) { raise ValueError("too cold"); }
        self._c = v;
    }
}
```

**Built-in functions:** `print input len range str repr int float bool list
tuple set dict type isinstance issubclass abs min max sum round divmod pow
sorted reversed chr ord hex oct bin format clock exit map filter enumerate zip
any all callable iter next getattr setattr hasattr id hash open property
staticmethod classmethod`

Keyword arguments of built-ins: `print(sep=, end=, file=, flush=)`,
`sorted(key=, reverse=)`, `list.sort(key=, reverse=)`, `min`/`max(key=,
default=)`, `enumerate(start=)`, `open(mode=, encoding=)`, `dict(**kw)`,
`int(text, base)`. `map`, `filter`, `enumerate` and `zip` return lists rather
than lazy iterators. `isinstance(x, (A, B))` accepts a tuple of classes.

**Methods:**
- list: `append extend pop insert remove index count clear copy reverse sort`
- tuple: `count index`
- str: `upper lower casefold title capitalize swapcase strip lstrip rstrip split
  rsplit splitlines partition rpartition join replace startswith endswith find
  rfind index rindex count removeprefix removesuffix zfill center ljust rjust
  expandtabs format isdigit isalpha isalnum isspace isupper islower istitle
  isnumeric isdecimal isidentifier isascii`
- dict: `keys values items get pop popitem setdefault update clear copy`
- set: `add remove discard pop clear copy update union intersection difference
  symmetric_difference issubset issuperset isdisjoint`
- file: `read readline readlines write writelines seek tell flush close`, plus
  `name mode closed`; iterating a file gives its lines
- generator: `send close`

**Files** are UTF-8 text: `open(path, "r" | "w" | "a" | "x" [+ "+"])`.
Reading turns `"\r\n"` into `"\n"`; writing writes exactly what you give it,
so programs produce the same files on every OS. Missing files raise
`FileNotFoundError` (and friends, all subclasses of `OSError`). Files still
open when the program ends are flushed.

**Modules** (`import math, os, json;`):
- `math`: `pi e tau inf nan sqrt sin cos tan asin acos atan atan2 exp log
  log10 log2 pow floor ceil trunc hypot degrees radians isnan isinf fabs`
- `random`: `random randint uniform choice shuffle seed`
- `time`: `time perf_counter sleep`
- `os`: `getcwd chdir listdir mkdir makedirs rmdir remove rename replace
  getenv putenv unsetenv system walk cpu_count getpid environ name sep linesep`
- `os.path`: `join exists isfile isdir isabs basename dirname split splitext
  abspath normpath realpath relpath expanduser getsize getmtime`
- `sys`: `argv path exit platform version executable maxsize stdin stdout
  stderr`
- `json`: `dumps loads dump load` (`indent=`, `sort_keys=`, `ensure_ascii=`,
  `separators=`; Python's output format and error messages)
- `shutil`: `copyfile copy copytree move rmtree which`
- `re`: `compile search match fullmatch findall finditer sub subn split escape`
  and the flags `I M S X`; Python syntax including named groups,
  look-around and backreferences; match objects have `group groups groupdict
  start end span` and `m[1]`; `sub` takes `\1` / `\g<name>` templates or a
  function
- `datetime`: `date time datetime timedelta` with `now today fromtimestamp
  fromisoformat strptime strftime isoformat timestamp replace weekday`,
  arithmetic (`dt + timedelta(hours=3)`, `d2 - d1`) and comparisons (no time
  zones)
- `collections`: `Counter defaultdict deque namedtuple OrderedDict`
- `subprocess`: `run check_output call check_call getoutput getstatusoutput`,
  `CompletedProcess`, `CalledProcessError`, `TimeoutExpired`, `PIPE STDOUT
  DEVNULL` (text output; `capture_output=`, `check=`, `cwd=`, `shell=`,
  `input=`, `timeout=`, `env=`)
- `gui`: windows, widgets and a drawing canvas (below)

`datetime`, `collections` and `subprocess` are written in Faston (see
`src/stdlib/`) and built into the executable.

Program arguments follow the file: `faston game.fst --level 3` gives
`sys.argv == ["game.fst", "--level", "3"]`.

**Strings** also come raw (`r"\d+"`, `r"C:\path"`: backslashes kept) and
triple-quoted (`"""..."""`, `'''...'''`, `f"""..."""`), like Python's.

### Packages and projects

`import game.enemies;` loads `game/enemies.fst` and binds `game`;
`from game.enemies import Slime;` and `import game.enemies as en;` work too. A
package is a folder; an `__init__.fst` in it runs when the package is imported
(and its top-level names become the package's attributes), but it's optional.
Imports look in the importing file's folder, then in `sys.path`: the script's
folder, the project root (the folder with `faston.toml`), the project's
installed packages (`.faston/packages`), then `FASTON_PATH`. Programs can add
folders to `sys.path`.

A project is a folder with a `faston.toml`:

```toml
[package]
name = "mygame"
version = "0.1.0"
main = "main.fst"

[dependencies]
mathx = "https://github.com/someone/mathx"   # a git repository
utils = "../utils"                           # or a folder
```

```sh
faston new mygame          # create a project (faston new mylib --lib: a library)
faston run                 # run the main file
faston add <git url | folder> [name]   # install a package and record it
faston install             # install everything in faston.toml (after a clone)
faston remove name         # uninstall
faston build               # a standalone executable named after the project
```

Packages are installed into `.faston/packages/<name>` (git clones need `git`
on your PATH), and a package's own dependencies are installed too. A library
is a repository with an `__init__.fst` at its root (`faston new NAME --lib`
makes one). `faston build` bundles every module and package the program
imports.

### GUI

`gui.run(draw)` opens a window and calls `draw(ui)` every frame (egui's
immediate mode: there are no widget objects to keep; each widget returns what
the user did). Options: `title=`, `width=`, `height=`, `dark=`, `resizable=`,
`animate=True` (redraw continuously, for games), and `left=` / `right=` /
`top=` / `bottom=` functions that draw side panels. See
[examples/gui_demo.fst](examples/gui_demo.fst).

- text: `ui.label(text, size=, color=, bold=, italic=)`, `heading`,
  `monospace`, `hyperlink(url, text)`, `separator()`, `space(px)`,
  `image(path, width=, height=)`
- input: `button(text)` → clicked?, `checkbox(text, value)`, `radio(text,
  selected)`, `selectable(text, selected)`, `slider(text, value, min, max)`,
  `drag(text, value, speed=)`, `textbox(text, hint=, password=)`,
  `textarea(text, rows=)`, `combo(label, options, selected)`, `color(color)`,
  `progress(fraction)`, `spinner()`; each returns the new value
- layout (each takes a function that gets a nested `ui`): `horizontal`,
  `vertical`, `group`, `indent`, `scroll(fn, height=)`, `collapsing(title,
  fn)`, `columns(n, fn)` (fn gets a list of uis), `window(title, fn)`
- `ui.canvas(width, height, background=)` → `c.rect(x, y, w, h, color,
  fill=, radius=)`, `c.circle(x, y, r, color)`, `c.line(...)`,
  `c.polygon(points, color)`, `c.text(x, y, text, color, size=, align=)`,
  `c.image(path, x, y)`, `c.clear(color)`, `c.mouse()`, `c.clicked()`,
  `c.down()`, `c.width`, `c.height`
- input state: `ui.key_down("left")`, `ui.key_pressed("space")`,
  `ui.mouse()`, `ui.mouse_down()`, `ui.clicked()`, `ui.dt()`, `ui.time()`,
  `ui.fps()`; `ui.close()`, `ui.set_dark(bool)`
- colors: names (`"red"`, `"orange"`, ...), `"#rgb"`, `"#rrggbb"`,
  `"#rrggbbaa"`, `(r, g, b)` / `(r, g, b, a)`, or `gui.rgb(r, g, b, a)`

Errors raised inside `draw` close the window and come out of `gui.run`. One
`gui.run` per program (use `ui.window` for more windows). Not yet available
on macOS (the window needs the main thread there).

**Uncaught exceptions** print a Python-style traceback (from where the
exception was raised) and exit with status 1:

```
Traceback (most recent call last):
  File "game.fst", line 3, in <module>
  File "game.fst", line 2, in g
  File "game.fst", line 1, in f
TypeError: unsupported operand type(s) for +: 'str' and 'int'
```

### Differences from Python (current limitations)

- No `async`, no `match`, no binary files / `bytes` yet.
- Generator expressions in parentheses are only allowed as call arguments
  (`sum(x * x for x in xs)`), and are evaluated eagerly.
- Generator functions run in the interpreter even in native mode (the code
  that loops over them is still compiled). A closure created inside a
  generator keeps the values its variables had when the generator last paused.
- `del` works on globals, items, slices and attributes, not on function locals.
- A class attribute changed after a subclass was created isn't seen by the
  subclass (methods and attributes are copied when the subclass is defined).
- Parameters after `*args` (keyword-only parameters) aren't supported.
- Integers are 64-bit; overflow raises `OverflowError` instead of growing.
- `type(x)` returns the type's name as a string (`"int"`, `"Player"`).
- Sets keep insertion order, so they may print in a different order than
  Python's.
- Instances hash by identity (no `__hash__`), and `==` between two *lists* of
  objects compares the objects by identity (`in`, `index`, `count` and
  `remove` do use `__eq__`).
- A nested function only sees an enclosing function's `let` variable if it was
  declared before the nested `def`/`lambda` (plain assignments are found
  anywhere in the function).
- No relative imports (`from . import x`) and no `from m import *`.
- `datetime` objects have no time zones, and dates / namedtuples aren't
  hashable by value (instances hash by identity), so they're poor dict keys.

## Performance

Measured on the bundled benchmarks (`bench/run.sh`, best of 3 runs on a noisy
2-core cloud VM; treat the numbers as rough). "Native" includes compile time.

| Benchmark   | What it does                                  | Interpreted | Native | CPython 3.13 | Native vs CPython |
|-------------|-----------------------------------------------|------------:|-------:|-------------:|------------------:|
| `fib`       | recursive calls, fib(32)                      | 0.22 s      | 0.13 s | 0.41 s       | 3.2×              |
| `loop`      | 30M-iteration integer loop with `%` and `if`  | 0.51 s      | 0.09 s | 1.89 s       | 21×               |
| `objects`   | 2M method calls that allocate new objects     | 0.29 s      | 0.21 s | 0.48 s       | 2.3×              |
| `lists`     | list comprehensions with a filter             | 0.30 s      | 0.13 s | 0.74 s       | 5.7×              |
| `particles` | game-style update loop over 1,000 objects     | 0.13 s      | 0.09 s | 0.22 s       | 2.4×              |
| `strings`   | f-strings + dict counting, 1M words           | 0.31 s      | 0.28 s | 0.37 s       | 1.3×              |

Numeric code whose types the compiler can infer (loop counters, accumulators)
becomes plain machine code: 5–20× faster than CPython 3.13 and about 4–6×
faster than the interpreter. Dynamic code (attributes, method calls, values of
unknown type) gains less, since every operation still has to check types at run
time. Code dominated by string building and dict lookups is only a little
faster than CPython, since that work is already optimized C there.

### How it works

```
source ─► lexer ─► single-pass compiler ─► bytecode ─┬─► interpreter ─┐
                                                     └─► Cranelift ───┴─► shared runtime + GC
                                                         (machine code)
```

- **Values** are 16-byte `Copy` types: `None`, `bool`, `int`, `float` are
  stored inline; everything else is a pointer into a garbage-collected heap.
  Copying a value never touches a reference count.
- **Garbage collector**: mark-and-sweep, so reference cycles (enemies pointing
  at each other, parent/child links) are freed. It only runs at safe points
  (calls and loop back-edges) and recycles object memory through a free list.
- **One value stack** holds every frame's locals and temporaries; a call's
  arguments already sit where the callee's locals start, so calls copy nothing.
- **Superinstructions**: a peephole pass fuses common sequences
  (`if (i < n)` → one compare-and-branch on two locals, `total += i` → one
  instruction, `i % 3` / `n - 1` / `x * dt` → one instruction, `self.x` → one
  instruction).
- **Inverted loops**: `for` loops test for the next item at the bottom, so each
  iteration costs one dispatch for the loop itself.
- **Jump threading**: `if (a or b)` jumps straight into the body when `a` is
  true instead of materializing and re-testing a boolean; a final pass then
  re-fuses and compacts the code.
- **Inline caches**: attribute reads/writes and method calls rewrite
  themselves after the first run to remember the class layout they saw, so
  `self.x` becomes a type check plus an array index instead of a hash lookup.
- **Fixed field slots**: each class assigns field names to slot numbers, so
  instances store fields in a flat array.
- **Loops over `range` and lists** keep their iteration state in hidden local
  slots, with no iterator objects allocated.
- **Closures** use Lua-style upvalues: a captured variable stays in its stack
  slot (full speed for the enclosing function) and is moved to the heap only
  when its scope ends. Functions that capture nothing stay plain functions.
- **Modules** each have their own globals array; a frame caches a pointer to
  it, so global access costs the same in every file.
- **Exceptions** cost nothing until raised: `try` pushes a handler record,
  and the call stack is captured only when an exception is actually thrown.
- **Operator methods** live on the slow paths only: int/float arithmetic and
  comparisons never check for them.
- **Generators**: calling a generator function packages the new frame into a
  generator object; `yield` copies the frame's stack slots (and active `try`
  blocks) into it, and resuming copies them back and continues where it left
  off.

## Project layout

```
src/
  main.rs       CLI, REPL
  lexer.rs      tokens; f-strings are desugared here
  compiler.rs   single-pass compiler + peephole fusion
  bytecode.rs   instruction set, fusion rules
  vm.rs         interpreter loop, calls, inline caches
  jit.rs        native code generator: type inference, Cranelift IR, runtime helpers
  bundle.rs     `faston build`: standalone executables
  value.rs      Value, heap objects, garbage collector, dicts
  ops.rs        operator semantics (Python-compatible)
  builtins.rs   built-in functions, methods, math/random/time
  fileio.rs     open(), file objects, standard streams
  osmod.rs      sys, os, os.path, shutil
  json.rs       json module
  re.rs         re module (fancy-regex)
  natives.rs    native helpers for the Faston-written modules
  stdlib/*.fst  datetime, collections, subprocess (built into the executable)
  modules.rs    finding modules and packages (disk or a built program's bundle)
  pkg.rs        faston.toml, faston new / add / install / remove
  gui.rs        gui module (egui): windows, widgets, canvas
  fx.rs         fast hash function
editors/vscode/ the VS Code extension
tests/
  cases/*.fst   programs + expected output (.out); `_*.fst` are helper modules
  errors/*.fst  programs that must fail with the message in .err
  run.rs        cargo test runner (every test interpreted and native)
  native.sh     run the tests compiled to native code
  fuzz.py       random programs run in both Faston and CPython
examples/       tour.fst, life.fst, arena/ (multi-file), physics.fst (operators,
                exceptions, tuples, keyword arguments), gui_demo.fst (gui),
                stdlib_demo.fst (re, collections, datetime, json, subprocess)
scripts/        build.ps1 / build.sh: build faston and faston-slim
bench/          benchmarks with CPython equivalents
```

## Roadmap ideas

An embedding API so a game engine (Rust, C, or a Tauri/JS host) can run Faston
scripts and register its own functions (`Vm::call` and native functions already
exist internally); a package manager and import search path; `bytes` and
binary files; `subprocess`, `datetime`, `re`, networking; native file dialogs
for the gui; arbitrary-size integers. For the native compiler: type feedback (record the types each
operation actually sees, then compile a specialized version), unboxed float
fields, and caching compiled code inside built executables.
