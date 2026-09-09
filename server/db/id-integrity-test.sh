#!/usr/bin/env bash
# Идентификатор тикета: непустой, по форме, и НИКАКОГО запасного пути через uuid.
#
# Почему проверка нужна отдельно от схемы: PK и CHECK делают пустой id
# недостижимым, но это ограничение, а не доказательство поведения. Ограничение
# говорит «так не запишется», а вопрос был «как отвечает сервис и не находит ли
# он тикет обходным путём». Выдавать схему за пройденную регрессию нельзя —
# это разные утверждения, и здесь проверяется второе.
#
#   ./id-integrity-test.sh [воркспейс]        по умолчанию test
#
# Пустой и короткий id через клиента не выразить, поэтому эти два случая идут
# сырым запросом. Адрес и ключ берутся оттуда же, откуда их берёт клиент.
set -euo pipefail
WS="${1:-test}"
NTK="${NTK:-ntk}"
PROJ="${ID_TEST_PROJECT:-test}"
CFG="$HOME/.config/ntk/config.json"

[ -f "$CFG" ] || { echo "нет $CFG — сначала ntk login"; exit 1; }
URL=$(python3 -c "import json;print(json.load(open('$CFG'))['url'])")
KEY=$(python3 -c "import json;print(json.load(open('$CFG'))['key'])")

ok=0; fail=0
check() { local d="$1" want="$2" got="$3"
  if [ "$want" = "$got" ]; then echo "ок    $d"; ok=$((ok+1))
  else echo "ПРОВАЛ $d — ожидали '$want', получили '$got'"; fail=$((fail+1)); fi; }

MARK=id-integrity-test
cleanup() {
  local ids
  ids=$("$NTK" ls -W "$WS" -P "$PROJ" --all -t "$MARK" --strict --json 2>/dev/null \
        | python3 -c 'import sys,json;print(" ".join(t["id"] for t in json.load(sys.stdin)))' 2>/dev/null || true)
  for t in $ids; do "$NTK" rm "$t" -W "$WS" -y >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

post() { # тело -> код ответа
  curl -s -o /dev/null -w '%{http_code}' -X POST "$URL/v1/tickets" \
    -H "Authorization: Bearer $KEY" -H 'content-type: application/json' -d "$1"
}
base='"workspace":"'"$WS"'","title":"id-integrity","project":"'"$PROJ"'","priority":"low","tags":["'"$MARK"'"]'

# 1-2. Пустой и слишком короткий отвергаются. Именно отвергаются, а не молча
#      подменяются выдуманным: подмена и была прежним дефектом.
# Тело собирается printf в переменную, а не пишется литералом в аргументе:
# {a,b} в оболочке — это РАСКРЫТИЕ СКОБОК, и JSON превращался в два слова, до
# сервиса доезжало тело без скобок, а он отвечал 422 «не разобрал» вместо 400.
# Проверка при этом «краснела» на верном поведении сервиса.
BODY_EMPTY=$(printf '{%s,"id":""}' "$base")
BODY_SHORT=$(printf '{%s,"id":"ab"}' "$base")
BODY_NONE=$(printf '{%s}' "$base")

check "пустой id отвергнут"        400 "$(post "$BODY_EMPTY")"
check "слишком короткий отвергнут" 400 "$(post "$BODY_SHORT")"

# 3. Отсутствие id — НЕ пустой id: сервер назначает свой. Эти два случая
#    когда-то путались, отсюда и «ложный запасной путь».
check "без id тикет заводится" 201 "$(post "$BODY_NONE")"

ID=$("$NTK" ls -W "$WS" -P "$PROJ" --all -t "$MARK" --strict --json \
      | python3 -c 'import sys,json;r=json.load(sys.stdin);print(r[0]["id"] if r else "")')
[ -n "$ID" ] || { echo "ПРОВАЛ: заведённый тикет не найден"; exit 1; }
check "назначенный id по форме" "да" \
      "$(printf '%s' "$ID" | grep -qE '^[A-Za-z0-9_-]{3,64}$' && echo да || echo нет)"

# 4. Запасного пути через uuid НЕТ. Ровно этот путь давал неоднозначность:
#    один тикет находился двумя разными именами.
UUID=$("$NTK" show "$ID" -W "$WS" --json \
        | python3 -c 'import sys,json;d=json.load(sys.stdin);d=d[0] if isinstance(d,list) else d;print(d["uuid"])')
check "по uuid тикет НЕ находится" "нет" \
      "$("$NTK" show "$UUID" -W "$WS" --json >/dev/null 2>&1 && echo да || echo нет)"
check "по id тикет находится"      "да" \
      "$("$NTK" show "$ID" -W "$WS" --json >/dev/null 2>&1 && echo да || echo нет)"

echo
echo "ок: $ok, провалов: $fail"
[ "$fail" -eq 0 ]
