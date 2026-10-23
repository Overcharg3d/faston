def main() {
    let total = 0;
    for (r in range(200)) {
        let xs = [i * 2 for i in range(50000) if i % 3 != 0];
        total += len(xs) + xs[len(xs) - 1];
    }
    print(total);
}
main();
