#!/usr/bin/env bash
# Заводит воркспейс: схема + роль + права. Идемпотентен.
#   ./bootstrap-workspace.sh <имя>
#
# Что внутри схемы (таблицы, индексы, триггеры) — не наше дело, это миграции.
# Наше — кто имеет право в неё войти.
set -euo pipefail
WS="${1:?укажи имя воркспейса}"
[[ "$WS" =~ ^[a-z_][a-z0-9_]{0,30}$ ]] || { echo "ОШИБКА: имя только [a-z0-9_], до 31 символа"; exit 1; }
ROLE="ntk_ws_${WS}"

PW="$(openssl rand -base64 24 | tr -d '/+=' | head -c 32)"

sudo -u postgres psql -q -v ON_ERROR_STOP=1 -d ntk <<SQL
create schema if not exists "${WS}" authorization ntk_admin;

-- Регистрируем в core, иначе выдать пользователю доступ будет некуда:
-- core.user_workspaces ссылается сюда внешним ключом.
insert into core.workspaces (name, schema_name) values ('${WS}', '${WS}')
  on conflict (name) do nothing;

-- Роль воркспейса. Логиниться ей никто не будет: в неё входят через SET ROLE.
select 'create role "${ROLE}" nologin' where not exists
  (select from pg_roles where rolname='${ROLE}')\gexec

-- Никаких прав по умолчанию и никому лишнему.
revoke all on schema "${WS}" from public;
grant usage on schema "${WS}" to "${ROLE}";

-- Уже существующие объекты.
grant select, insert, update, delete on all tables    in schema "${WS}" to "${ROLE}";
grant usage, select                  on all sequences in schema "${WS}" to "${ROLE}";

-- И, главное, БУДУЩИЕ: миграции создаёт ntk_admin, значит права выдаём за него.
-- Без этого каждая новая таблица приезжала бы без прав, и это выяснялось бы
-- в проде, а не на миграции.
alter default privileges for role ntk_admin in schema "${WS}"
  grant select, insert, update, delete on tables to "${ROLE}";
alter default privileges for role ntk_admin in schema "${WS}"
  grant usage, select on sequences to "${ROLE}";

-- ntk_api САМ прав на схему не имеет — он лишь член роли и входит через SET ROLE.
-- Поэтому забытый фильтр в коде не открывает чужой воркспейс: привилегий нет.
-- Обязательно: без NOINHERIT членство даёт права БЕЗ SET ROLE, и изоляция
-- превращается в фикцию. Утверждаем при каждом заведении воркспейса.
alter role ntk_api noinherit;
grant "${ROLE}" to ntk_api;
SQL

echo "воркспейс ${WS}: схема и роль ${ROLE} готовы"
echo "в запросе: SET LOCAL ROLE \"${ROLE}\"; SET LOCAL search_path = \"${WS}\";"
