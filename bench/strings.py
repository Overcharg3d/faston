def main():
    words = []
    for i in range(1000000):
        words.append(f"item{i % 1000}")
    counts = {}
    for w in words:
        counts[w] = counts.get(w, 0) + 1
    total = 0
    for k in counts: total += counts[k] * len(k)
    print(len(counts), total, "-".join(["done"]))
main()
