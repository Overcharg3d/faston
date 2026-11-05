# packages: dotted imports, `as`, namespace packages, lazy submodules, sys.path
import _pkgdemo;
import _pkgdemo.shapes;
import _pkgdemo.shapes as sh;
from _pkgdemo.sub.deep import where as deep_where;
from _pkgdemo import util;
import sys;

print(_pkgdemo.describe(), _pkgdemo.VERSION);
let c = _pkgdemo.shapes.Circle(2);
print(c.area(), c.twice().r, sh.Circle(1).area(), sh is _pkgdemo.shapes);
print(deep_where(), util.double(21), _pkgdemo.sub.deep.where());
print(type(sys.path), len(sys.path) >= 1);
try {
    import _pkgdemo.nothing;
} except (ModuleNotFoundError as e) {
    print("ModuleNotFoundError:", "_pkgdemo.nothing" in str(e));
}
try {
    import _pkgdemo.util.more;
} except (ImportError as e) {
    print("not a package:", "is not a package" in str(e));
}
