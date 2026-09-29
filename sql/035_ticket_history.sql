-- История тикета: кто, когда и что изменил.
--
-- До сих пор её не было вовсе. Тикет хранит только ПОСЛЕДНЕЕ состояние плюс
-- четыре отметки времени — когда завели, когда трогали, когда взяли в работу,
-- когда закрыли. На вопрос «кто поменял статус и с какого на какой» ответить
-- было нечем: ни в одной таблице этого не записано. Человек видел, что тикет
-- стал другим, и не мог узнать, кем и почему.
--
-- ТРИГГЕР, А НЕ ЗАПИСЬ ИЗ КОДА. Код можно обойти — и обходят: правки прямыми
-- запросами в базу делались не раз, включая мои собственные. Такая правка
-- ничем не отличается от обычной работы и исчезает бесследно. Триггер видит
-- всё, что доходит до таблицы, независимо от того, каким путём пришло.
--
-- ДЕЛЬТА, А НЕ СНИМОК. Пишем только изменившиеся поля, парой «было/стало»:
-- снимок целиком на каждую правку тега — это тело тикета в каждой строке
-- истории, а тело у нас до 2257 знаков. Дельта читается глазами и занимает
-- сотни байт вместо килобайтов.
--
-- Сравнение идёт по jsonb всей строки, а не по перечню колонок: перечень
-- пришлось бы править при каждом добавлении поля, и однажды забыли бы — а
-- забытое поле меняется молча, что хуже отсутствия истории вовсе.
create table if not exists ticket_history (
    id         bigint generated always as identity primary key,
    ticket_id  text not null,
    at         timestamptz not null default clock_timestamp(),
    -- Кто. Ставится из настройки транзакции, которую выставляет сервер.
    -- Пусто означает «пришло мимо сервера» — и это тоже запись, а не пробел.
    actor      text not null,
    op         text not null check (op in ('create', 'update', 'delete', 'restore')),
    changes    jsonb not null
);
create index if not exists ticket_history_ticket on ticket_history (ticket_id, id desc);

create or replace function record_ticket_history() returns trigger
language plpgsql as $$
declare
    who     text := coalesce(nullif(current_setting('ntk.actor', true), ''), 'unknown');
    what    text;
    delta   jsonb;
begin
    if tg_op = 'INSERT' then
        insert into ticket_history (ticket_id, actor, op, changes)
        values (new.id, who, 'create', to_jsonb(new) - 'body');
        return new;
    end if;

    if tg_op = 'DELETE' then
        insert into ticket_history (ticket_id, actor, op, changes)
        values (old.id, who, 'delete', to_jsonb(old) - 'body');
        return old;
    end if;

    -- Мягкое удаление и возврат — это не «поле поменялось», это событие, и
    -- называть его надо своим именем: иначе восстановление тикета выглядит
    -- правкой даты, и найти его в истории можно только зная, что искать.
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
       -- updated_at двигает любая правка, и в истории он был бы шумом в
       -- каждой строке: время записи и так стоит в at.
       and coalesce(o.key, n.key) <> 'updated_at';

    -- Правка, ничего не изменившая, истории не оставляет: строка «поменяли
    -- ноль полей» не отвечает ни на один вопрос, а место занимает.
    if delta is null then
        return new;
    end if;

    insert into ticket_history (ticket_id, actor, op, changes)
    values (new.id, who, what, delta);
    return new;
end $$;

do $$ begin
    if not exists (select from pg_trigger where tgname = 'tickets_history'
                    and tgrelid = format('%I.tickets', current_schema())::regclass) then
        create trigger tickets_history after insert or update or delete
        on tickets for each row execute function record_ticket_history();
    end if;
end $$;

-- WORM. История дописывается и никогда не меняется: запись, которую можно
-- подправить, не свидетельство, а мнение. Исправление — это новая строка,
-- а не правка старой.
create or replace function history_is_immutable() returns trigger
language plpgsql as $$
begin
    raise exception 'WORM: история дописывается, а не меняется';
end $$;

do $$ begin
    if not exists (select from pg_trigger where tgname = 'ticket_history_worm'
                    and tgrelid = format('%I.ticket_history', current_schema())::regclass) then
        create trigger ticket_history_worm before update or delete or truncate
        on ticket_history for each statement execute function history_is_immutable();
    end if;
end $$;

-- Право менять историю отзывается ЯВНО, а не только триггером. Роль
-- воркспейса получает права на новые таблицы по умолчанию, включая UPDATE и
-- DELETE, и полагаться на один триггер значит держать дверь открытой и
-- сторожа рядом. Пусть двери не будет.
do $$
declare role_name text := 'ntk_ws_' || current_schema();
begin
    if exists (select from pg_roles where rolname = role_name) then
        execute format('revoke update, delete on ticket_history from %I', role_name);
        execute format('grant select, insert on ticket_history to %I', role_name);
    end if;
end $$;
