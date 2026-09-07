#!/usr/bin/env bash
# Выдаёт ключ пользователю. Ключ показывается ОДИН раз — в базе только sha256.
#   ./issue-key.sh <user-id> "<Имя>" <human|agent> <воркспейс>[,<воркспейс>…] [метка]
#
# Ключ принадлежит пользователю: область доступа берётся из core.user_workspaces,
# поэтому выпуск второго ключа ничего не расширяет, а отзыв первого ничего не ломает.
set -euo pipefail
UID_="${1:?user-id}"; NAME="${2:?имя}"; KIND="${3:?human|agent}"; WS="${4:?воркспейсы через запятую}"
LABEL="${5:-}"
[[ "$KIND" =~ ^(human|agent)$ ]] || { echo "ОШИБКА: kind = human или agent"; exit 1; }

KEY="ntk_$(openssl rand -base64 36 | tr -dc 'A-Za-z0-9' | head -c 40)"
HASH="$(printf '%s' "$KEY" | sha256sum | cut -d' ' -f1)"
PREFIX="${KEY:0:12}"

sudo -u postgres psql -q -v ON_ERROR_STOP=1 -d ntk <<SQL
insert into core.users (id, display_name, kind)
values ('${UID_}', \$\$${NAME}\$\$, '${KIND}')
on conflict (id) do update set display_name = excluded.display_name;

insert into core.user_workspaces (user_id, workspace)
select '${UID_}', trim(w) from unnest(string_to_array('${WS}', ',')) w
on conflict do nothing;

insert into core.user_keys (key_hash, key_prefix, user_id, label)
values ('${HASH}', '${PREFIX}', '${UID_}', nullif(\$\$${LABEL}\$\$, ''));
SQL

echo "пользователь: ${UID_} (${KIND})"
echo "воркспейсы:   ${WS}"
echo "префикс:      ${PREFIX}…"
echo
echo "КЛЮЧ (показывается один раз, сохрани сейчас):"
echo "  ${KEY}"
