-- Триггер засева долга обязан искать таблицы В СВОЕЙ схеме, а не по
-- search_path вызывающего.
--
-- Первая версия использовала неполные имена: vector_debt, tickets. Внутри
-- функции они разрешаются в момент ВЫЗОВА, по search_path того, кто дёрнул
-- UPDATE. Владелец включает векторизацию обычным psql, где search_path равен
-- "$user", public — и вставка падала «нет такого отношения». Долг не засевался
-- вовсе, а сам UPDATE при этом откатывался: включение просто не срабатывало.
--
-- Поймано первым же прогоном на боевых данных, скопированных в test. На пустом
-- воркспейсе это не проявлялось: засевать было нечего, и ошибки не возникало.
--
-- Схема берётся из TG_TABLE_SCHEMA — то есть из той таблицы, на которой висит
-- триггер. Это надёжнее, чем закреплять search_path при создании: он у функции
-- один, а схем три, и каждая должна работать со своими таблицами.
create or replace function vector_seed_on_enable() returns trigger
language plpgsql as $$
begin
  if new.enabled and not coalesce(old.enabled, false) then
    -- chr(10), а не E'\n': внутри строки формата обратный слеш пришлось бы
    -- экранировать дважды, и один пропущенный слой дал бы вместо перевода
    -- строки два символа — то есть ДРУГОЙ отпечаток, чем считает сервис.
    execute format($f$
      insert into %1$I.vector_debt (ticket_id, op, input_sha, attempts, last_error, queued_at)
      select t.id, 'upsert',
             encode(sha256(convert_to(left(t.title || chr(10) || t.body, 2000), 'UTF8')), 'hex'),
             0, null, now()
        from %1$I.tickets t
        left join %1$I.vector_index_state s on s.ticket_id = t.id
       where t.deleted_at is null
         and (s.indexed_sha is null
              or s.indexed_sha <> encode(sha256(convert_to(left(t.title || chr(10) || t.body, 2000), 'UTF8')), 'hex'))
      on conflict (ticket_id) do nothing
    $f$, TG_TABLE_SCHEMA);
  end if;
  return new;
end;
$$;
