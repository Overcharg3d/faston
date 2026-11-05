import math;
from _pkgdemo.util import double;

print("init _pkgdemo.shapes");

class Circle {
    def init(self, r) { self.r = r; }
    def area(self) { return round(math.pi * self.r * self.r, 2); }
    def twice(self) { return Circle(double(self.r)); }
}
