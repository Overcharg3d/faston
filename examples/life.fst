# Conway's Game of Life on a wrapping grid.
const W = 24;
const H = 10;

def make_grid() {
    return [[False] * W for _ in range(H)];
}

def neighbours(grid, r, c) {
    let n = 0;
    for (dr in [-1, 0, 1]) {
        for (dc in [-1, 0, 1]) {
            if (dr == 0 and dc == 0) { continue; }
            if (grid[(r + dr) % H][(c + dc) % W]) { n += 1; }
        }
    }
    return n;
}

def step(grid) {
    let next = make_grid();
    for (r in range(H)) {
        for (c in range(W)) {
            let n = neighbours(grid, r, c);
            next[r][c] = n == 3 or (grid[r][c] and n == 2);
        }
    }
    return next;
}

def show(grid, gen) {
    print(f"generation {gen}:");
    for (row in grid) {
        print("".join(["#" if cell else "." for cell in row]));
    }
}

let grid = make_grid();
# a glider and a blinker
for (r, c in [[0, 1], [1, 2], [2, 0], [2, 1], [2, 2], [5, 10], [5, 11], [5, 12]]) {
    grid[r][c] = True;
}

for (gen in range(4)) {
    show(grid, gen);
    grid = step(grid);
}
