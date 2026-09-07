-- Сколько тикет висит в своём нынешнем статусе.
--
-- Существующие отметки отвечают на другие вопросы: created_at — когда завели,
-- updated_at — когда трогали (любая правка тела сбивает), started_at — когда
-- впервые взяли в работу, closed_at — когда закрыли. Ни одна не говорит,
-- сколько тикет лежит в to_review прямо сейчас, а это и есть вопрос, который
-- задают о зависшей работе.
--
-- Заполняется ПЕРЕХОДОМ, как и closed_at: время меняется, только когда
-- меняется статус. Правка тела не должна выглядеть сменой состояния.
alter table tickets add column if not exists current_status_at timestamptz;

-- Историю переходов взять неоткуда: в Notion её нет. Для уже существующих
-- строк ставим лучшее из доступного и НЕ притворяемся, что это точная дата:
-- started_at, если тикет в работе; closed_at, если закрыт; иначе updated_at.
update tickets
   set current_status_at = coalesce(
         case when status = 'in_progress' then started_at end,
         closed_at,
         updated_at,
         created_at)
 where current_status_at is null;

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

  if new.status = 'in_progress' and new.started_at is null then
    new.started_at := now();
  end if;

  -- Отметка текущего статуса. При вставке своё значение уважаем (импорт знает
  -- лучше), при правке двигаем только если статус реально сменился.
  if tg_op = 'INSERT' then
    new.current_status_at := coalesce(new.current_status_at, new.updated_at);
  elsif new.status is distinct from old.status then
    new.current_status_at := now();
  end if;

  return new;
end;
$$;
