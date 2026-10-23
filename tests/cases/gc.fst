# Lots of garbage, including reference cycles, must be collected while
# live data stays intact.
class Node {
    def init(self, v) { self.v = v; self.peer = None; self.data = [v, v + 1, str(v)]; }
}

def churn(rounds) {
    let keep = [];
    for (i in range(rounds)) {
        let a = Node(i);
        let b = Node(i + 1);
        a.peer = b;
        b.peer = a;              # cycle
        let tmp = {"k": [a, b], "s": f"node{i}"};
        if (i % 50000 == 0) { keep.append(a); }
    }
    return keep;
}

let kept = churn(400000);
print(len(kept), [n.v for n in kept], kept[3].peer.peer.v, kept[3].data);

let big = [];
for (i in range(200000)) { big.append([i]); }
let s = 0;
for (x in big) { s += x[0]; }
print(s);
