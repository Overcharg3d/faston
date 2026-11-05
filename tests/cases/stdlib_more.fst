# re, datetime, collections, subprocess (expected output generated with CPython)
import re, subprocess;
from datetime import date, datetime, timedelta, time;
from collections import Counter, defaultdict, deque, namedtuple, OrderedDict;

# ---- re
let m = re.search(r"(\w+)@(\w+)\.com", "mail ann@example.com now");
print(m.group(), m.group(1), m.groups(), m.span(), m.start(2), m.end());
print(re.findall(r"\d+", "a1 b22 c333"), re.findall(r"(\w)(\d)", "a1 b2"), re.split(r"[,;]\s*", "a, b;c"));
print(re.sub(r"(\w+) (\w+)", r"\2 \1", "hello world"), re.sub(r"\d", lambda (mm) str(int(mm.group()) * 2), "a1b2c3"));
print(re.subn("o", "0", "foo boo"), re.match(r"\d", "a1"), bool(re.match(r"a", "abc")), re.fullmatch(r"\d+", "123x"));
let p = re.compile(r"(?P<year>\d{4})-(?P<month>\d\d)", re.I);
let d = p.search("on 2026-11 we");
print(d.group("year"), d["month"], d.groupdict(), p.pattern, p.groups);
print(re.escape("a.b*c"), re.split(r"(-)", "1-2-3"), [x.group() for x in re.finditer(r"[A-Z]", "aBcDe")]);
print(re.findall(r"^\w", "one\ntwo", re.M), re.search(r"a.b", "a\nb", re.S) is not None, re.sub(r"(?P<w>\w+)", r"<\g<w>>", "hi yo"));
print(re.findall(r"(?<=\$)\d+", "cost $30 and $45"), re.search(r"(\w)\1", "abccd").group());

# ---- datetime
let d1 = date(2024, 2, 28);
print(d1 + timedelta(days=2), d1.weekday(), d1.isoweekday(), d1.toordinal(), date.fromordinal(739000));
print(date(2026, 1, 1) - date(2025, 1, 1), (date(2026, 3, 1) - date(2026, 2, 1)).days, d1.isoformat(), repr(d1));
let dt = datetime(2026, 11, 4, 21, 39, 13, 250000);
print(dt, repr(dt), dt.isoformat(), dt.date(), dt.time(), repr(datetime(2026, 1, 2)));
print(dt + timedelta(hours=3, minutes=30), dt - timedelta(days=400), dt - datetime(2026, 1, 1));
print(dt.strftime("%Y/%m/%d %H:%M:%S.%f %a %A %b %B %j %p %I %y %%"), d1.strftime("%d.%m.%Y"));
print(datetime.strptime("2026-03-05 14:07:09", "%Y-%m-%d %H:%M:%S"), datetime.strptime("5 Mar 2026 2:07 PM", "%d %b %Y %I:%M %p"));
print(datetime.fromisoformat("2026-11-04T21:39:13.5"), date.fromisoformat("2024-02-29"), time(9, 5), time(9, 5, 3, 7));
print(timedelta(days=1, hours=2, minutes=3, seconds=4, microseconds=5), timedelta(seconds=-1), repr(timedelta(hours=1)), timedelta(0));
print(timedelta(hours=36).total_seconds(), timedelta(days=1) / timedelta(hours=1), timedelta(minutes=90) * 2, -timedelta(days=1, seconds=5));
print(datetime(2026, 1, 1) < datetime(2026, 1, 2), date(2026, 1, 1) == date(2026, 1, 1), sorted([date(2026, 5, 1), date(2024, 1, 1)]));
let ts = datetime(2026, 6, 15, 12, 0).timestamp();
print(datetime.fromtimestamp(ts), type(datetime.now().year), date.today() <= date.today());
try {
    date(2026, 2, 30);
} except (ValueError as e) {
    print("ValueError:", e);
}

# ---- collections
let words = "the cat and the hat and the bat".split();
let cnt = Counter(words);
print(cnt, cnt["the"], cnt["zebra"], cnt.most_common(2), len(cnt), "cat" in cnt);
cnt.update(["cat", "cat"]);
print(cnt.most_common(1), cnt.total(), sorted(cnt.elements())[:4], Counter("abca") + Counter("bcd"));
let groups = defaultdict(list);
for (w in words) { groups[len(w)].append(w); }
print(dict(groups), groups[99], len(groups));
let tally = defaultdict(int);
for (ch in "banana") { tally[ch] += 1; }
print(dict(tally));
let q = deque([1, 2, 3]);
q.appendleft(0);
q.append(4);
print(q, q.popleft(), q.pop(), q, len(q), q[0], q[-1]);
q.rotate(1);
print(q, deque(range(10), maxlen=3), list(q));
let Point = namedtuple("Point", "x y");
let pt = Point(3, y=4);
let px, py = pt;
print(pt, pt.x, pt[1], px + py, pt._asdict(), pt._replace(x=10), Point._fields, pt == (3, 4));
let od = OrderedDict();
od["b"] = 1;
od["a"] = 2;
print(od, list(od.keys()));

# ---- subprocess
let r = subprocess.run("echo hello", shell=True, capture_output=True);
print(repr(r.stdout), r.returncode, r.stderr);
print(subprocess.getoutput("echo from getoutput"), subprocess.call("exit 3", shell=True));
try {
    subprocess.run("exit 2", shell=True, check=True);
} except (subprocess.CalledProcessError as e) {
    print("CalledProcessError:", e.returncode);
}
