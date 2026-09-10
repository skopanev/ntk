-- Две находки ревью, обе про «сегодня недостижимо, а завтра тихо сломается».
--
-- B. Триггер засева стоял только на UPDATE. Законный путь включения — UPDATE,
-- но таблице не запрещён DELETE строки и INSERT новой с enabled = true, и тогда
-- долг не засеялся бы вовсе. Условие when (new.enabled) заодно избавляет от
-- лишних срабатываний на правках, не меняющих флаг.
drop trigger if exists vector_seed on vector_policy;
create trigger vector_seed after insert or update on vector_policy
  for each row when (new.enabled) execute function vector_seed_on_enable();

-- C. Долг на удаление висел на внешнем ключе к tickets с on delete cascade.
-- Значит физическое удаление тикета унесло бы долг вместе со строкой, а точка
-- осталась бы в Qdrant навсегда — призрак, которого никто не найдёт, потому что
-- искать его нечем: uuid жил только в удалённой строке.
--
-- Сегодня этот путь недостижим: remove помечает deleted_at, физического
-- удаления в коде нет. Но «недостижимо» — не «невозможно», а цена ошибки здесь
-- вечная утечка.
--
-- Решение: долг несёт uuid САМ и больше не зависит от выживания строки тикета.
-- Точку можно снести, даже если тикета уже нет.
alter table vector_debt add column if not exists uuid text;
alter table vector_debt drop constraint if exists vector_debt_ticket_id_fkey;

update vector_debt d
   set uuid = t.uuid::text
  from tickets t
 where t.id = d.ticket_id and d.uuid is null;

-- Реестр проиндексированного остаётся на внешнем ключе намеренно: он говорит
-- «эта строка проиндексирована», и без строки утверждение бессмысленно.
