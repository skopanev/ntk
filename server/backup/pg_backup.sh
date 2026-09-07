#!/usr/bin/env bash
# Ночной дамп базы ntk в DigitalOcean Spaces. Запускается таймером systemd.
# Секреты берутся из /etc/ntk/backup.env (chmod 600), в репозиторий не попадают.
#
# Загрузка идёт обычным curl: он умеет подписывать S3-запросы сам (--aws-sigv4),
# начиная с 7.75. Отдельный S3-клиент не нужен — на коробке с 2 ГБ это был бы
# Python с boto3 ради одного PUT.
set -euo pipefail

: "${SPACES_BUCKET:?}" "${SPACES_ENDPOINT:?}" "${SPACES_REGION:?}"
: "${AWS_ACCESS_KEY_ID:?}" "${AWS_SECRET_ACCESS_KEY:?}"

STAMP="$(date -u +%Y-%m-%dT%H-%M-%SZ)"
FILE="/var/backups/ntk/ntk-${STAMP}.dump"
mkdir -p /var/backups/ntk

# Формат custom: сжат, восстанавливается выборочно через pg_restore.
sudo -u postgres pg_dump --format=custom --compress=9 ntk > "$FILE"

# Дамп нулевого размера — это провал, а не пустая база.
[ -s "$FILE" ] || { echo "ОШИБКА: дамп пустой"; exit 1; }

put() {
  curl -fsS --aws-sigv4 "aws:amz:${SPACES_REGION}:s3" \
       --user "${AWS_ACCESS_KEY_ID}:${AWS_SECRET_ACCESS_KEY}" \
       -T "$1" "${SPACES_ENDPOINT%/}/${SPACES_BUCKET}/$2"
}

put "$FILE" "postgres/$(basename "$FILE")"
# Указатель на последний дамп: restore-verify берёт его и не разбирает XML листинга.
put "$FILE" "postgres/latest.dump"

# Локально держим неделю.
find /var/backups/ntk -name 'ntk-*.dump' -mtime +7 -delete

# Старое в Spaces убирает правило жизненного цикла бакета: postgres/ntk-*
# живут 30 дней, latest.dump под шаблон не подходит и остаётся. Правило
# работает на стороне хранилища и не зависит от того, отработал ли этот скрипт.
#
# Раньше уборка была здесь, и это был второй механизм на ту же задачу: два
# таких однажды расходятся, и остаётся выяснять, какой прав.
echo "готово: $(basename "$FILE") ($(du -h "$FILE" | cut -f1))"
