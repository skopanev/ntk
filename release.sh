#!/usr/bin/env bash
# Выпуск клиента: собрать, подписать, нотаризовать, положить в Spaces,
# зарегистрировать.
#
# Подпись обязательна не для красоты: клиент проверяет её ПОСЛЕ скачивания.
# Автообновление без этой проверки — канал доставки чего угодно на машины
# людей, самая опасная функция в любом клиенте.
set -euo pipefail
cd "$(dirname "$0")"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
PLATFORM=darwin-arm64
KEY="releases/${PLATFORM}/ntk-${VERSION}"
BIN=target/release/ntk
LINUX_PLATFORM=linux-x86_64
LINUX_KEY="releases/${LINUX_PLATFORM}/ntk-${VERSION}"
ARM_PLATFORM=linux-arm64
ARM_KEY="releases/${ARM_PLATFORM}/ntk-${VERSION}"
ARM_TARGET=aarch64-unknown-linux-musl

# Адрес дроплета — из окружения: репозиторий публичный, и молчаливое умолчание
# на боевую машину означало бы выкатку не туда у любого, кто просто склонировал.
HOST="${NTK_HOST:?укажи NTK_HOST, например root@192.0.2.10}"

# Версия уже выпущенная — не переиздаём.
#
# release.sh молча заменил содержимое 0.5.3 другим бинарём: bash на macOS не
# понимает адрес 0 в sed, поднятие версии не произошло, а скрипт этого не
# заметил. Выпуск — вещь, на которую ссылаются по номеру: один номер обязан
# всегда означать один и тот же файл.
EXISTING=$(ssh "$HOST" "set -a; . /etc/ntk/admin.env; set +a
  psql \"\$DATABASE_URL\" -tAc \"select count(*) from core.releases where version='${VERSION}'\"")
if [ "${EXISTING:-0}" != 0 ] && [ "${FORCE:-}" != 1 ]; then
  echo "версия ${VERSION} уже выпущена (${EXISTING} платформ). Поднимите номер в Cargo.toml." >&2
  exit 1
fi

echo "== сборка ${VERSION}"
cargo build --release -p ntk-cli 2>&1 | tail -1

echo "== подпись и нотаризация"
./sign-macos.sh "$BIN" | tail -2

echo "== проверка, что подпись легла"
# Вывод забираем целиком, а не через `grep -q`: тот закрывает канал на первом
# совпадении, codesign получает SIGPIPE и возвращает 141, а pipefail считает
# это провалом. Проверка падала на подписанном бинаре.
SIGN_OUT=$(codesign -dvvv "$BIN" 2>&1 || true)
case "$SIGN_OUT" in *"Developer ID Application"*) ;; *) echo "бинарь не подписан"; exit 1;; esac
GATE_OUT=$(spctl -a -vvv -t install "$BIN" 2>&1 || true)
case "$GATE_OUT" in *accepted*) ;; *) echo "Gatekeeper не принял: $GATE_OUT"; exit 1;; esac
case "$GATE_OUT" in *Notarized*) ;; *) echo "не нотаризован — Gatekeeper пустит только на этой машине"; exit 1;; esac

SHA=$(shasum -a 256 "$BIN" | cut -d' ' -f1)
echo "== sha256 ${SHA}"

echo "== в Spaces: ${KEY}"
ssh "$HOST" "mkdir -p /tmp/rel"
scp -q "$BIN" "$HOST":/tmp/rel/ntk
ssh "$HOST" "set -a; . /etc/ntk/api.env; set +a
  curl -fsS --aws-sigv4 \"aws:amz:\${SPACES_REGION}:s3\" \
       --user \"\${AWS_ACCESS_KEY_ID}:\${AWS_SECRET_ACCESS_KEY}\" \
       -T /tmp/rel/ntk \"\${SPACES_ENDPOINT}/\${SPACES_BUCKET}/${KEY}\"
  rm -f /tmp/rel/ntk"

# Проверяем, что объект ЛЁГ, прежде чем записывать версию.
#
# Скрипт регистрировал выпуск, не убедившись в загрузке: 0.5.1 появился в
# core.releases, а файла в хранилище не было. /v1/download отдавал NoSuchKey,
# и версия в базе врала о том, что можно скачать.
echo "== проверка, что объект на месте"
ssh "$HOST" "set -a; . /etc/ntk/api.env; set +a
  code=\$(curl -sS -o /dev/null -w '%{http_code}' -I --aws-sigv4 \"aws:amz:\${SPACES_REGION}:s3\" \
    --user \"\${AWS_ACCESS_KEY_ID}:\${AWS_SECRET_ACCESS_KEY}\" \
    \"\${SPACES_ENDPOINT}/\${SPACES_BUCKET}/${KEY}\")
  [ \"\$code\" = 200 ] || { echo \"  объект не загрузился: HTTP \$code\"; exit 1; }
  echo '  объект на месте'"

echo "== регистрация"
ssh "$HOST" "set -a; . /etc/ntk/admin.env; set +a
  psql \"\$DATABASE_URL\" -q -c \"insert into core.releases (version, object_key, sha256, platform)
       values ('${VERSION}', '${KEY}', '${SHA}', '${PLATFORM}')
       on conflict (version, platform) do update set object_key=excluded.object_key,
         sha256=excluded.sha256, published_at=now(), yanked_at=null\""
echo "выпущено: ${VERSION}"

# ---------------------------------------------------------------------------
# Linux собирается НА ДРОПЛЕТЕ: он и есть x86_64, тулчейн там уже стоит, а
# кросс-компиляция с Apple Silicon уходит в эмуляцию и не заканчивается.
#
# Подпись Apple здесь не нужна и не применима: Gatekeeper — механизм macOS.
# Целостность проверяется контрольной суммой, как и на любом Linux.
echo
echo "== сборка ${LINUX_PLATFORM} на дроплете"
rsync -az --delete --exclude target/ --exclude .git/ ./ "$HOST":/opt/ntk/src/
ssh "$HOST" "cd /opt/ntk/src && /root/.cargo/bin/cargo build --release --bin ntk 2>&1 | tail -1"

LINUX_SHA=$(ssh "$HOST" "sha256sum /opt/ntk/src/target/release/ntk | cut -d' ' -f1")
echo "== sha256 ${LINUX_SHA}"

echo "== в Spaces: ${LINUX_KEY}"
ssh "$HOST" "set -a; . /etc/ntk/api.env; set +a
  curl -fsS --aws-sigv4 \"aws:amz:\${SPACES_REGION}:s3\" \
       --user \"\${AWS_ACCESS_KEY_ID}:\${AWS_SECRET_ACCESS_KEY}\" \
       -T /opt/ntk/src/target/release/ntk \"\${SPACES_ENDPOINT}/\${SPACES_BUCKET}/${LINUX_KEY}\"
  code=\$(curl -sS -o /dev/null -w '%{http_code}' -I --aws-sigv4 \"aws:amz:\${SPACES_REGION}:s3\" \
    --user \"\${AWS_ACCESS_KEY_ID}:\${AWS_SECRET_ACCESS_KEY}\" \
    \"\${SPACES_ENDPOINT}/\${SPACES_BUCKET}/${LINUX_KEY}\")
  [ \"\$code\" = 200 ] || { echo \"  объект не загрузился: HTTP \$code\"; exit 1; }
  echo '  объект на месте'
  set -a; . /etc/ntk/admin.env; set +a
  psql \"\$DATABASE_URL\" -q -c \"insert into core.releases (version, object_key, sha256, platform)
       values ('${VERSION}', '${LINUX_KEY}', '${LINUX_SHA}', '${LINUX_PLATFORM}')
       on conflict (version, platform) do update set object_key=excluded.object_key, sha256=excluded.sha256, published_at=now()\""
echo "выпущено: ${VERSION} для ${LINUX_PLATFORM}"

# ---------------------------------------------------------------------------
# linux-arm64 — тоже на дроплете, но КРОСС-СБОРКОЙ и СТАТИЧЕСКИ, под musl.
#
# Статически не ради изящества: бинарь ставят в контейнер, и связка с glibc
# хозяина означала бы, что сборка годится ровно для того образа, где собрана.
# Debian, Alpine, чужая версия Ubuntu — с musl всё равно.
#
# Линкер берём aarch64-linux-gnu-gcc: под musl-цель он годится, потому что
# линкуется всё статически и системная библиотека хозяина не участвует. Ставить
# отдельную musl-цепочку под aarch64 ради этого незачем.
#
# Подпись Apple здесь не нужна и не применима, как и для x86_64: Gatekeeper —
# механизм macOS. Целостность проверяется контрольной суммой.
echo
echo "== сборка ${ARM_PLATFORM} на дроплете (кросс, статически)"
ssh "$HOST" "cd /opt/ntk/src
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
  export CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc
  /root/.cargo/bin/cargo build --release --bin ntk --target ${ARM_TARGET} 2>&1 | tail -1"

# Проверяем, ЧТО собрали, а не только что команда не упала: цель можно задать и
# получить хозяйский бинарь, если линкер молча подставит своё.
ssh "$HOST" "f=/opt/ntk/src/target/${ARM_TARGET}/release/ntk
  file \"\$f\" | grep -q 'ARM aarch64' || { echo '  не aarch64'; exit 1; }
  file \"\$f\" | grep -q 'statically linked' || { echo '  не статический'; exit 1; }
  echo '  aarch64, статический'"

ARM_SHA=$(ssh "$HOST" "sha256sum /opt/ntk/src/target/${ARM_TARGET}/release/ntk | cut -d' ' -f1")
echo "== sha256 ${ARM_SHA}"

echo "== в Spaces: ${ARM_KEY}"
ssh "$HOST" "set -a; . /etc/ntk/api.env; set +a
  curl -fsS --aws-sigv4 \"aws:amz:\${SPACES_REGION}:s3\" \
       --user \"\${AWS_ACCESS_KEY_ID}:\${AWS_SECRET_ACCESS_KEY}\" \
       -T /opt/ntk/src/target/${ARM_TARGET}/release/ntk \"\${SPACES_ENDPOINT}/\${SPACES_BUCKET}/${ARM_KEY}\"
  code=\$(curl -sS -o /dev/null -w '%{http_code}' -I --aws-sigv4 \"aws:amz:\${SPACES_REGION}:s3\" \
    --user \"\${AWS_ACCESS_KEY_ID}:\${AWS_SECRET_ACCESS_KEY}\" \
    \"\${SPACES_ENDPOINT}/\${SPACES_BUCKET}/${ARM_KEY}\")
  [ \"\$code\" = 200 ] || { echo \"  объект не загрузился: HTTP \$code\"; exit 1; }
  echo '  объект на месте'
  set -a; . /etc/ntk/admin.env; set +a
  psql \"\$DATABASE_URL\" -q -c \"insert into core.releases (version, object_key, sha256, platform)
       values ('${VERSION}', '${ARM_KEY}', '${ARM_SHA}', '${ARM_PLATFORM}')
       on conflict (version, platform) do update set object_key=excluded.object_key, sha256=excluded.sha256, published_at=now()\""
echo "выпущено: ${VERSION} для ${ARM_PLATFORM}"
