-- Объекты одного воркспейса. Применяется В КАЖДУЮ схему воркспейса
-- (acme, ftk) — раннер выставляет search_path, поэтому имён схем здесь
-- нет намеренно: один и тот же файл описывает любой воркспейс.
--
-- Идентичность сюда не заходит: люди, ключи и область доступа живут в core и
-- принадлежат серверной части. Здесь только тикеты.

-- Справочник статусов. В Notion группа — свойство типа status, в Postgres
-- такого понятия нет, а гард «тикет уже подобран» опирается именно на группу.
-- Без этой таблицы гард отключается молча: ни ошибки, ни падения теста.
create table if not exists statuses (
  name  text primary key,
  grp   text not null check (grp in ('todo', 'in_progress', 'complete')),
  sort  smallint not null
);

-- Поведение гарда вынесено ДАННЫМИ, а не зашито в код: владелец оставил
-- to_review и reviewed в группе todo, признав следствие — тикет на ревью
-- правится без --force. Решение может поменяться, и тогда это один UPDATE,
-- а не правка кода и релиз.
-- Ссылки на statuses(grp) быть не может: в группе несколько статусов, колонка
-- не уникальна. Список групп задан тем же check, что и в statuses.
create table if not exists status_policy (
  grp             text primary key check (grp in ('todo', 'in_progress', 'complete')),
  requires_force  boolean not null
);

-- Одна шкала приоритетов вместо двух. В Notion сейчас живут high/med/low и
-- P0–P3 вперемешку; сведение к одной — задача импорта (U6), здесь только
-- место, куда её свести. rank задаёт порядок: сортировка по важности
-- перестаёт быть клиентской.
create table if not exists priorities (
  name  text primary key,
  rank  smallint not null unique
);

create table if not exists projects (
  id          text primary key,
  name        text not null,
  archived    boolean not null default false,
  created_at  timestamptz not null default now()
);

create table if not exists tickets (
  -- Ключ вида proj-xxxxxxxxxx: его генерирует клиент, а не база. Уникальность
  -- обеспечивает первичный ключ — insert ... on conflict do nothing вместо
  -- сегодняшней проверки чтением, у которой есть окно гонки.
  id          text primary key check (id ~ '^[a-z0-9_-]+-[a-z0-9]{6,}$'),
  -- Форма вывода обещает uuid у каждого тикета, и агенты могли на это
  -- опереться. В Notion это был id страницы; здесь свой, чтобы обещание
  -- пережило смену бэкенда.
  uuid        uuid not null default gen_random_uuid() unique,
  title       text not null,
  status      text not null references statuses(name),
  priority    text references priorities(name),
  type        text,
  -- Исполнитель — тот же короткий хэндл, что в core.users: skk, camlo,
  -- agent-lane-p1. Человек и агент здесь неразличимы намеренно.
  assignee    text references core.users(id),
  project_id  text references projects(id),
  tags        text[] not null default '{}',
  body        text not null default '',
  meta        jsonb not null default '{}',
  due         date,
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now(),
  -- Ставится при переходе в терминальный статус и снимается при уходе из
  -- него. В Notion это делал CLI; здесь — триггер, иначе поле тихо перестанет
  -- обновляться, как это уже было с created_at/updated_at.
  closed_at   timestamptz,
  started_at  timestamptz
);

-- Зависимости — рёбра, а не список в строке: тогда «кто меня блокирует» и
-- «кого блокирую я» одинаково дёшевы, и обрезки на 25 записях, как в Notion,
-- не существует.
create table if not exists deps (
  ticket_id   text not null references tickets(id) on delete cascade,
  depends_on  text not null references tickets(id) on delete cascade,
  primary key (ticket_id, depends_on),
  check (ticket_id <> depends_on)
);
create index if not exists deps_depends_on on deps(depends_on);

create table if not exists attachments (
  id           bigserial primary key,
  ticket_id    text not null references tickets(id) on delete cascade,
  object_key   text not null unique,
  filename     text not null,
  content_type text,
  size_bytes   bigint not null check (size_bytes >= 0),
  etag         text,
  uploaded_at  timestamptz not null default now()
);
create index if not exists attachments_ticket on attachments(ticket_id);

-- Очередь: «самый важный незанятый» — это единственный запрос, который будет
-- исполняться под конкуренцией, и частичный индекс делает его дешёвым.
create index if not exists tickets_open_queue
  on tickets (priority, created_at) where status = 'open';
create index if not exists tickets_project_status on tickets (project_id, status);
create index if not exists tickets_tags on tickets using gin (tags);
create index if not exists tickets_meta on tickets using gin (meta);
create index if not exists tickets_assignee on tickets (assignee) where assignee is not null;

-- Время правки и закрытия ведёт база. В Notion это были её служебные поля;
-- перенести их в код значило бы получить строки, которые «не менялись» после
-- каждой записи, сделанной чем-то кроме нашего клиента.
create or replace function touch_ticket() returns trigger language plpgsql as $$
declare
  terminal boolean;
begin
  new.updated_at := now();

  select s.grp = 'complete' into terminal from statuses s where s.name = new.status;
  if terminal then
    -- Уже закрытый и закрываемый снова сохраняет исходный момент закрытия:
    -- правка описания не должна выглядеть как повторное закрытие.
    if tg_op = 'INSERT' or new.closed_at is null then
      new.closed_at := now();
    end if;
  else
    new.closed_at := null;
  end if;

  if new.status = 'in_progress' and new.started_at is null then
    new.started_at := now();
  end if;

  return new;
end;
$$;

drop trigger if exists tickets_touch on tickets;
create trigger tickets_touch before insert or update on tickets
  for each row execute function touch_ticket();
