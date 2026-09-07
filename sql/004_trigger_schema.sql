-- Триггер обращался к statuses без имени схемы, поэтому работал только у того,
-- кто заранее выставил search_path. Любая запись из обычной сессии — psql,
-- скрипт восстановления, проверка бэкапа — падала с «relation statuses does
-- not exist», и запись просто не проходила.
--
-- Поймано случайно: сверка сообщила, что тела совпадают, хотя я только что
-- пытался их испортить. Совпадали они потому, что UPDATE не выполнился вовсе.
-- Проверка, которая не может отличить «всё цело» от «изменить не удалось», —
-- ровно тот класс дефекта, за которым мы охотимся весь проект.
--
-- Схему функция берёт у таблицы, на которой сработала: она одинаковая в каждом
-- воркспейсе, и привязывать её к одной схеме нельзя.
create or replace function touch_ticket() returns trigger language plpgsql as $$
declare
  terminal boolean;
begin
  new.updated_at := now();

  execute format('select grp = %L from %I.statuses where name = $1', 'complete', tg_table_schema)
    into terminal using new.status;

  if terminal then
    if tg_op = 'INSERT' or new.closed_at is null then
      new.closed_at := now();
    end if;
  else
    new.closed_at := null;
  end if;

  if new.status = 'in_progress' and new.started_at is null then
    new.started_at := now();
  end if;

  return new;
end;
$$;
