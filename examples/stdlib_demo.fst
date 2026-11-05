# A tour of the standard library: re, collections, datetime, subprocess, json.
# Run: faston examples/stdlib_demo.fst
import re, json, subprocess;
from collections import Counter, defaultdict, deque, namedtuple;
from datetime import date, datetime, timedelta;

let log = r"""
2026-11-03 09:12:44 INFO  user=ann  action=login
2026-11-03 09:15:02 ERROR user=bob  action=upload size=4096
2026-11-04 18:01:10 INFO  user=ann  action=upload size=128
2026-11-04 18:05:59 WARN  user=cara action=login
2026-11-05 07:30:00 ERROR user=bob  action=login
""";

# ---- regular expressions: pull structured data out of text
let Entry = namedtuple("Entry", "when level user action");
let pattern = re.compile(r"(?P<ts>\S+ \S+) (?P<level>\w+)\s+user=(?P<user>\w+)\s+action=(?P<action>\w+)");
let entries = [];
for (m in pattern.finditer(log)) {
    let when = datetime.strptime(m.group("ts"), "%Y-%m-%d %H:%M:%S");
    entries.append(Entry(when, m.group("level"), m.group("user"), m.group("action")));
}
print(f"parsed {len(entries)} entries; first: {entries[0]}");

# ---- collections: count and group
let levels = Counter([e.level for e in entries]);
print("by level:", levels.most_common());
let by_user = defaultdict(list);
for (e in entries) { by_user[e.user].append(e.action); }
print("actions by user:", dict(by_user));
let recent = deque(maxlen=2);
for (e in entries) { recent.append(e.user); }
print("last two users:", list(recent));

# ---- datetime: durations and formatting
let first = entries[0].when;
let last = entries[-1].when;
print("log covers", last - first, "| from", first.strftime("%A %d %B"), "to", last.strftime("%a %H:%M"));
let deadline = date(2026, 12, 24);
print("days until", deadline.isoformat(), ":", (deadline - date(2026, 11, 5)).days);
print("in 90 minutes it will be", (datetime(2026, 11, 5, 23, 0) + timedelta(minutes=90)).strftime("%Y-%m-%d %H:%M"));

# ---- re.sub with a function: redact user names
print(re.sub(r"user=(\w+)", lambda (m) "user=" + m.group(1)[0] + "***", log.strip().splitlines()[1]));

# ---- json: save a report
let report = {"levels": dict(levels), "users": sorted(by_user.keys()), "span_hours": (last - first).total_seconds() / 3600};
print(json.dumps(report, indent=2));

# ---- subprocess: run another program and read its output
let r = subprocess.run("echo hello from a child process", shell=True, capture_output=True);
print("child said:", r.stdout.strip(), "| exit code", r.returncode);
