-- Долг индексации: что осталось отправить в векторный индекс.
--
-- Долг пишется В ТОЙ ЖЕ ТРАНЗАКЦИИ, что и сама правка тикета. Иначе он
-- теряется ровно тогда, когда нужнее всего: процесс упал между коммитом и
-- постановкой в очередь — и тикет навсегда остался неиндексированным, причём
-- молча. Атомарность здесь не аккуратность, а единственный способ не врать.
create table if not exists vector_debt (
  -- Одна строка на тикет: последнее намерение вытесняет предыдущее. Десять
  -- правок подряд дают один долг, а не десять обращений к модели.
  ticket_id  text primary key references tickets(id) on delete cascade,
  op         text not null check (op in ('upsert','delete')),
  -- Отпечаток ТОГО, ЧТО ОТПРАВИМ: title + '\n' + body, усечённое до предела.
  -- У удаления входа нет.
  input_sha  text,
  attempts   int  not null default 0,
  last_error text,
  queued_at  timestamptz not null default now()
);
create index if not exists vector_debt_queued on vector_debt (queued_at);

-- Что уже лежит в индексе. Отдельно от tickets: состояние индекса — не свойство
-- тикета, и когда векторизацию выключат, эту таблицу можно снести целиком, не
-- трогая рабочие данные.
--
-- Хранится отпечаток ОТПРАВЛЕННОГО входа, а не времени: сравнение по времени
-- заставляет платить модели за правку, не изменившую текст, — например смену
-- приоритета или исполнителя.
create table if not exists vector_index_state (
  ticket_id   text primary key references tickets(id) on delete cascade,
  indexed_sha text        not null,
  indexed_at  timestamptz not null default now()
);
