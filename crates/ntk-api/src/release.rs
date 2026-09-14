//! Client releases: what counts as current and where to get it.
//!
//! The artefacts live in a private Spaces bucket; what goes out is not the file
//! but a pre-signed link with a short life. The Spaces keys never leave the
//! droplet and the bytes never pass through it: downloading is between the
//! client and the storage.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;

/// The client platform. Required, with no default.
///
/// darwin-arm64 used to be hard-wired here, and the server handed the macOS
/// binary to ANY client. A successful download on Linux would have replaced a
/// working binary with an incompatible one — the upgrade would have broken the
/// machine rather than fixed it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformQuery {
    platform: Option<String>,
    /// Конкретная версия вместо новейшей. Раньше этого поля не было вовсе, и
    /// `?version=…` молча проглатывался: спрашиваешь 0.5.60, получаешь свежую.
    /// Молчаливо проигнорированный параметр хуже отказа — он выглядит
    /// исполненным.
    version: Option<String>,
}

/// Имя платформы приводится к нашему написанию.
///
/// В докере архитектуру берут штатным путём — `TARGETARCH` или
/// `dpkg --print-architecture`, — и оба говорят `amd64`. Мы же выкладываем
/// `x86_64`, поэтому обычно написанный Dockerfile спрашивал `linux-amd64` и
/// получал 404, а сборка падала на curl. На arm64 словари совпадают случайно,
/// поэтому на Маке этого не видно вовсе — всплыло только на linux/amd64.
///
/// Переводим у себя, а не заставляем каждого держать свой перевод: тот, кто
/// его не напишет, получит не отказ, а непонятную 404.
fn normalise(p: &str) -> String {
    let (os, arch) = match p.split_once('-') {
        Some(v) => v,
        None => return p.to_string(),
    };
    let arch = match arch {
        "amd64" | "x64" | "x86-64" => "x86_64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

fn wanted(q: &PlatformQuery) -> Option<String> {
    q.platform
        .as_deref()
        .filter(|p| {
            !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .map(normalise)
}
use serde_json::json;

use crate::{spaces, App};

/// The current version for a platform. Answers WITHOUT a key: a client that has
/// not signed in yet must still be able to update itself, and knowing a version
/// number is not a secret.
pub async fn current(State(app): State<Arc<App>>, Query(q): Query<PlatformQuery>) -> Response {
    let Some(platform) = wanted(&q) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"name a platform — it cannot be guessed, and the wrong binary breaks the installation"})),
        )
            .into_response();
    };
    let Ok(c) = app.pool.get().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"the database is unavailable"}))).into_response();
    };
    let row = c
        .query_opt(
            "select version, object_key, sha256 from core.releases
              where platform = $1 and yanked_at is null
              order by published_at desc limit 1",
            &[&platform],
        )
        .await;

    match row {
        Ok(Some(r)) => {
            let version: String = r.get(0);
            let key: String = r.get(1);
            let sha256: String = r.get(2);
            match spaces::presign_get(&app.cfg, &key, 900) {
                Ok(url) => Json(json!({
                    "version": version,
                    "url": url,
                    "sha256": sha256,
                    "expires_in": 900
                }))
                .into_response(),
                Err(e) => {
                    tracing::error!(error = %e, "could not sign the release link");
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))).into_response()
                }
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("there are no releases for platform {platform}")})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "the release query failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))).into_response()
        }
    }
}

/// Download in one link: `curl -L https://…/v1/download -o ntk`.
///
/// The bucket is private, so a bare link does not work — the server signs one
/// and redirects to it. Without this a person would have to call /v1/version
/// first, parse the JSON and only then download, and in the instructions that
/// would read as "ask someone for the file".
pub async fn download(State(app): State<Arc<App>>, Query(q): Query<PlatformQuery>) -> Response {
    let Some(platform) = wanted(&q) else {
        return (StatusCode::BAD_REQUEST, "name a platform, e.g. ?platform=darwin-arm64").into_response();
    };
    let Ok(c) = app.pool.get().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "the database is unavailable").into_response();
    };
    // Названная версия отдаётся именно она, а не новейшая.
    let row = match q.version.as_deref().filter(|v| !v.is_empty()) {
        Some(v) => {
            c.query_opt(
                "select object_key from core.releases
                  where platform = $1 and version = $2 and yanked_at is null",
                &[&platform, &v],
            )
            .await
        }
        None => {
            c.query_opt(
                "select object_key from core.releases
                  where platform = $1 and yanked_at is null
                  order by published_at desc limit 1",
                &[&platform],
            )
            .await
        }
    };
    match row {
        Ok(Some(r)) => match spaces::presign_get(&app.cfg, &r.get::<_, String>(0), 900) {
            Ok(url) => Redirect::temporary(&url).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response(),
        },
        Ok(None) => (StatusCode::NOT_FOUND, "there are no releases").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response(),
    }
}

/// The install script: `curl -fsSL https://…/v1/install.sh | sh`.
///
/// WHY A SCRIPT AND NOT A GUESS ON THE SERVER. `platform` is required on the
/// other two handles precisely because guessing it once cost us an installation:
/// the platform was hard-wired, a Linux client was handed the macOS binary, the
/// download SUCCEEDED, and the upgrade broke the machine instead of fixing it.
/// Nothing here reverses that decision — the server still guesses nothing. The
/// script asks `uname` on the machine that will run the binary, which is the one
/// place where the answer is a fact rather than an inference.
///
/// IT REFUSES WHAT WE DO NOT BUILD. macOS on Intel is a real combination and we
/// publish no binary for it; the script says so and stops. Falling back to
/// "something close" is the exact failure this whole path exists to prevent.
///
/// IT VERIFIES. The checksum comes from /v1/version and is checked before the
/// file is made executable — the same order the client's own upgrade uses, and
/// for the same reason: an install path is a delivery channel for anything at
/// all, and the most convenient one, because it already has the right to write
/// a binary.
///
/// No key needed, like the other two: a client that has not signed in yet must
/// still be able to install itself.
pub async fn install_sh(State(app): State<Arc<App>>) -> Response {
    let base = app.cfg.public_url.trim_end_matches('/').to_string();
    let script = format!(
        r##"#!/bin/sh
# ntk installer.  Usage:  curl -fsSL {base}/v1/install.sh | sh
# Install elsewhere:      curl -fsSL {base}/v1/install.sh | NTK_DIR=/opt/bin sh
set -eu

BASE="{base}"
DIR="${{NTK_DIR:-/usr/local/bin}}"

os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux)  os_name=linux ;;
  Darwin) os_name=darwin ;;
  *) echo "ntk: no build for $os" >&2; exit 1 ;;
esac

case "$arch" in
  aarch64|arm64)  arch_name=arm64 ;;
  x86_64|amd64)   arch_name=x86_64 ;;
  *) echo "ntk: no build for $arch" >&2; exit 1 ;;
esac

platform="$os_name-$arch_name"

# We publish darwin-arm64, linux-x86_64 and linux-arm64. Anything else stops
# here rather than installing something close: a binary for the wrong platform
# downloads perfectly and then breaks the machine.
case "$platform" in
  darwin-arm64|linux-x86_64|linux-arm64) ;;
  *) echo "ntk: no build for $platform" >&2; exit 1 ;;
esac

echo "ntk: $platform"

meta=$(curl -fsSL "$BASE/v1/version?platform=$platform")
version=$(printf '%s' "$meta" | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
want=$(printf '%s'  "$meta" | sed -n 's/.*"sha256":"\([^"]*\)".*/\1/p')
[ -n "$version" ] && [ -n "$want" ] || {{ echo "ntk: the service gave no version for $platform" >&2; exit 1; }}

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
curl -fsSL "$BASE/v1/download?platform=$platform" -o "$tmp"

# Checked BEFORE it becomes executable.
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
  got=$(shasum -a 256 "$tmp" | cut -d' ' -f1)
else
  echo "ntk: no sha256sum or shasum — cannot verify the download" >&2; exit 1
fi
[ "$got" = "$want" ] || {{ echo "ntk: checksum mismatch: expected $want, got $got" >&2; exit 1; }}

mkdir -p "$DIR"
chmod 755 "$tmp"
mv "$tmp" "$DIR/ntk"
trap - EXIT

echo "ntk $version installed in $DIR/ntk"
"##
    );
    (
        StatusCode::OK,
        [("content-type", "text/x-shellscript; charset=utf-8")],
        script,
    )
        .into_response()
}

#[cfg(test)]
mod platform_tests {
    use super::normalise;

    /// Докерное имя обязано работать без перевода на стороне того, кто нас
    /// ставит: TARGETARCH и `dpkg --print-architecture` говорят `amd64`.
    #[test]
    fn the_docker_word_for_the_arch_is_accepted() {
        assert_eq!(normalise("linux-amd64"), "linux-x86_64");
        assert_eq!(normalise("darwin-amd64"), "darwin-x86_64");
    }

    /// И обратная пара: uname говорит `aarch64`, мы выкладываем `arm64`.
    #[test]
    fn the_uname_word_for_the_arch_is_accepted() {
        assert_eq!(normalise("linux-aarch64"), "linux-arm64");
    }

    /// Наше собственное написание не трогается.
    #[test]
    fn our_own_spelling_survives_untouched() {
        assert_eq!(normalise("linux-x86_64"), "linux-x86_64");
        assert_eq!(normalise("darwin-arm64"), "darwin-arm64");
        assert_eq!(normalise("linux-arm64"), "linux-arm64");
    }

    /// Незнакомое не выдумывается: пусть лучше будет честная 404, чем подмена
    /// на «что-то похожее». Не тот бинарь ломает машину, а не установку.
    #[test]
    fn an_unknown_arch_is_left_alone() {
        assert_eq!(normalise("linux-riscv64"), "linux-riscv64");
        assert_eq!(normalise("plan9-386"), "plan9-386");
        assert_eq!(normalise("nodash"), "nodash");
    }
}
