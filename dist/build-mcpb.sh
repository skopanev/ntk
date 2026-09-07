#!/usr/bin/env bash
# Собирает .mcpb для Claude Desktop.
#
# Подпись проверяется НА БИНАРЕ ВНУТРИ БАНДЛА, а не только после выпуска.
# Первый бандл прошёл упаковку и отвечал по MCP с НЕПОДПИСАННЫМ бинарём:
# клиент пересобрали после релиза и упаковали свежую сборку. Юрист получил бы
# «разработчик не может быть проверен» и упёрся бы на первом шаге.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p ntk-cli 2>&1 | tail -1
./sign-macos.sh target/release/ntk | tail -1
install -d dist/mcpb/server
cp target/release/ntk dist/mcpb/server/ntk

OUT=$(spctl -a -vvv -t install dist/mcpb/server/ntk 2>&1 || true)
case "$OUT" in *"Notarized Developer ID"*) ;; *) echo "бинарь в бандле не нотаризован: $OUT"; exit 1;; esac

npx -y @anthropic-ai/mcpb pack dist/mcpb dist/ntk.mcpb | grep -E 'Output|package size'
