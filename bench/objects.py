class Vec2:
    def __init__(self, x, y): self.x = x; self.y = y
    def add(self, o): return Vec2(self.x + o.x, self.y + o.y)
def main():
    p = Vec2(0, 0); v = Vec2(1, 2)
    for i in range(2000000): p = p.add(v)
    print(p.x, p.y)
main()
