-- Функция истории не находила свою таблицу, когда правка шла мимо сервера.
--
-- Внутри функции стояло `insert into ticket_history`, без схемы. Сервер перед
-- работой делает `SET LOCAL search_path` на схему воркспейса, поэтому через
-- API всё работало. А прямой запрос из psql приходит с обычным путём поиска,
-- имя не разрешается, и правка падает: «relation "ticket_history" does not
-- exist».
--
-- То есть правка мимо сервера не записывалась в историю — она вообще не
-- проходила. Строгость эта мнимая: чинить данные руками иногда приходится
-- (восстановление после сбоя, исправление чужой ошибки), и такой отказ
-- означал бы, что для починки надо сначала снять триггер — то есть отключить
-- историю ровно в тот момент, когда она нужнее всего.
--
-- Схема берётся из самого триггера: он знает таблицу, на которой висит, и от
-- пути поиска больше не зависит. Тот же приём уже применён в этой схеме для
-- справочника статусов (миграция 004).
create or replace function record_ticket_history() returns trigger
language plpgsql as $$
declare
    who   text := coalesce(nullif(current_setting('ntk.actor', true), ''), 'unknown');
    what  text;
    delta jsonb;
begin
    if tg_op = 'INSERT' then
        execute format('insert into %I.ticket_history (ticket_id, actor, op, changes) values ($1,$2,$3,$4)', tg_table_schema)
          using new.id, who, 'create', to_jsonb(new) - 'body';
        return new;
    end if;

    if tg_op = 'DELETE' then
        execute format('insert into %I.ticket_history (ticket_id, actor, op, changes) values ($1,$2,$3,$4)', tg_table_schema)
          using old.id, who, 'delete', to_jsonb(old) - 'body';
        return old;
    end if;

    what := case
        when old.deleted_at is null and new.deleted_at is not null then 'delete'
        when old.deleted_at is not null and new.deleted_at is null then 'restore'
        else 'update'
    end;

    select jsonb_object_agg(
               coalesce(o.key, n.key),
               jsonb_build_object('from', o.value, 'to', n.value))
      into delta
      from jsonb_each(to_jsonb(old)) o
      full join jsonb_each(to_jsonb(new)) n on n.key = o.key
     where o.value is distinct from n.value
       and coalesce(o.key, n.key) <> 'updated_at';

    if delta is null then
        return new;
    end if;

    execute format('insert into %I.ticket_history (ticket_id, actor, op, changes) values ($1,$2,$3,$4)', tg_table_schema)
      using new.id, who, what, delta;
    return new;
end $$;
