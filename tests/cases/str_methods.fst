# String methods added to match Python (expected output generated with CPython).
let s = "hello wide world";
print(s.title(), "|", "they're bill's 2nd".title(), "|", "hELLO".capitalize(), "|", "AbC".swapcase());
print("42".zfill(5), "-42".zfill(5), "+7".zfill(4), "abc".zfill(2));
print("[" + "ab".center(7) + "]", "[" + "ab".center(6, "*") + "]", "[" + "abc".ljust(6, ".") + "]", "[" + "abc".rjust(6) + "]");
print("a\nb\r\nc\rd".splitlines(), "x\ny\n".splitlines(True), "".splitlines());
print("key=value=x".partition("="), "key=value=x".rpartition("="), "abc".partition("="), "abc".rpartition("="));
print("a,b,c,d".split(",", 2), "a,b,c,d".rsplit(",", 1), "  a  b c  ".split(None, 1), "  a  b c  ".rsplit(None, 1));
print("a b c".split(maxsplit=1), "a-b-c".rsplit(sep="-", maxsplit=1));
print("banana".find("an"), "banana".rfind("an"), "banana".find("an", 2), "banana".find("an", 2, 4), "banana".index("na"), "banana".rindex("na"));
print("unittest".removeprefix("unit"), "unittest".removesuffix("test"), "x".removeprefix("y"));
print("ABC".isupper(), "AbC".isupper(), "abc".islower(), "a1".isalnum(), "a 1".isalnum(), "123".isnumeric(), "12a".isdecimal());
print("Hello World".istitle(), "Hello world".istitle(), "_x1".isidentifier(), "1x".isidentifier(), "abc".isascii());
print("a\tbc\td".expandtabs(4), "PyThOn".casefold());
print("photo.png".endswith((".jpg", ".png")), "photo.png".startswith(("x", "y")));
print("{} + {} = {}".format(1, 2, 3), "{1}{0}".format("a", "b"), "{name} is {age}".format(name="Ann", age=7));
print("{:>6}|{:<6}|{:^6}|".format("ab", "cd", "ef"), "{:.3f} {:,} {:08.2f} {:x}".format(3.14159, 1234567, -3.5, 255));
print("{0!r} {0!s} {{literal}} {x:{w}}|".format("hi", x="pad", w=6));
let pt = {"x": 3, "y": [10, 20]};
print("{p[x]} {p[y]}".format(p=pt));
print(int("ff", 16), int("0x1f", 0), int("-101", 2), int("0o17", 8), int("z", 36));
print(hex(255), hex(-255), oct(8), bin(5), bin(-5));
print(divmod(17, 5), divmod(-17, 5), divmod(7.5, 2), pow(2, 10), pow(3, 4, 5), pow(-2, 3, 7));
print(list(reversed([1, 2, 3])), list(reversed("abc")));
let d = dict(a=1, b=2);
print(d, dict([("x", 1), ("y", 2)]), dict({"k": 9}), dict());
print(d.setdefault("a", 100), d.setdefault("c", 3), d);
print(d.popitem(), d);
d.update([("z", 26)], q=17);
print(d);
class P {
    def init(self) { self.v = 5; }
}
let p = P();
print(getattr(p, "v"), getattr(p, "missing", "default"), hasattr(p, "v"), hasattr(p, "w"));
setattr(p, "w", 42);
print(p.w, hash(5), hash("a") == hash("a"), id(p) == id(p), id(p) == id(P()));
class Q(P) { }
print(issubclass(Q, P), issubclass(P, Q), issubclass(Q, (Exception, P)));
