def classify(n) {
    if (n < 0) {
        return "negative";
    } elif (n == 0) {
        return "zero";
    } else if (n < 10) {
        return "small";
    } else {
        return "large";
    }
}
print(classify(-1), classify(0), classify(5), classify(50));

let i = 0;
let seen = [];
while (True) {
    i += 1;
    if (i % 2 == 0) { continue; }
    if (i > 9) { break; }
    seen.append(i);
}
print(seen);

let total = 0;
for (k in range(10, 0, -3)) { total += k; }
print(total, list(range(5)), list(range(2, 8, 2)), list(range(5, 0, -1)), list(range(0)));

def end_marker() { return "|"; }
for (ch in "héllo") { print(ch, end_marker()); }

let d = {"b": 2, "a": 1};
for (key in d) { print(key, d[key]); }
for (k, v in d.items()) { print(f"{k}={v}"); }

# nested loops with break only leaving the inner loop
let pairs = [];
for (x in range(3)) {
    for (y in range(3)) {
        if (y > x) { break; }
        pairs.append([x, y]);
    }
}
print(pairs);

# loop variables are block scoped; outer value untouched
let item = "outer";
for (item2 in [1, 2]) { let item = "inner"; }
print(item);

# while with complex condition
let a = 0;
let b = 10;
while (a < b and not (a == 4)) { a += 1; b -= 1; }
print(a, b);
