-- OAuth 2.1 для удалённого MCP: Claude Desktop подключается по URL.
--
-- Клиент регистрируется сам (RFC 7591): заранее мы его не знаем, а просить
-- человека завести приложение руками — это ровно тот барьер, из-за которого
-- удалённый коннектор перестаёт быть «вставил ссылку и работает».
--
-- Своих паролей здесь нет: личность по-прежнему подтверждает Google, а мы лишь
-- выдаём токен на уже опознанного человека.

create table if not exists core.oauth_clients (
  client_id      text primary key,
  client_name    text,
  redirect_uris  text[] not null,
  created_at     timestamptz not null default now(),
  -- Регистрация открыта всему интернету, поэтому запись о том, кто её сделал,
  -- нужна для разбора, а не для красоты.
  created_ip     text
);

-- Код авторизации: живёт минуты и погибает при первом обмене.
create table if not exists core.oauth_codes (
  code           text primary key,
  client_id      text not null references core.oauth_clients(client_id) on delete cascade,
  user_id        text not null references core.users(id),
  redirect_uri   text not null,
  -- PKCE обязателен: без него перехваченный код обменивается кем угодно.
  code_challenge text not null,
  -- RFC 8707: токен привязан к ресурсу, для которого запрошен.
  resource       text,
  scope          text,
  expires_at     timestamptz not null,
  used_at        timestamptz
);

-- Токен хранится хешем, как и ключи: утечка дампа не должна давать доступ.
create table if not exists core.oauth_tokens (
  token_hash     text primary key,
  client_id      text not null references core.oauth_clients(client_id) on delete cascade,
  user_id        text not null references core.users(id),
  kind           text not null check (kind in ('access','refresh')),
  resource       text,
  expires_at     timestamptz,
  revoked_at     timestamptz,
  created_at     timestamptz not null default now()
);
create index if not exists oauth_tokens_user on core.oauth_tokens (user_id) where revoked_at is null;

-- Состояние похода в Google: между /oauth/authorize и возвратом из Google надо
-- помнить, кому и куда потом возвращаться.
create table if not exists core.oauth_states (
  state          text primary key,
  client_id      text not null references core.oauth_clients(client_id) on delete cascade,
  redirect_uri   text not null,
  code_challenge text not null,
  client_state   text,
  resource       text,
  scope          text,
  expires_at     timestamptz not null
);

grant select, insert, update, delete on core.oauth_clients, core.oauth_codes,
      core.oauth_tokens, core.oauth_states to ntk_api;
