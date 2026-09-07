#!/usr/bin/env bash
# Разворачивает последний дамп в одноразовую базу и проверяет, что данные на месте.
# Непроверенный бэкап бэкапом не считается — гонять регулярно, а не после инцидента.
set -euo pipefail

: "${SPACES_BUCKET:?}" "${SPACES_ENDPOINT:?}" "${SPACES_REGION:?}"
: "${AWS_ACCESS_KEY_ID:?}" "${AWS_SECRET_ACCESS_KEY:?}"

# Провал восстановления обязан быть слышен, а не остаться в журнале.
#
# Раньше скрипт только возвращал ненулевой код, и никто его не запускал: таймера
# на него не было вовсе. Дамп на 111 КБ, где не было ни одного тикета, при этом
# проходил проверку свежести — она смотрит возраст и размер, а не содержимое.
SLACK_URL="${SLACK_FN_URL:-https://us-central1-ifa-automation.cloudfunctions.net/sendSlackMessage}"
SLACK_CHANNEL="${SLACK_CHANNEL:-tech}"
alarm() {
  echo "ТРЕВОГА: $1" >&2
  curl -fsS -X POST "$SLACK_URL" -H 'content-type: application/json' \
    --max-time 20 --data "$(printf '{"channel":"%s","message":"ntk ТРЕВОГА: восстановление из бэкапа — %s"}' "$SLACK_CHANNEL" "$1")" \
    >/dev/null || echo "не удалось отправить тревогу в Slack" >&2
  exit 1
}

TMPDB="ntk_restore_check_$$"
DUMP=/tmp/verify-$$.dump

cleanup() { sudo -u postgres dropdb --if-exists "$TMPDB"; rm -f "$DUMP"; }
trap cleanup EXIT

curl -fsS --aws-sigv4 "aws:amz:${SPACES_REGION}:s3" \
     --user "${AWS_ACCESS_KEY_ID}:${AWS_SECRET_ACCESS_KEY}" \
     -o "$DUMP" "${SPACES_ENDPOINT%/}/${SPACES_BUCKET}/postgres/latest.dump"

[ -s "$DUMP" ] || { echo "ПРОВАЛ: скачанный дамп пуст"; exit 1; }

sudo -u postgres createdb "$TMPDB"
sudo -u postgres pg_restore --dbname="$TMPDB" --no-owner "$DUMP"

# Проверка обязана ПАДАТЬ, а не сообщать.
#
# Раньше она печатала число схем и завершалась успехом при нуле. Утром это
# было верно — схем ещё не было. К вечеру в базе лежало 2691 тикет, а в Spaces
# оставались утренние дампы по 1929 байт, и восстановление «проходило»,
# потому что восстанавливать было нечего. Зелёная проверка на пустоте — это
# не проверка.
WS=$(sudo -u postgres psql -d "$TMPDB" -tAc "select count(*) from information_schema.schemata
  where schema_name not in ('public','core','information_schema','pg_catalog','pg_toast')")
[ "${WS:-0}" -ge 1 ] || { echo "ПРОВАЛ: в дампе нет ни одной схемы воркспейса"; exit 1; }

TICKETS=0
for s in $(sudo -u postgres psql -d "$TMPDB" -tAc "select schema_name from information_schema.schemata
    where schema_name not in ('public','core','information_schema','pg_catalog','pg_toast')"); do
  n=$(sudo -u postgres psql -d "$TMPDB" -tAc "select count(*) from \"$s\".tickets" 2>/dev/null || echo 0)
  echo "  $s: $n тикетов"
  TICKETS=$((TICKETS + n))
done
[ "$TICKETS" -ge 1 ] || alarm "в дампе нет ни одного тикета"

USERS=$(sudo -u postgres psql -d "$TMPDB" -tAc "select count(*) from core.users" 2>/dev/null || echo 0)
[ "${USERS:-0}" -ge 1 ] || alarm "в дампе нет людей — core не восстановился"

# Дамп может восстановиться и при этом отстать от живой базы — так и вышло с
# ночным прогоном, снятым до заливки тикетов. Порог 90%: тикеты добавляются
# постоянно, точного равенства между снимком и «сейчас» не бывает никогда.
LIVE=$(sudo -u postgres psql -d ntk -tAc "select coalesce(sum(n),0) from (
    select (xpath('/row/c/text()', query_to_xml('select count(*) c from '||quote_ident(table_schema)||'.tickets', false, true, '')))[1]::text::int as n
      from information_schema.tables
     where table_name='tickets' and table_schema not in ('information_schema','pg_catalog')) t" 2>/dev/null || echo 0)
if [ "${LIVE:-0}" -gt 0 ]; then
  MIN=$(( LIVE * 90 / 100 ))
  [ "$TICKETS" -ge "$MIN" ] || alarm "в дампе $TICKETS тикетов против $LIVE в базе — снимок отстал"
fi
echo "восстановление прошло: схем $WS, тикетов $TICKETS (в базе $LIVE), людей $USERS"
