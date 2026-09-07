-- Тот же дефект, что был у closed_at, только в соседнем поле — и я его не
-- увидел, когда чинил первый, потому что смотрел на одно поле, а не на
-- правило.
--
-- `started_at` ставился при ЛЮБОЙ записи со статусом in_progress, включая
-- вставку. На импорте это значит, что каждый исторический тикет в работе
-- получает день импорта: 55 таких в acme, 13 в проверочном прогоне. Дата
-- выглядит правдоподобной и потому не вызывает вопросов — ровно как 1603
-- закрытия одним днём.
--
-- Правило, из которого это следует, записано в ntk-zxksmdiiat: поле, которое
-- база ВЫЧИСЛЯЕТ, обязано либо иметь свою строку в сверке, либо не
-- вычисляться там, где значение известно снаружи. Здесь второе: начало работы
-- фиксируется переходом, а вставка со своим значением — это импорт, и он
-- знает лучше. Пустое поле честнее выдуманного дня.
create or replace function touch_ticket() returns trigger language plpgsql as $$
declare
  terminal boolean;
begin
  new.updated_at := now();

  execute format('select grp = %L from %I.statuses where name = $1', 'complete', tg_table_schema)
    into terminal using new.status;

  if terminal then
    if tg_op = 'UPDATE' and new.closed_at is null then
      new.closed_at := now();
    end if;
  else
    new.closed_at := null;
  end if;

  -- Только переход. При вставке пришедшее значение сохраняется как есть, а
  -- его отсутствие остаётся отсутствием.
  if tg_op = 'UPDATE' and new.status = 'in_progress' and new.started_at is null then
    new.started_at := now();
  end if;

  if tg_op = 'INSERT' then
    new.current_status_at := coalesce(new.current_status_at, new.updated_at);
  elsif new.status is distinct from old.status then
    new.current_status_at := now();
  end if;

  return new;
end;
$$;
