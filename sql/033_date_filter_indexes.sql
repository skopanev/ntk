-- Отбор по датам уходил в последовательный скан по четырём полям из пяти.
--
-- Замерено на боевой базе, 4415 тикетов, план запроса списка с отбором и
-- обычной сортировкой `created_at desc`:
--   created_at        Index Scan (tickets_assignee_created) + отдельная Sort
--   updated_at        Seq Scan
--   closed_at         Seq Scan
--   started_at        Seq Scan
--   due               Seq Scan
--   stale             Seq Scan
--
-- По created_at индекс формально находился, но чужой: в
-- tickets_assignee_created ведущая колонка — исполнитель, поэтому отбор по
-- одной дате идёт полным проходом по индексу, а порядок он не отдаёт.
--
-- Порядок колонок тот же, что и в 011: сначала то, по чему фильтруют, потом
-- то, по чему сортируют. Здесь фильтр и сортировка совпадают, поэтому индекс
-- отдаёт уже отсортированное и Sort из плана уходит.
--
-- Частичные там, где колонка обычно пуста: начатых, закрытых и тикетов со
-- сроком заметно меньше, чем всех, и индекс по ним во столько же раз меньше.
create index if not exists tickets_created
  on tickets (created_at desc)
  where deleted_at is null;

create index if not exists tickets_updated
  on tickets (updated_at desc)
  where deleted_at is null;

create index if not exists tickets_closed
  on tickets (closed_at desc)
  where deleted_at is null and closed_at is not null;

create index if not exists tickets_started
  on tickets (started_at desc)
  where deleted_at is null and started_at is not null;

create index if not exists tickets_due
  on tickets (due)
  where deleted_at is null and due is not null;

-- stale спрашивает «дольше N дней в текущем статусе» и до сих пор читал всю
-- таблицу на каждый вызов — а именно этим ищут брошенную работу.
create index if not exists tickets_current_status
  on tickets (current_status_at)
  where deleted_at is null;
