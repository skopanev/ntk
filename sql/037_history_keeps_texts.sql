-- Тексты хранятся целиком, а производные отметки времени — нет.
--
-- Две правки к истории, и обе про читаемость.
--
-- 1. ИСХОДНОЕ ТЕЛО ТЕРЯЛОСЬ. При заведении тикета снимок писался без тела —
-- я выбросил его, экономя место. Экономия вышла не на том: тело пишется один
-- раз на тикет, это две тысячи знаков, а без него в ленте нет НАЧАЛА цепочки.
-- Видно, как текст переписали, и не видно, что было написано сначала.
-- Экономить имело смысл на снимке при КАЖДОЙ правке — от него и отказались в
-- пользу дельты; заведение бывает однажды.
--
-- 2. ПРОИЗВОДНЫЕ ОТМЕТКИ ЗАСОРЯЛИ ЛЕНТУ. started_at, closed_at и
-- current_status_at меняются не сами по себе — их двигает переход статуса,
-- который в той же записи и стоит. В строке «status: in_progress → to_test»
-- они занимали больше места, чем сам переход, и в узком выводе вытесняли его
-- совсем: человек видел две метки времени вместо того, что случилось.
-- Выводимое из записанного не записывается второй раз.
create or replace function record_ticket_history() returns trigger
language plpgsql as $$
declare
    who   text := coalesce(nullif(current_setting('ntk.actor', true), ''), 'unknown');
    what  text;
    delta jsonb;
    -- Отметки, выводимые из перехода статуса. Здесь же updated_at: его двигает
    -- любая правка, а время записи и так стоит отдельным полем.
    derived constant text[] := array['updated_at', 'current_status_at', 'started_at', 'closed_at'];
begin
    if tg_op = 'INSERT' then
        execute format('insert into %I.ticket_history (ticket_id, actor, op, changes) values ($1,$2,$3,$4)', tg_table_schema)
          using new.id, who, 'create', to_jsonb(new);
        return new;
    end if;

    if tg_op = 'DELETE' then
        execute format('insert into %I.ticket_history (ticket_id, actor, op, changes) values ($1,$2,$3,$4)', tg_table_schema)
          using old.id, who, 'delete', to_jsonb(old);
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
       and not (coalesce(o.key, n.key) = any(derived));

    -- Правка, изменившая только производные отметки, записи не оставляет:
    -- она уже записана той строкой, где сменился статус.
    if delta is null then
        return new;
    end if;

    execute format('insert into %I.ticket_history (ticket_id, actor, op, changes) values ($1,$2,$3,$4)', tg_table_schema)
      using new.id, who, what, delta;
    return new;
end $$;
