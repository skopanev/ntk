#!/usr/bin/env bash
# Идентификатор тикета: непустой, по форме, с ВНЯТНЫМ отказом, и НИКАКОГО
# запасного пути через uuid. Плюс единство разрешения по регистру во всех ручках.
#
# Почему проверка нужна отдельно от схемы: PK и CHECK делают пустой id
# недостижимым, но это ограничение, а не доказательство поведения. Ограничение
# говорит «так не запишется», а вопрос был «как отвечает сервис и не находит ли
# он тикет обходным путём». Это разные утверждения; здесь проверяется второе.
#
#   ./id-integrity-test.sh [воркспейс]        по умолчанию test
#
# Пустой и короткий id через клиента не выразить, поэтому эти случаи идут сырым
# запросом. Адрес и ключ берутся оттуда же, откуда их берёт клиент.
set -euo pipefail
WS="${1:-test}"
NTK="${NTK:-ntk}"
PROJ="${ID_TEST_PROJECT:-test}"
CFG="$HOME/.config/ntk/config.json"

[ -f "$CFG" ] || { echo "нет $CFG — сначала ntk login"; exit 1; }
URL=$(python3 -c "import json;print(json.load(open('$CFG'))['url'])")
KEY=$(python3 -c "import json;print(json.load(open('$CFG'))['key'])")

# Метка УНИКАЛЬНА на прогон. С постоянной уборка захватывала бы чужие остатки, а
# параллельный прогон подменял бы проверяемый тикет своим.
MARK="id-integrity-$$-${RANDOM}"
RESP=$(mktemp)

ok=0; fail=0
check() { local d="$1" want="$2" got="$3"
  if [ "$want" = "$got" ]; then echo "ок    $d"; ok=$((ok+1))
  else echo "ПРОВАЛ $d — ожидали '$want', получили '$got'"; fail=$((fail+1)); fi; }

cleanup() {
  local ids
  ids=$("$NTK" ls -W "$WS" -P "$PROJ" --all -t "$MARK" --strict --json 2>/dev/null \
        | python3 -c 'import sys,json;print(" ".join(t["id"] for t in json.load(sys.stdin)))' 2>/dev/null || true)
  for t in $ids; do "$NTK" rm "$t" -W "$WS" -y >/dev/null 2>&1 || true; done
  rm -f "$RESP"
}
trap cleanup EXIT

post() { # тело -> код; ответ остаётся в $RESP
  curl -s -o "$RESP" -w '%{http_code}' -X POST "$URL/v1/tickets" \
    -H "Authorization: Bearer $KEY" -H 'content-type: application/json' -d "$1"
}
field() { python3 -c "import sys,json;print(json.load(sys.stdin).get('$1',''))" < "$RESP"; }
upper() { printf '%s' "$1" | tr 'a-z' 'A-Z'; }

# Тело собирается printf в переменную, а не пишется литералом в аргументе:
# {a,b} в оболочке — это РАСКРЫТИЕ СКОБОК, и JSON превращался в два слова, до
# сервиса доезжало тело без скобок, а он отвечал 422 «не разобрал» вместо 400.
# Проверка при этом краснела на верном поведении сервиса.
base='"workspace":"'"$WS"'","title":"id-integrity","project":"'"$PROJ"'","priority":"low","tags":["'"$MARK"'"]'
BODY_EMPTY=$(printf '{%s,"id":""}' "$base")
BODY_SHORT=$(printf '{%s,"id":"ab"}' "$base")
BODY_NULL=$(printf '{%s,"id":null}' "$base")
BODY_NONE=$(printf '{%s}' "$base")

# 1-2. Пустой и короткий отвергаются, и отказ НАЗЫВАЕТ поле.
#      Проверяется не только код: любой 400 давал бы зелёный, а нам нужно, чтобы
#      человек по ответу понял, что чинить. Прежняя версия выбрасывала тело.
for pair in "пустой:$BODY_EMPTY" "короткий:$BODY_SHORT"; do
  what="${pair%%:*}"; body="${pair#*:}"
  check "$what id отвергнут"            400 "$(post "$body")"
  reason=$(field error)
  check "$what: причина непуста"        "да" "$([ -n "$reason" ] && echo да || echo нет)"
  check "$what: причина называет поле"  "да" "$(printf '%s' "$reason" | grep -qi 'идентификатор' && echo да || echo нет)"
  check "$what: причина не простыня"    "да" "$([ "${#reason}" -le 300 ] && echo да || echo нет)"
done

# 3. ОТСУТСТВИЕ id и ЯВНЫЙ null — оба означают «назначь сам», и это не то же
#    самое, что пустая строка. Путались когда-то именно они.
check "без id тикет заводится"      201 "$(post "$BODY_NONE")"
ID_NONE=$(field id)
check "явный id:null заводится"     201 "$(post "$BODY_NULL")"
ID_NULL=$(field id)
# Идентификатор берётся ИЗ ОТВЕТА, а не из выборки: выборка могла бы вернуть
# чужой тикет и проверка молча ушла бы не туда.
for v in "$ID_NONE" "$ID_NULL"; do
  check "назначенный id по форме" "да" \
        "$(printf '%s' "$v" | grep -qE '^[A-Za-z0-9_-]{3,64}$' && echo да || echo нет)"
done

# 4. Запасного пути через uuid НЕТ: один тикет не находится двумя именами.
UUID=$("$NTK" show "$ID_NONE" -W "$WS" --json \
        | python3 -c 'import sys,json;d=json.load(sys.stdin);d=d[0] if isinstance(d,list) else d;print(d["uuid"])')
check "по uuid тикет НЕ находится" "нет" \
      "$("$NTK" show "$UUID" -W "$WS" --json >/dev/null 2>&1 && echo да || echo нет)"
check "по id тикет находится"      "да" \
      "$("$NTK" show "$ID_NONE" -W "$WS" --json >/dev/null 2>&1 && echo да || echo нет)"

# 5. Единство разрешения: одно имя означает одно и то же во ВСЕХ ручках.
#    show и deps искали через lower(id), а правка, захват, закрытие и удаление
#    сравнивали точно и отвечали «такого тикета нет» про существующий тикет.
U=$(upper "$ID_NONE")
check "show по верхнему регистру"   "да" "$("$NTK" show   "$U" -W "$WS" --json >/dev/null 2>&1 && echo да || echo нет)"
check "deps по верхнему регистру"   "да" "$("$NTK" deps   "$U" -W "$WS"        >/dev/null 2>&1 && echo да || echo нет)"
check "update по верхнему регистру" "да" "$("$NTK" update "$U" -W "$WS" -p high >/dev/null 2>&1 && echo да || echo нет)"
check "close по верхнему регистру"  "да" "$("$NTK" close  "$U" -W "$WS"        >/dev/null 2>&1 && echo да || echo нет)"
V=$(upper "$ID_NULL")
check "start по верхнему регистру"  "да" "$("$NTK" start  "$V" -W "$WS"        >/dev/null 2>&1 && echo да || echo нет)"
check "rm по верхнему регистру"     "да" "$("$NTK" rm     "$V" -W "$WS" -y     >/dev/null 2>&1 && echo да || echo нет)"

# И наоборот: несуществующее имя по-прежнему честно отвергается.
check "несуществующий id отвергнут" "нет" \
      "$("$NTK" show "${PROJ}-nesushestvuet99" -W "$WS" --json >/dev/null 2>&1 && echo да || echo нет)"

echo
echo "ок: $ok, провалов: $fail"
[ "$fail" -eq 0 ]
