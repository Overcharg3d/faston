let name = "main";
import _enemies;
import _enemies;                   # cached: "loading _enemies" prints once
from _enemies import Slime, spawn, MAX_HP;
from math import sqrt;

let pack = spawn(3);
print(len(pack), _enemies.count, pack[0].hit(20), MAX_HP);
let s = Slime(7);
print(s.hit(2), name, _enemies.name, sqrt(16));
print(_enemies.helper_uses_util(), type(_enemies), _enemies);
_enemies.count = 100;              # write an existing module variable
print(_enemies.count, _enemies.spawn(1)[0].hp, _enemies.count);
import _util;
print(_util.VERSION, _util.double(21));

import _circ_a, _circ_b;
print(_circ_a.ax(), _circ_b.bx());
