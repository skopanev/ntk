//! A copyable OAuth return link for clients running outside the browser's host.
use axum::{
    http::header,
    response::{Html, IntoResponse, Redirect, Response},
};
use base64::Engine;
use sha2::{Digest, Sha256};

const SCRIPT: &str = r#"const field=document.getElementById('callback-link');
document.getElementById('copy').addEventListener('click',async()=>{
  const status=document.getElementById('status');
  try{await navigator.clipboard.writeText(field.value);status.textContent='Copied. Paste it into your app to finish signing in.';}
  catch{field.focus();field.select();status.textContent='Select the link and copy it with Command+C or Ctrl+C.';}
});"#;

pub fn finish(redirect: &str, code: &str, state: Option<&str>) -> Response {
    let Ok(mut url) = reqwest::Url::parse(redirect) else {
        return crate::enroll::plain_page(
            "Sign-in failed",
            "The app supplied an invalid return address.",
        );
    };
    url.query_pairs_mut().append_pair("code", code);
    if let Some(state) = state {
        url.query_pairs_mut().append_pair("state", state);
    }
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !local {
        return Redirect::to(url.as_str()).into_response();
    }
    page(url.as_str())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn page(callback: &str) -> Response {
    let link = escape(callback);
    let html = format!(
        r#"<!doctype html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Finish signing in · NTK</title>
<style>html{{color-scheme:light}}*{{box-sizing:border-box}}body{{margin:0;background:#f3f5f2;color:#24372e;font:16px/1.6 system-ui,sans-serif;min-height:100vh;display:grid;place-items:center;padding:24px}}main{{width:100%;max-width:540px;background:#fff;border:1px solid #dae2dc;border-radius:20px;padding:36px}}.brand{{font-weight:750;letter-spacing:.08em;color:#51705b}}h1{{font-size:30px;line-height:1.2;margin:22px 0 12px}}p{{margin:12px 0}}textarea{{display:block;width:100%;height:110px;resize:vertical;margin:20px 0 14px;padding:12px;border:1px solid #cbd8cd;border-radius:10px;font:14px/1.5 ui-monospace,monospace;overflow-wrap:anywhere;color:inherit;background:#f7f9f6}}button{{width:100%;padding:13px;border:0;border-radius:10px;background:#294d39;color:white;font:600 16px system-ui;cursor:pointer}}button:focus-visible,a:focus-visible,textarea:focus-visible{{outline:3px solid #78a087;outline-offset:3px}}.hint,#status{{font-size:14px;color:#617468}}#status{{min-height:24px}}a{{color:#294d39}}@media(max-width:480px){{main{{padding:24px}}h1{{font-size:27px}}}}</style></head>
<body><main><div class="brand">NTK</div><h1>Finish signing in.</h1>
<p>Copy this link and paste it into the sign-in prompt in your app.</p>
<textarea id="callback-link" aria-label="Sign-in link" readonly spellcheck="false">{link}</textarea>
<button id="copy" type="button">Copy sign-in link</button><p id="status" role="status" aria-live="polite"></p>
<p class="hint">Keep the app's sign-in prompt open. This link expires in 5 minutes and can be used once.</p>
<p class="hint">App running on this computer? <a href="{link}" rel="noreferrer">Return to app</a></p>
</main><script>{SCRIPT}</script></body></html>"#
    );
    let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(SCRIPT.as_bytes()));
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY,
        format!("default-src 'none'; script-src 'sha256-{hash}'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'").parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::StatusCode};

    #[tokio::test]
    async fn loopback_clients_get_a_copyable_link_without_navigation() {
        for target in [
            "http://localhost:51485/callback",
            "http://127.0.0.1:8080/callback",
            "http://[::1]:8080/callback",
        ] {
            let response = finish(target, "test-code", Some("state&with=punctuation"));
            assert_eq!(response.status(), StatusCode::OK);
            assert!(!response.headers().contains_key(header::LOCATION));
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
            let body = String::from_utf8(
                to_bytes(response.into_body(), 100_000)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert!(body.contains("Copy sign-in link"));
            assert!(body.contains("code=test-code&amp;state=state%26with%3Dpunctuation"));
            assert!(!body.contains("window.location"));
        }
    }

    #[test]
    fn hosted_clients_keep_their_registered_redirect() {
        let response = finish(
            "https://app.example/callback?existing=yes",
            "test-code",
            Some("state"),
        );
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[header::LOCATION],
            "https://app.example/callback?existing=yes&code=test-code&state=state"
        );
        assert_eq!(
            finish("http://localhost.evil.example/callback", "c", None).status(),
            StatusCode::SEE_OTHER
        );
    }

    #[tokio::test]
    async fn callback_content_cannot_inject_markup_or_script() {
        let response =
            page("http://localhost/?code=</textarea><script>alert(1)</script>&state=\"'");
        let body = String::from_utf8(
            to_bytes(response.into_body(), 100_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(!body.contains("</textarea><script>alert"));
        assert!(body.contains("&lt;/textarea&gt;&lt;script&gt;"));
        assert!(body.contains("&quot;&#39;"));
    }
}
