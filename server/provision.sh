#!/usr/bin/env bash
# Первичная настройка дроплета под ntk 0.5.0. Идемпотентен: повторный запуск безопасен.
# Ubuntu 24.04 LTS. Запускать от root.
set -euo pipefail

DOMAIN="${DOMAIN:?укажи DOMAIN=tickets.example.com}"
PG_VERSION=16

log() { printf '\n== %s\n' "$*"; }

log "система"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get upgrade -y -qq
apt-get install -y -qq postgresql-$PG_VERSION postgresql-contrib ufw \
                       unattended-upgrades curl gnupg debian-keyring apt-transport-https

log "автообновления безопасности"
dpkg-reconfigure -f noninteractive unattended-upgrades

log "файрвол — наружу только SSH и HTTPS"
ufw --force reset >/dev/null
ufw default deny incoming
ufw default allow outgoing
ufw allow 22/tcp   comment 'ssh'
ufw allow 80/tcp   comment 'acme http-01'
ufw allow 443/tcp  comment 'api'
ufw deny 5432/tcp  comment 'postgres: никогда наружу'
ufw --force enable

log "сетевые очереди"
# Очередь незавершённых соединений по умолчанию 128. Всплеск в 500 агентов
# упирался в неё, и ядро ОТБРАСЫВАЛО лишние вместо того, чтобы поставить в
# очередь: клиент получал не отказ сервиса, а несостоявшееся соединение.
# Замерено: при 500 одновременных 301 запрос не дошёл вовсе.
cat > /etc/sysctl.d/60-ntk.conf <<'CONF'
net.ipv4.tcp_max_syn_backlog = 4096
net.core.somaxconn = 4096
net.ipv4.tcp_abort_on_overflow = 0
CONF
sysctl -q --system

log "caddy"
if ! command -v caddy >/dev/null; then
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
    | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
    > /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -qq && apt-get install -y -qq caddy
fi

log "postgres: слушает только localhost"
install -o postgres -g postgres -m 644 \
  ./postgres/ntk.conf /etc/postgresql/$PG_VERSION/main/conf.d/ntk.conf
install -o postgres -g postgres -m 640 \
  ./postgres/pg_hba.conf /etc/postgresql/$PG_VERSION/main/pg_hba.conf
systemctl restart postgresql

log "роли и база"
# ntk_admin — миграции и создание схем. ntk_api — рабочая роль сервиса, без superuser.
sudo -u postgres psql -v ON_ERROR_STOP=1 <<'SQL'
select 'create role ntk_admin login'
 where not exists (select from pg_roles where rolname='ntk_admin')\gexec
select 'create role ntk_api login'
 where not exists (select from pg_roles where rolname='ntk_api')\gexec
select 'create database ntk owner ntk_admin'
 where not exists (select from pg_database where datname='ntk')\gexec

-- Без NOINHERIT роль-член получает права включающей роли АВТОМАТИЧЕСКИ, и
-- ntk_api читал бы любую схему воркспейса без SET ROLE. Изоляция превращается
-- в фикцию. На этом краснел server/db/isolation-test.sh, пока не добавили.
alter role ntk_api noinherit;
SQL
echo "!! пароли ролей задать вручную: \\password ntk_admin / ntk_api — в env-файл, не в репозиторий"

log "caddy"
install -m 644 ./caddy/Caddyfile /etc/caddy/Caddyfile
sed -i "s/__DOMAIN__/$DOMAIN/" /etc/caddy/Caddyfile
systemctl reload caddy || systemctl restart caddy

log "готово. API ещё не существует (юнит U7) — слот в Caddyfile зарезервирован."
echo "Проверь снаружи: ./verify.sh $DOMAIN"
