-- Включение векторизации — по воркспейсу, и ВЫКЛЮЧЕНО по умолчанию.
--
-- Политика в данных, как status_policy, tag_requirements и write_limits:
-- владелец меняет её одним UPDATE. Здесь это важнее обычного — включение
-- означает обращения к платному провайдеру, и такое не должно требовать
-- выпуска, чтобы его можно было мгновенно выключить.
--
-- Таблица лежит В СХЕМЕ ВОРКСПЕЙСА, поэтому «по воркспейсу» получается само:
-- у каждого своя строка, и включение одного не трогает остальные. Массово при
-- выпуске не включается ничего — default false и есть это обещание.
create table if not exists vector_policy (
  -- Одна строка на воркспейс. Ключ-заглушка нужен, чтобы строка была ровно
  -- одна: без него таблица однажды окажется с двумя противоречащими записями.
  only_row   boolean primary key default true check (only_row),
  enabled    boolean not null default false,
  updated_at timestamptz not null default now()
);

insert into vector_policy (only_row, enabled) values (true, false)
  on conflict (only_row) do nothing;
