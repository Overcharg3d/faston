# collections: Counter, defaultdict, deque, namedtuple, OrderedDict
import _faston;

# dicts keep insertion order already
OrderedDict = dict;

# Dict-like helpers shared by Counter and defaultdict (they store data in self._d).
class _DictLike {
    def __setitem__(self, key, value) { self._d[key] = value; }
    def __delitem__(self, key) { del self._d[key]; }
    def __contains__(self, key) { return key in self._d; }
    def __len__(self) { return len(self._d); }
    def __iter__(self) { return list(self._d.keys()); }
    def __bool__(self) { return len(self._d) > 0; }
    def keys(self) { return self._d.keys(); }
    def values(self) { return self._d.values(); }
    def items(self) { return self._d.items(); }
    def get(self, key, default = None) { return self._d.get(key, default); }
    def pop(self, key, *default) {
        if (key in self._d or not default) { return self._d.pop(key); }
        return default[0];
    }
    def setdefault(self, key, default = None) { return self._d.setdefault(key, default); }
    def clear(self) { self._d.clear(); }
}

class Counter(_DictLike) {
    def init(self, iterable = None, **kwargs) {
        self._d = {};
        self.update(iterable, **kwargs);
    }

    def update(self, iterable = None, **kwargs) {
        if (iterable is not None) {
            if (type(iterable) == "dict" or isinstance(iterable, Counter)) {
                for (k, v in iterable.items()) { self._d[k] = self._d.get(k, 0) + v; }
            } else {
                for (x in iterable) { self._d[x] = self._d.get(x, 0) + 1; }
            }
        }
        for (k, v in kwargs.items()) { self._d[k] = self._d.get(k, 0) + v; }
    }

    def subtract(self, iterable = None, **kwargs) {
        if (iterable is not None) {
            if (type(iterable) == "dict" or isinstance(iterable, Counter)) {
                for (k, v in iterable.items()) { self._d[k] = self._d.get(k, 0) - v; }
            } else {
                for (x in iterable) { self._d[x] = self._d.get(x, 0) - 1; }
            }
        }
        for (k, v in kwargs.items()) { self._d[k] = self._d.get(k, 0) - v; }
    }

    # missing keys count as 0
    def __getitem__(self, key) { return self._d.get(key, 0); }

    def most_common(self, n = None) {
        let items = sorted(self._d.items(), key=lambda (kv) -kv[1]);
        return items if n is None else items[:n];
    }

    def elements(self) {
        let out = [];
        for (k, v in self._d.items()) {
            for (_ in range(v)) { out.append(k); }
        }
        return out;
    }

    def total(self) { return sum(self._d.values()); }
    def copy(self) { return Counter(self._d); }

    def _combine(self, other, pick) {
        let out = Counter();
        for (k in list(self._d.keys()) + [k for k in other._d.keys() if k not in self._d]) {
            let v = pick(self._d.get(k, 0), other._d.get(k, 0));
            if (v > 0) { out._d[k] = v; }
        }
        return out;
    }
    def __add__(self, other) { return self._combine(other, lambda (a, b) a + b); }
    def __sub__(self, other) { return self._combine(other, lambda (a, b) a - b); }
    def __or__(self, other) { return self._combine(other, max); }
    def __and__(self, other) { return self._combine(other, min); }

    def __eq__(self, other) {
        if (not isinstance(other, Counter)) { return False; }
        for (k in list(self._d.keys()) + list(other._d.keys())) {
            if (self[k] != other[k]) { return False; }
        }
        return True;
    }

    def __repr__(self) {
        if (not self._d) { return "Counter()"; }
        let parts = [repr(k) + ": " + repr(v) for k, v in self.most_common()];
        return "Counter({" + ", ".join(parts) + "})";
    }
}

class defaultdict(_DictLike) {
    def init(self, default_factory = None, data = None) {
        self.default_factory = default_factory;
        self._d = {} if data is None else dict(data);
    }

    # a missing key gets default_factory() stored and returned
    def __getitem__(self, key) {
        if (key in self._d) { return self._d[key]; }
        if (self.default_factory is None) { raise KeyError(repr(key)); }
        let v = self.default_factory();
        self._d[key] = v;
        return v;
    }

    def copy(self) { return defaultdict(self.default_factory, self._d); }
    def __eq__(self, other) {
        if (isinstance(other, defaultdict)) { return self._d == other._d; }
        return self._d == other;
    }
    def __repr__(self) { return "defaultdict(" + repr(self.default_factory) + ", " + repr(self._d) + ")"; }
}

# Fast appends and pops at both ends.
class deque {
    def init(self, iterable = None, maxlen = None) {
        self._items = [];
        self._head = 0;
        self.maxlen = maxlen;
        if (iterable is not None) { self.extend(iterable); }
    }

    def _compact(self) {
        if (self._head > 32 and self._head * 2 > len(self._items)) {
            self._items = self._items[self._head:];
            self._head = 0;
        }
    }

    def append(self, x) {
        self._items.append(x);
        if (self.maxlen is not None and len(self) > self.maxlen) { self.popleft(); }
    }

    def appendleft(self, x) {
        if (self._head > 0) {
            self._head -= 1;
            self._items[self._head] = x;
        } else {
            self._items.insert(0, x);
        }
        if (self.maxlen is not None and len(self) > self.maxlen) { self.pop(); }
    }

    def pop(self) {
        if (len(self) == 0) { raise IndexError("pop from an empty deque"); }
        return self._items.pop();
    }

    def popleft(self) {
        if (len(self) == 0) { raise IndexError("pop from an empty deque"); }
        let x = self._items[self._head];
        self._items[self._head] = None;
        self._head += 1;
        self._compact();
        return x;
    }

    def extend(self, items) {
        for (x in items) { self.append(x); }
    }

    def extendleft(self, items) {
        for (x in items) { self.appendleft(x); }
    }

    def rotate(self, n = 1) {
        let k = len(self);
        if (k == 0) { return; }
        n = n % k;
        let items = self._items[self._head:];
        self._items = items[k - n:] + items[:k - n];
        self._head = 0;
    }

    def clear(self) {
        self._items = [];
        self._head = 0;
    }

    def count(self, x) { return self._items[self._head:].count(x); }
    def index(self, x) { return self._items[self._head:].index(x); }
    def remove(self, x) {
        let items = self._items[self._head:];
        items.remove(x);
        self._items = items;
        self._head = 0;
    }
    def reverse(self) {
        let items = self._items[self._head:];
        items.reverse();
        self._items = items;
        self._head = 0;
    }
    def copy(self) { return deque(self._items[self._head:], self.maxlen); }

    def __len__(self) { return len(self._items) - self._head; }
    def __bool__(self) { return len(self) > 0; }
    def __iter__(self) { return self._items[self._head:]; }
    def __contains__(self, x) { return x in self._items[self._head:]; }
    def __getitem__(self, i) {
        let k = len(self);
        if (i < 0) { i += k; }
        if (i < 0 or i >= k) { raise IndexError("deque index out of range"); }
        return self._items[self._head + i];
    }
    def __setitem__(self, i, v) {
        let k = len(self);
        if (i < 0) { i += k; }
        if (i < 0 or i >= k) { raise IndexError("deque index out of range"); }
        self._items[self._head + i] = v;
    }
    def __eq__(self, other) {
        return isinstance(other, deque) and self._items[self._head:] == other._items[other._head:];
    }
    def __repr__(self) {
        let body = repr(self._items[self._head:]);
        if (self.maxlen is None) { return "deque(" + body + ")"; }
        return "deque(" + body + ", maxlen=" + str(self.maxlen) + ")";
    }
}

# A class for tuples with named fields: Point = namedtuple("Point", "x y");
def namedtuple(typename, field_names, defaults = None) {
    let fields = field_names.replace(",", " ").split() if type(field_names) == "str" else list(field_names);
    let defaults_map = {};
    if (defaults is not None) {
        let defs = list(defaults);
        let first = len(fields) - len(defs);
        for (i in range(len(defs))) { defaults_map[fields[first + i]] = defs[i]; }
    }
    let _NT = None;
    class _NT {
        def init(self, *args, **kwargs) {
            if (len(args) > len(fields)) {
                raise TypeError(f"{typename}() takes {len(fields)} arguments but {len(args)} were given");
            }
            let values = list(args);
            for (name in fields[len(args):]) {
                if (name in kwargs) {
                    values.append(kwargs.pop(name));
                } elif (name in defaults_map) {
                    values.append(defaults_map[name]);
                } else {
                    raise TypeError(f"{typename}() missing required argument: '{name}'");
                }
            }
            for (name in kwargs) {
                raise TypeError(f"{typename}() got an unexpected keyword argument '{name}'");
            }
            self._values = tuple(values);
            for (i in range(len(fields))) { setattr(self, fields[i], values[i]); }
        }
        def __getitem__(self, i) { return self._values[i]; }
        def __iter__(self) { return list(self._values); }
        def __len__(self) { return len(self._values); }
        def __eq__(self, other) {
            if (isinstance(other, _NT)) { return self._values == other._values; }
            return type(other) == "tuple" and self._values == other;
        }
        def __lt__(self, other) { return self._values < tuple(other); }
        def __repr__(self) {
            let parts = [fields[i] + "=" + repr(self._values[i]) for i in range(len(fields))];
            return typename + "(" + ", ".join(parts) + ")";
        }
        def _asdict(self) { return {fields[i]: self._values[i] for i in range(len(fields))}; }
        def _replace(self, **changes) {
            let vals = [changes.get(f, getattr(self, f)) for f in fields];
            return _NT(*vals);
        }
        def count(self, x) { return self._values.count(x); }
        def index(self, x) { return self._values.index(x); }
    }
    _NT._fields = tuple(fields);
    _faston.set_class_name(_NT, typename);
    return _NT;
}
