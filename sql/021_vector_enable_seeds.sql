-- Включение воркспейса САМО ставит долг на всё, что уже есть.
--
-- Без этого «включить» ничего не означало бы: слив просыпается на старте
-- процесса и на следующей записи, а после переключения записи может не быть
-- никогда — воркспейс с тремя тысячами тикетов остался бы непроиндексированным,
-- и молча. Владелец включает одним UPDATE, значит этот же UPDATE и есть явная
-- начальная синхронизация. Ни команды, ни таймера, ни отдельной операции.
--
-- Отпечаток считается ТЕМ ЖЕ способом, что и в сервисе: title, перевод строки,
-- тело, обрезка до 2000 СИМВОЛОВ (left считает символы, не байты), sha256 от
-- UTF-8. Расхождение этих двух мест означало бы вечную переиндексацию всего.
create or replace function vector_seed_on_enable() returns trigger
language plpgsql as $$
begin
  if new.enabled and not coalesce(old.enabled, false) then
    insert into vector_debt (ticket_id, op, input_sha, attempts, last_error, queued_at)
    select t.id, 'upsert',
           encode(sha256(convert_to(left(t.title || E'\n' || t.body, 2000), 'UTF8')), 'hex'),
           0, null, now()
      from tickets t
      left join vector_index_state s on s.ticket_id = t.id
     where t.deleted_at is null
       -- Уже проиндексированное с тем же отпечатком повторно не ставим:
       -- переключение туда-обратно не должно стоить полной переоплаты модели.
       and (s.indexed_sha is null
            or s.indexed_sha <> encode(sha256(convert_to(left(t.title || E'\n' || t.body, 2000), 'UTF8')), 'hex'))
    on conflict (ticket_id) do nothing;
  end if;
  return new;
end;
$$;

drop trigger if exists vector_seed on vector_policy;
create trigger vector_seed after update on vector_policy
  for each row execute function vector_seed_on_enable();
