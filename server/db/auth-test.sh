#!/usr/bin/env bash
# Проверяет правила доступа: отказ по умолчанию, отзыв, выключенный пользователь,
# область из воркспейсов пользователя.
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
U=test_auth_user

# Воркспейсы заводит сам тест и сам убирает. Раньше здесь стояли имена рабочих
# воркспейсов, и проверка молча зависела от того, что они заведены: стоило
# одному смениться — падал внешний ключ на core.workspaces, а выглядело это как
# сломанное правило доступа.
WA=ws_auth_a
WB=ws_auth_b

cleanup() { sudo -u postgres psql -q -d ntk >/dev/null 2>&1 <<SQL || true
delete from core.user_keys where user_id='$U';
delete from core.user_workspaces where user_id='$U';
delete from core.users where id='$U';
delete from core.workspaces where name in ('$WA','$WB');
SQL
}
trap cleanup EXIT; cleanup

sudo -u postgres psql -q -v ON_ERROR_STOP=1 -d ntk <<SQL
insert into core.workspaces (name, schema_name) values
  ('$WA','$WA'), ('$WB','$WB')
on conflict (name) do nothing;
SQL

ok=0; fail=0
check() { local d="$1" want="$2" got="$3"
  if [ "$want" = "$got" ]; then echo "ок    $d"; ok=$((ok+1))
  else echo "ПРОВАЛ $d — ожидали '$want', получили '$got'"; fail=$((fail+1)); fi; }

q() { sudo -u postgres psql -tAd ntk -c "$1" | tr -d ' '; }

KEY=$("$DIR/issue-key.sh" $U "Тестовый" human "$WA,$WB" | tail -1 | tr -d ' ')
H=$(printf '%s' "$KEY" | sha256sum | cut -d' ' -f1)

# array_to_string обязателен: text || text[] поднимает всё выражение до массива
check "живой ключ отдаёт пользователя и оба воркспейса" "$U|human|member|$WA,$WB" \
      "$(q "select user_id||'|'||kind||'|'||role||'|'||array_to_string(workspaces,',') from core.resolve_key('$H')")"
check "неизвестный ключ — ноль строк" "" \
      "$(q "select user_id from core.resolve_key('deadbeef')")"

sudo -u postgres psql -q -d ntk -c "update core.users set active=false where id='$U'"
check "выключенный пользователь не проходит" "" "$(q "select user_id from core.resolve_key('$H')")"

sudo -u postgres psql -q -d ntk -c "update core.users set active=true where id='$U'"
sudo -u postgres psql -q -d ntk -c "update core.user_keys set revoked_at=now() where user_id='$U'"
check "отозванный ключ не проходит" "" "$(q "select user_id from core.resolve_key('$H')")"

echo "---"; echo "прошло: $ok, провалено: $fail"; [ "$fail" -eq 0 ]
