#!/usr/bin/env bash
# Подписывает и нотаризует клиентский бинарь для раздачи людям.
#
# Подпись идёт НА МАШИНЕ, где живёт ключ, а сюда возвращается готовый файл.
# Приватный ключ подписи не размножается по машинам: перенос .p12 сделал бы
# вторую копию там, где она не нужна.
#
# Ни машина, ни личность подписанта, ни файл с кредами здесь не записаны:
# в открытом репозитории это не секрет, но карта — куда идти за ключом.
# Всё приходит из окружения, и без него скрипт останавливается.
#
# Зачем это вообще: macOS вешает карантин на всё, что пришло из браузера или
# мессенджера. Неподписанный бинарь внутри .mcpb Gatekeeper не запустит, и
# юрист упрётся в «разработчик не может быть проверен» — ровно то, чего мы
# избегали, убирая терминал из его пути.
#
#   ./sign-macos.sh target/release/ntk
set -euo pipefail

BIN="${1:?укажи путь к бинарю}"
HOST="${SIGN_HOST:?укажи SIGN_HOST — машина, где лежит ключ подписи}"
SIGN_ID="${SIGN_ID:?укажи SIGN_ID, например 'Developer ID Application: Имя (TEAMID)'}"
# Файл с кредами нотаризации на машине подписи: APPLE_ID, APPLE_PASSWORD,
# APPLE_TEAM_ID и, если связка заперта, KEYCHAIN_PASSWORD.
SIGN_ENV="${SIGN_ENV:?укажи SIGN_ENV — путь к файлу с кредами на $HOST}"
REMOTE=/tmp/ntk-sign

[ -f "$BIN" ] || { echo "нет файла: $BIN"; exit 1; }
NAME="$(basename "$BIN")"

ssh "$HOST" "set -e
  [ -f $SIGN_ENV ] || { echo 'на $HOST нет $SIGN_ENV'; exit 1; }
  security find-identity -v -p codesigning | grep -q 'Developer ID Application' || {
    echo 'на $HOST не найден Developer ID Application — связка заблокирована?'; exit 1; }
  mkdir -p $REMOTE"

scp -q "$BIN" "$HOST:$REMOTE/$NAME"

ssh "$HOST" "set -e
  cd $REMOTE
  set -a; . $SIGN_ENV; set +a

  # По SSH login-keychain не разблокируется сам, и codesign не достаёт
  # приватный ключ: ошибка приходит как errSecInternalComponent, из которой
  # причина никак не следует. Разблокируем явно.
  if [ -n "\${KEYCHAIN_PASSWORD:-}" ]; then
    security unlock-keychain -p "\$KEYCHAIN_PASSWORD" ~/Library/Keychains/login.keychain-db
  else
    security show-keychain-info ~/Library/Keychains/login.keychain-db 2>/dev/null || {
      echo 'связка на $HOST заблокирована.'
      echo 'Разблокируйте её там, либо задайте KEYCHAIN_PASSWORD в $SIGN_ENV'
      exit 1; }
  fi

  # --options runtime обязателен: без hardened runtime нотаризация отклоняется.
  codesign --force --timestamp --options runtime --sign '$SIGN_ID' $NAME
  codesign -dvvv $NAME 2>&1 | grep -E 'Authority=|TeamIdentifier=' | sed 's/^/  /'

  # notarytool принимает zip, dmg или pkg — голый бинарь надо упаковать.
  ditto -c -k --keepParent $NAME $NAME.zip
  xcrun notarytool submit $NAME.zip \
    --apple-id \"\$APPLE_ID\" --password \"\$APPLE_PASSWORD\" \
    --team-id \"\${APPLE_TEAM_ID:?APPLE_TEAM_ID не задан в $SIGN_ENV}\" --wait

  # Степлер к голому бинарю неприменим: билет живёт у нотариуса, Gatekeeper
  # спросит о нём по сети. Для .mcpb степлить надо сам бандл, не бинарь.
  echo '  нотаризация завершена'"

scp -q "$HOST:$REMOTE/$NAME" "$BIN"
echo "подписан: $BIN"
codesign -dvvv "$BIN" 2>&1 | grep -E 'Authority=' | head -1 | sed 's/^/  /'
