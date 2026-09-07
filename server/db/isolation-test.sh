#!/usr/bin/env bash
# Доказывает, что изоляция воркспейсов держится НА УРОВНЕ БАЗЫ, без участия API.
# Заводит два одноразовых воркспейса, кладёт в каждый таблицу и проверяет доступ.
set -euo pipefail
A=ws_isolation_a; B=ws_isolation_b
DIR="$(cd "$(dirname "$0")" && pwd)"

cleanup() {
  sudo -u postgres psql -q -d ntk >/dev/null 2>&1 <<SQL || true
drop schema if exists $A cascade; drop schema if exists $B cascade;
revoke ntk_ws_$A from ntk_api; revoke ntk_ws_$B from ntk_api;
drop role if exists ntk_ws_$A; drop role if exists ntk_ws_$B;
SQL
}
trap cleanup EXIT
cleanup

"$DIR/bootstrap-workspace.sh" $A >/dev/null
"$DIR/bootstrap-workspace.sh" $B >/dev/null

# Таблицы создаёт ntk_admin — как это делали бы миграции.
sudo -u postgres psql -q -v ON_ERROR_STOP=1 -d ntk <<SQL
set role ntk_admin;
create table $A.t(id int); insert into $A.t values (1);
create table $B.t(id int); insert into $B.t values (2);
SQL

set -a; . /etc/ntk/api.env; set +a
ok=0; fail=0
check() { # описание, ожидание(allow|deny), sql
  local desc="$1" want="$2" sql="$3" out
  if out=$(psql "$DATABASE_URL" -tAc "$sql" 2>&1); then got=allow; else got=deny; fi
  if [ "$got" = "$want" ]; then echo "ок    $desc"; ok=$((ok+1))
  else echo "ПРОВАЛ $desc — ожидали $want, получили $got"; echo "      $out" | head -2; fail=$((fail+1)); fi
}

check "своя схема под своей ролью читается" allow \
      "set role ntk_ws_$A; select id from $A.t"
check "ЧУЖАЯ схема под своей ролью НЕ читается" deny \
      "set role ntk_ws_$A; select id from $B.t"
check "ntk_api без SET ROLE не читает ничего" deny \
      "select id from $A.t"
check "ntk_api не может создать таблицу в чужой схеме" deny \
      "set role ntk_ws_$A; create table $B.hack(x int)"

echo "---"; echo "прошло: $ok, провалено: $fail"
[ "$fail" -eq 0 ]
