-- Включение векторизации теперь будит процесс, а не ждёт первой записи.
--
-- Включают её руками: UPDATE vector_policy SET enabled = true. Это событие в
-- базе, а не запрос к сервису, и до сих пор процесс о нём не узнавал никак.
-- Долг засевался триггером и лежал — до первой чужой записи или до
-- перезапуска. То есть «включили» и «пошла обработка» разъезжались на
-- неопределённый срок, и выглядело это как «векторизация не работает».
--
-- Уведомление уходит в тот же транзакции, что и засев, поэтому доставляется
-- ПОСЛЕ фиксации: слушатель не может проснуться на долг, которого ещё нет.
-- Схему кладём в тело сообщения — не для выбора работы (слив всё равно
-- обходит все включённые воркспейсы), а чтобы в журнале было видно, что
-- именно включили.
create or replace function vector_seed_on_enable() returns trigger
language plpgsql as $$
begin
  if new.enabled and not coalesce(old.enabled, false) then
    execute format($f$
      insert into %1$I.vector_debt (ticket_id, uuid, op, input_sha, attempts, last_error, queued_at)
      select t.id, t.uuid::text, 'upsert',
             encode(sha256(convert_to(left(t.title || chr(10) || t.body, 2000), 'UTF8')), 'hex'),
             0, null, now()
        from %1$I.tickets t
        left join %1$I.vector_index_state s on s.ticket_id = t.id
       where t.deleted_at is null
         and (s.indexed_sha is null
              or s.indexed_sha <> encode(sha256(convert_to(left(t.title || chr(10) || t.body, 2000), 'UTF8')), 'hex'))
      on conflict (ticket_id) do update
         set uuid = excluded.uuid, op = excluded.op, input_sha = excluded.input_sha,
             attempts = 0, last_error = null, queued_at = now()
    $f$, TG_TABLE_SCHEMA);
    perform pg_notify('ntk_vector', TG_TABLE_SCHEMA);
  end if;
  return new;
end;
$$;
