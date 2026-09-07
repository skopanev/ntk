-- Триггер выдумывал дату закрытия при вставке.
--
-- Условие «tg_op = INSERT или closed_at пуст» ставило now() ВСЕГДА при
-- вставке, затирая даже переданное значение. На полном импорте это дало 1603
-- тикета с сегодняшней датой закрытия — включая 436, у которых настоящая дата
-- была известна и приехала из Notion. Остальные 1167 получили дату, которой не
-- существовало: они закрыты когда-то, а не сегодня.
--
-- Найдено сверкой содержимого после успешного импорта: числа сходились,
-- сверка была зелёной, а данные — испорчены. Контрольные суммы тел этого не
-- ловят, потому что тела как раз целы.
--
-- Теперь закрытие фиксируется ПЕРЕХОДОМ, а не фактом вставки: то, что пришло
-- в INSERT, сохраняется как есть; отсутствие даты у исторического тикета
-- остаётся отсутствием. Пустое поле честнее выдуманного дня.
create or replace function touch_ticket() returns trigger language plpgsql as $$
declare
  terminal boolean;
begin
  new.updated_at := now();

  execute format('select grp = %L from %I.statuses where name = $1', 'complete', tg_table_schema)
    into terminal using new.status;

  if terminal then
    -- Дата ставится только когда тикет закрывается ПЕРЕХОДОМ и своей даты не
    -- принёс. Вставка со своей датой — это импорт, и он знает лучше.
    if tg_op = 'UPDATE' and new.closed_at is null then
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
