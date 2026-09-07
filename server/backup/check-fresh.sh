#!/usr/bin/env bash
# Свежесть последнего бэкапа. Запускается таймером, кричит при устаревании.
#
# Правило хранения удаляет копии старше 30 дней. latest.dump под шаблон не
# подходит и не удаляется никогда — то есть последний удачный бэкап остаётся
# навсегда. Опасность не в том, что бэкапов не станет, а в том, что останется
# ОДИН, молча устаревающий: через месяц он месячной давности, и узнают об этом
# в худший момент.
#
# Поэтому проверяется не наличие, а ВОЗРАСТ.
set -uo pipefail

# Тревога уходит в Slack, а не только в журнал дроплета.
#
# Журнал — это дыра на уровень выше той, от которой защищаемся: проверка
# честно краснеет, и никто этого не видит, пока не зайдёт по ssh.
SLACK_URL="${SLACK_FN_URL:-https://us-central1-ifa-automation.cloudfunctions.net/sendSlackMessage}"
SLACK_CHANNEL="${SLACK_CHANNEL:-tech}"

alarm () {
  echo "ТРЕВОГА: $1"
  curl -fsS -X POST "$SLACK_URL" -H 'content-type: application/json' \
    --max-time 20 --data "$(printf '{"channel":"%s","message":"ntk ТРЕВОГА: %s"}' "$SLACK_CHANNEL" "$1")" \
    >/dev/null 2>&1 || echo "  (в Slack не ушло — смотрите журнал)"
  exit 1
}
: "${SPACES_BUCKET:?}" "${SPACES_ENDPOINT:?}" "${SPACES_REGION:?}"
: "${AWS_ACCESS_KEY_ID:?}" "${AWS_SECRET_ACCESS_KEY:?}"
MAX_HOURS="${MAX_BACKUP_AGE_HOURS:-30}"

head=$(curl -fsS -I --aws-sigv4 "aws:amz:${SPACES_REGION}:s3" \
  --user "${AWS_ACCESS_KEY_ID}:${AWS_SECRET_ACCESS_KEY}" \
  "${SPACES_ENDPOINT%/}/${SPACES_BUCKET}/postgres/latest.dump" 2>/dev/null) || {
    alarm "latest.dump недоступен — бэкапа нет вовсе"; }

mod=$(printf '%s' "$head" | grep -i '^last-modified:' | cut -d' ' -f2- | tr -d '\r')
size=$(printf '%s' "$head" | grep -i '^content-length:' | tr -dc '0-9')
age_h=$(( ( $(date -u +%s) - $(date -u -d "$mod" +%s) ) / 3600 ))

# Пустой дамп — это не бэкап. 10 КБ меньше любой осмысленной базы.
if [ "${size:-0}" -lt 10240 ]; then
  alarm "latest.dump всего ${size} байт — снимается пустая база"
fi
if [ "$age_h" -gt "$MAX_HOURS" ]; then
  alarm "последнему бэкапу ${age_h} ч при пороге ${MAX_HOURS} — ночной прогон не отработал"
fi
echo "бэкап свежий: ${age_h} ч назад, $((size/1024)) КБ"
