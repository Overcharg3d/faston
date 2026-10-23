def main() {
    let total = 0;
    for (i in range(30000000)) {
        if (i % 3 == 0) { total += i; }
    }
    print(total);
}
main();
