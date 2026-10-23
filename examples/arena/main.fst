# Run with: faston examples/arena/main.fst
from entities import Entity, make_spawner, ARENA;
import random;

random.seed(3);
let hero = Entity("hero", ARENA / 2, ARENA / 2);
let spawn_slime = make_spawner("slime");
let enemies = [spawn_slime(random.uniform(0, ARENA), random.uniform(0, ARENA)) for _ in range(6)];

let kills = 0;
def count_kill(e) {
    global kills;
    kills += 1;
    print(f"  {e.name} defeated");
}
for (e in enemies) { e.on_death(count_kill); }

for (turn in range(1, 4)) {
    # attack the three closest enemies that are still alive
    let alive = filter(lambda (e) e.hp > 0, enemies);
    let targets = sorted(alive, lambda (e) hero.dist(e))[:3];
    print(f"turn {turn}: attacking {', '.join(map(lambda (e) e.name, targets))}");
    for (t in targets) { t.damage(random.randint(10, 25)); }
}

let survivors = [f"{e.name}({e.hp})" for e in enemies if e.hp > 0];
print(f"kills: {kills}, survivors: {survivors}");
