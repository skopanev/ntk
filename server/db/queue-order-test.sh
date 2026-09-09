#!/usr/bin/env bash
# Порядок выдачи next: три свойства, которые обязаны выполняться ВМЕСТЕ.
#
# Порядок — сердце диспетчеризации, и он уже расходился с описанием молча:
# prefer стоял первым ключом и перекрывал подъём срочности, из-за чего блокеры
# начатого не выдавались, пока не несли нужного тега. Заметили это снаружи, на
# живой очереди, а не здесь. Поэтому проверка.
#
#   ./queue-order-test.sh [воркспейс]      по умолчанию test
#
# Работает ЧЕРЕЗ КЛИЕНТА, а не через базу: проверяется то, что видит вызывающий.
# Тикеты заводятся свои и убираются в конце, чужие не трогаются.
set -euo pipefail
WS="${1:-test}"
NTK="${NTK:-ntk}"
PROJ="${QUEUE_TEST_PROJECT:-test}"

ok=0; fail=0
check() { local d="$1" want="$2" got="$3"
  if [ "$want" = "$got" ]; then echo "ок    $d"; ok=$((ok+1))
  else echo "ПРОВАЛ $d — ожидали '$want', получили '$got'"; fail=$((fail+1)); fi; }

# Каждый заведённый тикет несёт метку, и уборка идёт ПО НЕЙ, а не по списку в
# переменной: mk вызывается через подстановку команды, то есть в подоболочке, и
# массив в родителя не вернётся. Первая версия этой проверки так и оставила три
# тикета после себя. Побочно метка убирает и остатки прошлого прогона, если он
# оборвался.
MARK=queue-order-test
mk() { # приоритет [доп. теги]
  "$NTK" create "queue-order-test $RANDOM" -W "$WS" -P "$PROJ" -p "$1" \
        -t "$MARK${2:+,$2}" --json \
    | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])'
}
peek() { # [аргументы next]
  "$NTK" next -W "$WS" --dry-run --json "$@" \
    | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d["id"] if d else "")'
}
cleanup() {
  local ids
  ids=$("$NTK" ls -W "$WS" -P "$PROJ" --all -t "$MARK" --strict --json 2>/dev/null \
        | python3 -c 'import sys,json;print(" ".join(t["id"] for t in json.load(sys.stdin)))' 2>/dev/null || true)
  for t in $ids; do "$NTK" rm "$t" -W "$WS" -y >/dev/null 2>&1 || true; done
}
trap cleanup EXIT

# Сцена: начатый low, который ждёт low-блокер. Блокер зависимости не имеет,
# поэтому пригоден к выдаче и наследует срочность начатого.
BLOCKER=$(mk low)
STARTED=$(mk low)
"$NTK" start "$STARTED" -W "$WS" >/dev/null
# Ребро добавляется ПОСЛЕ старта: заблокированный тикет начать нельзя.
"$NTK" update "$STARTED" -W "$WS" --deps "+$BLOCKER" --force >/dev/null

# 1. Шкала приоритетов НЕ переворачивается: high из бэклога обгоняет
#    low-блокер начатого low. Иначе одна забытая зависимость у низкого тикета
#    вытаскивала бы его вперёд всей очереди.
HIGH=$(mk high)
check "high из бэклога обгоняет low-блокер" "$HIGH" "$(peek)"
"$NTK" rm "$HIGH" -W "$WS" -y >/dev/null

# 2. Пожелание НИЖЕ блокера: среди равных по срочности блокер начатого идёт
#    раньше тикета, который просто удачно помечен.
WISH=$(mk low queue-order-wish)
check "блокер начатого обгоняет пожелание" "$BLOCKER" "$(peek --prefer queue-order-wish)"

# 3. Пожелание всё ещё РАБОТАЕТ там, где блокеров нет: оно не отсекает и не
#    стало бесполезным, а лишь уступило место блокерам.
"$NTK" update "$STARTED" -W "$WS" --deps "" --force >/dev/null
check "без блокеров пожелание решает" "$WISH" "$(peek --prefer queue-order-wish)"

echo
echo "ок: $ok, провалов: $fail"
[ "$fail" -eq 0 ]
