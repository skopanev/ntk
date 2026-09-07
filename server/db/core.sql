-- core — идентичность и доступ. Одна схема на всю базу.
-- Тикеты живут в схемах воркспейсов; сюда они не заходят.
--
-- Принцип: ключ принадлежит ПОЛЬЗОВАТЕЛЮ, а не воркспейсу. Область доступа
-- выводится из пользователя, поэтому ротация ключа её не меняет, а у одного
-- человека может быть несколько ключей (ноут, сервер, телефон).

create schema if not exists core authorization ntk_admin;

create table if not exists core.workspaces (
  name         text primary key,
  schema_name  text not null unique,
  created_at   timestamptz not null default now()
);

create table if not exists core.users (
  id           text primary key,                    -- короткий хэндл: skk, camlo, agent-falcon-p1
  display_name text not null,
  kind         text not null check (kind in ('human','agent')),
  role         text not null default 'member',      -- member | camlo | admin
  active       boolean not null default true,       -- выключатель без удаления
  created_at   timestamptz not null default now()
);

-- Куда пользователю можно. Нет строки — нет доступа: отказ по умолчанию.
create table if not exists core.user_workspaces (
  user_id    text not null references core.users(id)      on delete cascade,
  workspace  text not null references core.workspaces(name) on delete cascade,
  primary key (user_id, workspace)
);

-- Ключи. Хранится ТОЛЬКО sha256; исходник показывается один раз при выдаче.
-- Ключ высокоэнтропийный (192 бита), поэтому соль не нужна, а простой хеш
-- позволяет искать по индексу вместо перебора всех строк.
create table if not exists core.user_keys (
  key_hash     text primary key,
  key_prefix   text not null,                       -- первые символы, для опознания; не секрет
  user_id      text not null references core.users(id) on delete cascade,
  label        text,
  created_at   timestamptz not null default now(),
  last_used_at timestamptz,
  revoked_at   timestamptz
);
create index if not exists user_keys_user on core.user_keys(user_id) where revoked_at is null;

-- Разрешение ключа в личность и область. Единственный вход для API.
-- Живой ключ + активный пользователь + выданные воркспейсы, иначе ноль строк.
create or replace function core.resolve_key(p_hash text)
returns table (user_id text, kind text, role text, workspaces text[])
language sql stable as $$
  select u.id, u.kind, u.role,
         coalesce(array_agg(uw.workspace order by uw.workspace)
                  filter (where uw.workspace is not null), '{}')
  from core.user_keys k
  join core.users u on u.id = k.user_id
  left join core.user_workspaces uw on uw.user_id = u.id
  where k.key_hash = p_hash and k.revoked_at is null and u.active
  group by u.id, u.kind, u.role;
$$;

-- API читает и отмечает использование, но НЕ заводит людей и ключи:
-- выдача — операция администратора.
grant usage on schema core to ntk_api;
grant select on core.workspaces, core.users, core.user_workspaces, core.user_keys to ntk_api;
grant update (last_used_at) on core.user_keys to ntk_api;
grant execute on function core.resolve_key(text) to ntk_api;
revoke all on schema core from public;

-- ---------------------------------------------------------------------------
-- Вход через браузер: device-flow.
--
-- Человек не вводит ключ и не получает его в переписке. Расширение показывает
-- короткий код, человек открывает ссылку, входит через Google, расширение
-- забирает ключ само. Ключ не путешествует ни через почту, ни через владельца.
create table if not exists core.device_codes (
  code         text primary key,              -- короткий, человек читает с экрана
  device_hash  text not null,                 -- sha256 секрета, известного только расширению
  created_at   timestamptz not null default now(),
  expires_at   timestamptz not null,
  -- Заполняется после успешного входа. Пока null — опрос отвечает «ещё нет».
  user_id      text references core.users(id) on delete cascade,
  issued_key   text,                          -- одноразовая выдача, стирается после забора
  claimed_at   timestamptz
);
create index if not exists device_codes_expiry on core.device_codes(expires_at);

-- Кого пускать. Домен решает «пустить вообще», строка по email — что именно
-- человек получает. Более конкретное правило побеждает.
create table if not exists core.enrollment_rules (
  match_type   text not null check (match_type in ('domain','email')),
  match_value  text not null,
  workspaces   text[] not null,
  role         text not null default 'member',
  primary key (match_type, match_value)
);

grant select, insert, update, delete on core.device_codes to ntk_api;
grant select on core.enrollment_rules to ntk_api;
-- Заведение людей при самозаписи — единственное исключение: без него
-- самообслуживание невозможно, а иначе ключи снова раздаёт владелец руками.
grant insert, select on core.users to ntk_api;
grant insert, select on core.user_workspaces to ntk_api;
grant insert on core.user_keys to ntk_api;

-- ---------------------------------------------------------------------------
-- Адрес на карточке: без него совпадение инициалов отдаёт чужую личность.
--
-- Идентификатор человека выводится из части адреса до @, а карточки заведены
-- заранее из данных Notion по инициалам. Значит ar@другая-компания.com молча
-- получил бы карточку Александра Русских и 757 его тикетов — без ошибки, без
-- следа. Инициалы совпадают легко; это вопрос времени, а не вероятности.
--
-- Правило: карточка без адреса присваивается первым входом, карточка с
-- адресом пускает только его владельца.
alter table core.users add column if not exists email text unique;

-- Право узкое: API может проставить адрес на ничейной карточке и не может
-- изменить ничего другого. Самозапись создаёт человека, но не переписывает
-- то, что о нём знает администратор.
grant update (email) on core.users to ntk_api;

-- ---------------------------------------------------------------------------
-- Выпуски клиента: откуда обновляться.
--
-- Артефакты лежат в Spaces, бакет приватный, ссылки выдаёт API
-- предподписанными. Здесь только описание: что считать текущей версией и чем
-- проверить скачанное.
create table if not exists core.releases (
  version      text primary key,          -- 0.5.3
  object_key   text not null,             -- ключ в Spaces
  sha256       text not null,             -- сверяется ПОСЛЕ скачивания
  platform     text not null default 'darwin-arm64',
  published_at timestamptz not null default now(),
  -- Выпуск можно отозвать, не удаляя: клиенты перестанут его предлагать.
  yanked_at    timestamptz
);
grant select on core.releases to ntk_api;

-- Адрес, с которого начали вход: потолок на незавершённые входы считается по
-- нему. Глобальный потолок был дырой — один клиент закрывал вход всем.
alter table core.device_codes add column if not exists client_ip text;
create index if not exists device_codes_ip on core.device_codes(client_ip)
  where claimed_at is null;
