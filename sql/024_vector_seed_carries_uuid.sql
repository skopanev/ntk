-- Триггер засева не заполнял uuid, и это обесценивало починку 023.
--
-- В 023 долг перестал зависеть от выживания строки тикета: он несёт uuid сам,
-- чтобы точку можно было снести даже после физического удаления. Но uuid я
-- добавил только в путях сервиса (заведение, правка, удаление), а триггер
-- продолжал вставлять без него. Долги, поставленные ВКЛЮЧЕНИЕМ воркспейса — то
-- есть все начальные, — оставались без uuid, и для них призрак был снова
-- неустраним.
--
-- Поймано проверкой после выкатки 023: долгов 30, из них с uuid 0. Починка,
-- проверенная только на том пути, где её писали, — половина починки.
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
  end if;
  return new;
end;
$$;
