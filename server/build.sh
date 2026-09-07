#!/usr/bin/env bash
# Сборка идёт НА ДРОПЛЕТЕ, нативно.
#
# Почему не кросс-компиляция в Docker, как задумывалось сначала: машина
# разработчика на Apple Silicon, целевая — x86_64. Контейнер под arm64 не
# умеет линковать x86_64 без отдельного линкера, а запуск контейнера через
# --platform linux/amd64 уходит в эмуляцию: пустой crate не собрался и за две
# минуты, а здесь сотни крейтов. Нативная сборка на целевой машине проще и
# честнее. Цена — тулчейн на сервере; в рантайме он не участвует.
#
#   ./build.sh <крейт>        например: ./build.sh ntk-api
set -euo pipefail
CRATE="${1:?укажи крейт}"
# Исходники по умолчанию — этот же репозиторий: server/ лежит в нём, и
# отдельный путь на машине разработчика тут больше ничего не значит.
SRC="${NTK_SRC:-$(cd "$(dirname "$0")/.." && pwd)}"
HOST="${NTK_HOST:?укажи NTK_HOST, например root@203.0.113.10}"
CARGO=/root/.cargo/bin/cargo

rsync -az --delete \
  --exclude target/ --exclude .git/ --exclude node_modules/ \
  "$SRC/" "$HOST:/opt/ntk/src/"

# Миграции — часть выкатки сервиса, а не отдельный шаг, который можно забыть.
# Забыли один раз: выкатили код, ждущий новых колонок, и самый ходовой запрос
# отвечал «внутренняя ошибка», пока миграции лежали неприменёнными.
if [ "$CRATE" = "ntk-api" ]; then
  rsync -az --delete --exclude target/ --exclude .git/ "$SRC/" "$HOST:/opt/ntk/src/"
  ssh "$HOST" "cd /opt/ntk/src && $CARGO build --release --bin ntk-migrate 2>&1 | tail -1
    install -m 755 target/release/ntk-migrate /opt/ntk/bin/ntk-migrate
    set -a; . /etc/ntk/admin.env; set +a
    /opt/ntk/bin/ntk-migrate"
fi

ssh "$HOST" "cd /opt/ntk/src && $CARGO build --release --bin $CRATE 2>&1 | tail -3
  install -d /opt/ntk/bin
  install -m 755 target/release/$CRATE /opt/ntk/bin/.$CRATE.new
  mv /opt/ntk/bin/.$CRATE.new /opt/ntk/bin/$CRATE
  ls -lh /opt/ntk/bin/$CRATE | awk '{print \"  бинарь:\", \$5}'"
