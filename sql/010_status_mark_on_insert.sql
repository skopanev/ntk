-- Третий случай одного класса, после closed_at и started_at.
--
-- При вставке current_status_at брался из updated_at, то есть из времени
-- записи. Для привезённого импортом закрытого тикета это значит «висит в done
-- с сегодняшнего дня», хотя закрыт он был раньше: на проверке 80 тикетов из
-- 250 закрыты 31 августа, а отметка стояла 1 сентября.
--
-- Сама миграция 007 делала это правильно для строк, которые уже лежали в
-- базе: брала started_at для работающих, closed_at для закрытых. Новые строки
-- шли мимо этого правила, потому что оно жило в разовом UPDATE, а не в
-- триггере. Здесь та же логика переносится туда, где она и должна быть.
--
-- Найдено сверкой, которая появилась ровно из-за первых двух случаев. Это и
-- есть смысл правила из ntk-zxksmdiiat: вычисляемое поле проверяется, иначе
-- ошибка выглядит как правдоподобная дата.
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

  if tg_op = 'UPDATE' and new.status = 'in_progress' and new.started_at is null then
    new.started_at := now();
  end if;

  if tg_op = 'INSERT' then
    -- Порядок тот же, что в 007: сначала пришедшее значение, затем то, что
    -- известно о переходе, и только потом время записи.
    new.current_status_at := coalesce(
      new.current_status_at,
      case when terminal then new.closed_at end,
      case when new.status = 'in_progress' then new.started_at end,
      new.updated_at);
  elsif new.status is distinct from old.status then
    new.current_status_at := now();
  end if;

  return new;
end;
$$;
