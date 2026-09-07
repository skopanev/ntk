-- Словарь воркспейса. Набор переносится из Notion КАК ЕСТЬ — новой таксономии
-- здесь не изобретается, сегодняшний набор рабочий.
--
-- to_review и reviewed есть сейчас только в ftk; владелец решил завести их в
-- обоих воркспейсах, поэтому засев один на всех.

insert into statuses (name, grp, sort) values
  ('open',        'todo',        10),
  ('blocked',     'todo',        20),
  ('to_review',   'todo',        30),
  ('reviewed',    'todo',        40),
  ('in_progress', 'in_progress', 50),
  ('to_test',     'in_progress', 60),
  ('done',        'complete',    70)
on conflict (name) do update set grp = excluded.grp, sort = excluded.sort;

-- Гард «тикет уже кем-то подобран»: правка требует --force везде, кроме todo.
-- Владельцу названо следствие и он его принял: to_review и reviewed лежат в
-- todo, значит тикет НА РЕВЬЮ правится без --force, хотя подобран заведомо.
-- Решение живёт здесь именно чтобы менять его одной строкой.
insert into status_policy (grp, requires_force) values
  ('todo',        false),
  ('in_progress', true),
  ('complete',    true)
on conflict (grp) do update set requires_force = excluded.requires_force;

-- Одна шкала вместо двух. В Notion сейчас вперемешку high/med/low (329 тикетов)
-- и P0–P3/normal/4/medium (единицы). Свести к этой — задача импорта (U6);
-- здесь только сама шкала и порядок, по которому очередь выбирает следующий.
insert into priorities (name, rank) values
  ('high', 10),
  ('med',  20),
  ('low',  30)
on conflict (name) do update set rank = excluded.rank;
