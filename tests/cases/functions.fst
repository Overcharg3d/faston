def fib(n) {
    if (n < 2) { return n; }
    return fib(n - 1) + fib(n - 2);
}
print(fib(20));

def greet(name, greeting = "Hello", punct = "!") {
    return f"{greeting}, {name}{punct}";
}
print(greet("Ada"), greet("Bob", "Hi"), greet("Cy", "Yo", "?"));

def minmax(xs) {
    return min(xs), max(xs);
}
let lo, hi = minmax([3, 9, -2, 7]);
print(lo, hi, minmax([5]));

# globals: read freely, write with `global`
let counter = 0;
def bump() {
    global counter;
    counter += 1;
}
bump(); bump(); bump();
print(counter);

# plain assignment inside a function makes a local
let shadow = "global";
def local_assign() {
    shadow = "local";
    return shadow;
}
print(local_assign(), shadow);

# functions are values
def twice(f, x) { return f(f(x)); }
def inc(x) { return x + 1; }
print(twice(inc, 5));
let ops = [inc, abs, str];
print([f(-3) for f in ops]);

# early return from nested loops
def find(grid, target) {
    for (r in range(len(grid))) {
        for (c in range(len(grid[r]))) {
            if (grid[r][c] == target) { return [r, c]; }
        }
    }
    return None;
}
print(find([[1, 2], [3, 4]], 4), find([[1]], 9));

# deep recursion within the limit
def depth(n) { if (n == 0) { return 0; } return 1 + depth(n - 1); }
print(depth(3000));
