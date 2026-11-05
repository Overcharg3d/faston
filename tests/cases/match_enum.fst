# match statements and enums
class Point {
    def init(self, x, y) { self.x = x; self.y = y; }
}
class Circle {
    __match_args__ = ("radius",);
    def init(self, r) { self.radius = r; }
}

def describe(v) {
    match (v) {
        case 0 { return "zero"; }
        case 1 | 2 | 3 { return "small"; }
        case -1 { return "minus one"; }
        case "hi" | "hello" { return "greeting"; }
        case None { return "nothing"; }
        case True { return "yes"; }
        case [] { return "empty list"; }
        case [x] { return f"one item: {x}"; }
        case [0, *rest] { return f"starts with 0, then {rest}"; }
        case [first, *_, last] { return f"{first}..{last}"; }
        case (a, b) { return f"pair {a} {b}"; }
        case {"type": "user", "name": name} { return f"user {name}"; }
        case {"id": id} { return f"id {id}"; }
        case Point(0, 0) { return "origin"; }
        case Point(x, 0) { return f"on the x axis at {x}"; }
        case Point(x=x, y=y) if x == y { return f"diagonal {x}"; }
        case Point(x, y) { return f"point {x},{y}"; }
        case Circle(r) if r > 10 { return "big circle"; }
        case Circle(radius=r) { return f"circle {r}"; }
        case int(n) if n > 100 { return "big int"; }
        case int() { return "some int"; }
        case str() as s { return f"string {repr(s)}"; }
        case float(f) { return f"float {f}"; }
        case _ { return "something else"; }
    }
}

for (v in [0, 2, -1, "hello", None, True, [], [9], [0, 1, 2], [1, 2, 3, 4], (5, 6), [5, 6, 7],
           {"type": "user", "name": "ann"}, {"id": 7, "x": 1}, {"k": 1},
           Point(0, 0), Point(3, 0), Point(4, 4), Point(1, 2), Circle(20), Circle(2),
           500, 50, "abc", 2.5, {1, 2}]) {
    print(describe(v));
}

# nested patterns and captures
let shapes = [["move", [1, 2]], ["line", Point(1, 1), Point(2, 2)], ["quit"]];
for (cmd in shapes) {
    match (cmd) {
        case ["move", [x, y]] { print("move to", x, y); }
        case ["line", Point(x=x1), Point(x=x2)] { print("line from x", x1, "to", x2); }
        case ["quit"] { print("bye"); }
    }
}

# no case matches: nothing happens
match (42) { case 1 { print("never"); } }
let match = 5;    # still usable as a name
print("match variable", match);

# enums
enum Color { RED, GREEN, BLUE }
enum Status { OK = 200, NOT_FOUND = 404, ALIAS = 200 }
enum Planet {
    MERCURY = 3.303e+23, EARTH = 5.976e+24;
    def heavy(self) { return self.value > 1e24; }
}
print(Color.RED, repr(Color.GREEN), Color.BLUE.name, Color.BLUE.value);
print(list(Color), len(Color), Color.RED in Color, Color(2), Status(404));
print(Status.ALIAS is Status.OK, list(Status), Status.OK == Status.OK, Status.OK != Status.NOT_FOUND);
print(isinstance(Color.RED, Color), type(Color.RED), Planet.EARTH.heavy(), Planet.MERCURY.heavy());
for (c in Color) { print(c.name, c.value); }
let d = {Color.RED: "stop", Color.GREEN: "go"};
print(d[Color.GREEN]);
try { Color(9); } except (ValueError as e) { print("ValueError:", e); }

def light(c) {
    match (c) {
        case Color.RED { return "stop"; }
        case Color.GREEN | Color.BLUE { return "go"; }
    }
}
print(light(Color.RED), light(Color.BLUE));

# Python-style
from enum import Enum, auto;
class Weekday(Enum) {
    MONDAY = auto();
    TUESDAY = auto();
    FRIDAY = 5;
    SATURDAY = auto();
    def is_weekend(self) { return self.value >= 6; }
}
print(list(Weekday), Weekday.SATURDAY.is_weekend(), Weekday.MONDAY.is_weekend());
let enum = "still a name";
print(enum);
