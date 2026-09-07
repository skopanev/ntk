#!/usr/bin/env bash
# Проверка снаружи. Запускать НЕ на дроплете — смысл в том, чтобы смотреть со стороны.
set -uo pipefail
HOST="${1:?укажи домен или IP}"

echo "== 5432 должен быть закрыт =="
if timeout 5 bash -c "</dev/tcp/$HOST/5432" 2>/dev/null; then
  echo "ПРОВАЛ: Postgres принимает соединения снаружи"; exit 1
else
  echo "ок: 5432 недоступен"
fi

echo "== 443 должен отвечать =="
curl -sS -o /dev/null -w 'HTTP %{http_code}, TLS %{ssl_verify_result}\n' "https://$HOST/" \
  || echo "443 не отвечает (ожидаемо, пока нет API — но TLS должен подниматься)"

echo "== сертификат =="
echo | openssl s_client -connect "$HOST:443" -servername "$HOST" 2>/dev/null \
  | openssl x509 -noout -issuer -dates 2>/dev/null || echo "сертификата ещё нет"
