-- Выражение отпечатка жило в трёх местах, не связанных ничем.
--
-- Одно и то же вычисление стояло в триггере засева, в сверке и в Rust. Пока
-- они совпадают буква в букву — всё работает; разойдись хоть в символе, и
-- сверка объявит устаревшими ВСЕ точки воркспейса, переиндексируя его каждые
-- шесть часов за деньги. Проверить это глазами можно, но никто не проверяет
-- глазами то, что «и так очевидно совпадает».
--
-- Двух SQL-копий больше нет: обе зовут эту функцию. Третья, в Rust, остаётся —
-- связать их нечем, кроме теста на паритет, который и так есть. Зато число
-- мест, где можно ошибиться, стало два вместо трёх.
--
-- immutable, потому что от одних аргументов всегда один ответ: это позволяет
-- планировщику считать её один раз на строку, а не на каждое упоминание.
create or replace function vector_input_sha(title text, body text) returns text
language sql immutable as $$
  select encode(sha256(convert_to(left($1 || chr(10) || $2, 2000), 'UTF8')), 'hex')
$$;

-- Триггер засева теперь зовёт функцию вместо своей копии выражения.
create or replace function vector_seed_on_enable() returns trigger
language plpgsql as $$
begin
  if new.enabled and not coalesce(old.enabled, false) then
    execute format($f$
      insert into %1$I.vector_debt (ticket_id, uuid, op, input_sha, attempts, last_error, queued_at)
      select t.id, t.uuid::text, 'upsert', %1$I.vector_input_sha(t.title, t.body), 0, null, now()
        from %1$I.tickets t
        left join %1$I.vector_index_state s on s.ticket_id = t.id
       where t.deleted_at is null
         and (s.indexed_sha is null
              or s.indexed_sha <> %1$I.vector_input_sha(t.title, t.body))
      on conflict (ticket_id) do update
         set uuid = excluded.uuid, op = excluded.op, input_sha = excluded.input_sha,
             attempts = 0, last_error = null, queued_at = now()
    $f$, TG_TABLE_SCHEMA);
    perform pg_notify('ntk_vector', TG_TABLE_SCHEMA);
  end if;
  return new;
end;
$$;
