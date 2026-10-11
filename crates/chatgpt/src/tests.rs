use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::{rand::SystemRandom, signature};
use serde_json::{Value, json};
use url::Url;

const CLIENT: &str = "oaiapp_nuzky_test";
const EMAIL: &str = "editor@example.test";
const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const RESOURCE: &str = "https://api.openai.com/v1";
type Params = BTreeMap<String, String>;

fn params(query: &str) -> Params {
    url::form_urlencoded::parse(query.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("nuzky-chatgpt-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct SigningKey(signature::RsaKeyPair);
impl SigningKey {
    fn new() -> Self {
        let dir = TempDir::new();
        let path = dir.0.join("key.der");
        let output = Command::new("openssl")
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-outform", "DER", "-out"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        Self(signature::RsaKeyPair::from_pkcs8(&fs::read(path).unwrap()).unwrap())
    }
    fn jwks(&self) -> Value {
        let public: signature::RsaPublicKeyComponents<Vec<u8>> = self.0.public().into();
        json!({"keys": [{"kty": "RSA", "kid": "test-key", "alg": "RS256", "use": "sig",
            "n": URL_SAFE_NO_PAD.encode(public.n), "e": URL_SAFE_NO_PAD.encode(public.e)}]})
    }
    fn token(&self, header: Value, claims: &Value) -> String {
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap())
        );
        let mut sig = vec![0; self.0.public().modulus_len()];
        self.0.sign(&signature::RSA_PKCS1_SHA256, &SystemRandom::new(), signed.as_bytes(), &mut sig).unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig))
    }
}
fn key() -> &'static SigningKey {
    static KEY: OnceLock<SigningKey> = OnceLock::new();
    KEY.get_or_init(SigningKey::new)
}
fn claims(issuer: &str, nonce: &str) -> Value {
    json!({"iss": issuer, "aud": CLIENT, "azp": CLIENT, "exp": now() + 3600,
        "nonce": nonce, "sub": "test-editor", "email": EMAIL})
}
fn header() -> Value {
    json!({"alg": "RS256", "kid": "test-key"})
}

#[test]
fn chatgpt_pkce_and_fresh_attempt_secrets() {
    assert_eq!(
        oauth::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let a = Attempt::new("http://127.0.0.1:1234/auth/callback".into());
    let b = Attempt::new(a.redirect_uri.clone());
    let secrets = [&a.state, &a.nonce, &a.verifier, &b.state, &b.nonce, &b.verifier];
    for (i, secret) in secrets.iter().enumerate() {
        assert_eq!(secret.len(), 43);
        assert!(secret.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert_eq!(URL_SAFE_NO_PAD.decode(secret).unwrap().len(), 32);
        assert!(secrets[i + 1..].iter().all(|other| secret != other));
    }
}

#[test]
fn chatgpt_authorize_parameters_and_percent_encoding() {
    let attempt = Attempt::new("http://127.0.0.1:43210/auth/callback".into());
    let mut request = Authorize {
        client_id: None,
        host_id: "urn:uuid:test-host",
        id_token_hint: None,
        login_hint: None,
        consent: false,
    };
    let raw = oauth::authorize_url(ISSUER, &request, &attempt);
    assert!(raw.contains("openid%20profile%20email"));
    assert!(!raw.contains('+'));
    let url = Url::parse(&raw).unwrap();
    assert_eq!(url.path(), "/api/accounts/authorize");
    let mut expected = Params::from([
        ("client_id".into(), oauth::DYNAMIC_CLIENT.into()),
        ("agent_name_hint".into(), "Nuzky".into()),
        ("ext_agent_host_id".into(), request.host_id.into()),
        ("response_type".into(), "code".into()),
        ("redirect_uri".into(), attempt.redirect_uri.clone()),
        ("scope".into(), SCOPES.into()),
        ("resource".into(), RESOURCE.into()),
        ("state".into(), attempt.state.clone()),
        ("nonce".into(), attempt.nonce.clone()),
        ("code_challenge_method".into(), "S256".into()),
        ("code_challenge".into(), oauth::challenge(&attempt.verifier)),
    ]);
    assert_eq!(params(url.query().unwrap()), expected);
    request.client_id = Some(CLIENT);
    request.id_token_hint = Some("retained-id-token");
    request.login_hint = Some(EMAIL);
    expected.insert("client_id".into(), CLIENT.into());
    expected.remove("agent_name_hint");
    expected.insert("id_token_hint".into(), "retained-id-token".into());
    expected.insert("login_hint".into(), EMAIL.into());
    for consent in [false, true] {
        request.consent = consent;
        if consent {
            expected.insert("prompt".into(), "consent".into());
        }
        let raw = oauth::authorize_url(ISSUER, &request, &attempt);
        assert!(!raw.contains('+'));
        assert_eq!(params(Url::parse(&raw).unwrap().query().unwrap()), expected);
    }
}

#[test]
fn chatgpt_callback_state_registration_and_errors() {
    let parse = |query: &str, saved: Option<&str>| oauth::parse_callback(query, "expected", saved);
    let success = Callback::Code { code: "issued-code".into(), client_id: CLIENT.into() };
    assert_eq!(parse(&format!("state=expected&code=issued-code&client_id={CLIENT}"), None), Ok(success));
    assert_eq!(parse("state=expected&error=access_denied", None), Ok(Callback::Denied));
    for suffix in ["code=issued-code", "error=access_denied", "error=server_error"] {
        for prefix in ["", "state=wrong&"] {
            assert_eq!(parse(&format!("{prefix}{suffix}"), Some(CLIENT)), Err(CallbackError::WrongState));
        }
    }
    for query in [
        "state=expected&code=issued-code",
        "state=expected&code=issued-code&client_id=dynamic_agent_client",
        "state=expected&client_id=oaiapp_nuzky_test",
    ] {
        assert!(matches!(parse(query, None), Err(CallbackError::Failed(_))));
    }
    assert_eq!(
        parse("state=expected&code=issued-code", Some(CLIENT)),
        Ok(Callback::Code { code: "issued-code".into(), client_id: CLIENT.into() })
    );
    assert!(matches!(
        parse("state=expected&code=issued-code&client_id=oaiapp_other", Some(CLIENT)),
        Err(CallbackError::Failed(_))
    ));
    let Err(CallbackError::Failed(why)) =
        parse("state=expected&error=server_error&error_description=Try%20later", None)
    else {
        panic!("expected the OAuth error")
    };
    assert!(why.contains("Try later"));
}

#[test]
fn chatgpt_id_token_identity_and_rejected_claims() {
    let key = key();
    let jwks = serde_json::from_value(key.jwks()).unwrap();
    let timestamp = now();
    let valid = claims(ISSUER, "attempt-nonce");
    let verify = |claims: &Value| {
        oauth::verify_id_token(&key.token(header(), claims), &jwks, ISSUER, CLIENT, "attempt-nonce", timestamp)
    };
    assert_eq!(verify(&valid).unwrap(), oauth::Identity { subject: "test-editor".into(), email: Some(EMAIL.into()) });
    let mut array_audience = valid.clone();
    array_audience["aud"] = json!(["another-app", CLIENT]);
    assert!(verify(&array_audience).is_ok());
    let mut skew = valid.clone();
    skew["exp"] = json!(timestamp - 59);
    assert!(verify(&skew).is_ok());
    for (field, value) in [
        ("iss", json!("https://other.example.test")),
        ("aud", json!("another-app")),
        ("aud", json!(["another-app", "also-other"])),
        ("azp", json!("another-app")),
        ("exp", json!(timestamp - 61)),
        ("nonce", json!("another-attempt")),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        assert!(verify(&bad).is_err(), "accepted invalid {field}");
    }
}

#[test]
fn chatgpt_id_token_signature_key_algorithm_and_malformed_input() {
    let key = key();
    let valid = claims(ISSUER, "attempt-nonce");
    let jwks = serde_json::from_value(key.jwks()).unwrap();
    let verify = |token: &str| oauth::verify_id_token(token, &jwks, ISSUER, CLIENT, "attempt-nonce", now());
    let original = key.token(header(), &valid);
    let parts: Vec<_> = original.split('.').collect();
    let mut changed = valid.clone();
    changed["email"] = json!("intruder@example.test");
    let tampered =
        format!("{}.{}.{}", parts[0], URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).unwrap()), parts[2]);
    assert!(verify(&tampered).is_err());
    assert!(verify(&key.token(json!({"alg": "RS256", "kid": "unknown-key"}), &valid)).is_err());
    assert!(verify(&SigningKey::new().token(header(), &valid)).is_err());
    for alg in ["none", "HS256"] {
        assert!(verify(&key.token(json!({"alg": alg, "kid": "test-key"}), &valid)).is_err());
    }
    for malformed in ["", "not-a-jwt", "a.b", "a.b.c", "a.b.c.d"] {
        assert!(verify(malformed).is_err());
    }
}

struct ServerState {
    authorize: Vec<Params>,
    token_requests: Vec<Params>,
    revokes: Vec<Params>,
    refresh_error: Option<(u16, Value)>,
    revoke_status: u16,
    denied: bool,
    plan: bool,
    expires_in: u64,
    generation: u64,
    current_refresh: String,
    last_response: Value,
}
impl Default for ServerState {
    fn default() -> Self {
        Self {
            authorize: Vec::new(),
            token_requests: Vec::new(),
            revokes: Vec::new(),
            refresh_error: None,
            revoke_status: 200,
            denied: false,
            plan: true,
            expires_in: 3600,
            generation: 0,
            current_refresh: String::new(),
            last_response: Value::Null,
        }
    }
}

struct FakeOpenAi {
    issuer: String,
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl FakeOpenAi {
    fn new() -> Self {
        let _ = key();
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let issuer = format!("http://{}", socket.local_addr().unwrap());
        let state = Arc::new(Mutex::new(ServerState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopped, origin) = (state.clone(), stop.clone(), issuer.clone());
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match socket.accept() {
                    Ok((mut stream, _)) => {
                        let (method, target, body) = read_request(&mut stream);
                        let url = Url::parse(&format!("{origin}{target}")).unwrap();
                        let (status, location, body) =
                            serve(&method, &url, &body, &origin, &mut shared.lock().unwrap());
                        write!(stream,
                            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{body}",
                            body.len(), location.map(|l| format!("Location: {l}\r\n")).unwrap_or_default()
                        ).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("accepting the test request: {error}"),
                }
            }
        });
        Self { issuer, state, stop, worker: Some(worker) }
    }
}
impl Drop for FakeOpenAi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> (String, String, String) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
        assert!(bytes.len() < 64 * 1024);
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut request = headers.lines().next().unwrap().split_whitespace();
    let method = request.next().unwrap().to_owned();
    let target = request.next().unwrap().to_owned();
    let length: usize = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map_or(0, |(_, value)| value.trim().parse().unwrap());
    assert!(length < 64 * 1024);
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "request ended before body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    (method, target, String::from_utf8(bytes[header_end..header_end + length].to_vec()).unwrap())
}

fn serve(method: &str, url: &Url, body: &str, issuer: &str, s: &mut ServerState) -> (u16, Option<String>, String) {
    match (method, url.path()) {
        ("GET", "/api/accounts/authorize") => {
            let query = params(url.query().unwrap());
            let mut redirect = Url::parse(&query["redirect_uri"]).unwrap();
            assert_eq!(redirect.host_str(), Some("127.0.0.1"));
            assert_eq!(redirect.path(), "/auth/callback");
            let mut callback = redirect.query_pairs_mut();
            callback.append_pair("state", &query["state"]);
            if s.denied {
                callback.append_pair("error", "access_denied");
            } else {
                callback.append_pair("code", "test-code").append_pair("scope", SCOPES);
                if query["client_id"] == oauth::DYNAMIC_CLIENT {
                    callback.append_pair("client_id", CLIENT);
                }
            }
            drop(callback);
            s.authorize.push(query);
            (302, Some(redirect.into()), String::new())
        }
        ("GET", "/.well-known/jwks.json") => (200, None, key().jwks().to_string()),
        ("POST", "/api/accounts/oauth/token") => {
            let form = params(body);
            s.token_requests.push(form.clone());
            assert_eq!(form["client_id"], CLIENT);
            assert_eq!(form["resource"], RESOURCE);
            let refresh = form["grant_type"] == "refresh_token";
            if refresh {
                assert!(!form.contains_key("scope"));
                if let Some((status, body)) = &s.refresh_error {
                    return (*status, None, body.to_string());
                }
                if form["refresh_token"] != s.current_refresh {
                    return (400, None, json!({"error": "invalid_grant"}).to_string());
                }
            } else {
                assert_eq!(form["grant_type"], "authorization_code");
                let auth = s.authorize.last().unwrap();
                assert_eq!(form["code"], "test-code");
                assert_eq!(form["redirect_uri"], auth["redirect_uri"]);
                assert_eq!(auth["code_challenge_method"], "S256");
                assert_eq!(oauth::challenge(&form["code_verifier"]), auth["code_challenge"]);
            }
            s.generation += 1;
            s.current_refresh = format!("test-refresh-{}", s.generation);
            let scope = if s.plan { SCOPES } else { "openid profile email offline_access resource.invoke" };
            let scope = if refresh { format!("{scope} refreshed-{}", s.generation) } else { scope.into() };
            let id_token = key().token(header(), &claims(issuer, &s.authorize.last().unwrap()["nonce"]));
            s.last_response = json!({"access_token": format!("test-access-{}", s.generation),
                "refresh_token": s.current_refresh, "id_token": id_token,
                "expires_in": s.expires_in + if refresh { s.generation } else { 0 }, "scope": scope});
            (200, None, s.last_response.to_string())
        }
        ("POST", "/api/accounts/oauth/revoke") => {
            s.revokes.push(params(body));
            (s.revoke_status, None, String::new())
        }
        _ => panic!("unexpected test request: {method} {}", url.path()),
    }
}

struct Flow {
    server: FakeOpenAi,
    dir: TempDir,
}
impl Flow {
    fn new() -> Self {
        Self { server: FakeOpenAi::new(), dir: TempDir::new() }
    }
    fn app(&self) -> ChatGpt {
        ChatGpt::with(self.server.issuer.clone(), self.dir.0.clone(), Vault::File(self.dir.0.join("tokens.json")))
    }
    fn tokens_path(&self) -> PathBuf {
        self.dir.0.join("tokens.json")
    }
    fn tokens(&self) -> Value {
        serde_json::from_slice(&fs::read(self.tokens_path()).unwrap()).unwrap()
    }
    fn sign_in(&self) -> Result<Status> {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut browser = None;
        let result = self.app().sign_in(false, &cancel, |url| {
            let url = url.to_owned();
            let cancel = cancel.clone();
            browser = Some(thread::spawn(move || {
                let http: ureq::Agent =
                    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into();
                let result = http.get(&url).call().and_then(|mut r| r.body_mut().read_to_string());
                if result.is_err() {
                    cancel.store(true, Ordering::Relaxed);
                }
                result
            }));
            Ok(())
        });
        let page = browser.unwrap().join().unwrap().unwrap();
        assert!(page.contains(if result.is_ok() { listener::PAGE_DONE } else { listener::PAGE_DENIED }));
        result
    }
}
fn signed_in(plan: bool) -> Status {
    Status::SignedIn { email: Some(EMAIL.into()), plan }
}

#[test]
fn chatgpt_first_and_returning_sign_in_keep_registration_separate() {
    let f = Flow::new();
    assert_eq!(f.sign_in().unwrap(), signed_in(true));
    let reg_text = fs::read_to_string(f.dir.0.join("chatgpt.json")).unwrap();
    let reg: Value = serde_json::from_str(&reg_text).unwrap();
    assert_eq!(reg["client_id"], CLIENT);
    let host = reg["host_id"].as_str().unwrap();
    uuid::Uuid::parse_str(host.strip_prefix("urn:uuid:").unwrap()).unwrap();
    let tokens = f.tokens();
    for name in ["access_token", "refresh_token", "id_token"] {
        assert_eq!(tokens[name], f.server.state.lock().unwrap().last_response[name]);
        assert!(!reg_text.contains(tokens[name].as_str().unwrap()));
        assert!(reg.get(name).is_none());
    }
    assert_eq!(f.sign_in().unwrap(), signed_in(true));
    let s = f.server.state.lock().unwrap();
    assert_eq!(s.authorize.len(), 2);
    assert_eq!(s.authorize[0]["client_id"], oauth::DYNAMIC_CLIENT);
    assert_eq!(s.authorize[0]["agent_name_hint"], "Nuzky");
    let returning = &s.authorize[1];
    assert_eq!(returning["client_id"], CLIENT);
    assert_eq!(returning["id_token_hint"], tokens["id_token"].as_str().unwrap());
    assert_eq!(returning["login_hint"], EMAIL);
    assert!(!returning.contains_key("agent_name_hint"));
    assert!(!returning.contains_key("prompt"));
    assert_eq!(returning["ext_agent_host_id"], host);
    assert_eq!(s.authorize[0]["ext_agent_host_id"], host);
}

#[test]
fn chatgpt_restart_uses_fresh_tokens_without_a_token_request() {
    let f = Flow::new();
    assert_eq!(f.sign_in().unwrap(), signed_in(true));
    let before = fs::read(f.tokens_path()).unwrap();
    let requests = f.server.state.lock().unwrap().token_requests.len();
    assert_eq!(f.app().status().unwrap(), signed_in(true));
    assert_eq!(f.server.state.lock().unwrap().token_requests.len(), requests);
    assert_eq!(fs::read(f.tokens_path()).unwrap(), before);
}

#[test]
fn chatgpt_refresh_rotation_saves_the_whole_replacement() {
    let f = Flow::new();
    f.server.state.lock().unwrap().expires_in = 60;
    f.sign_in().unwrap();
    let original = f.tokens();
    for generation in 2..=3 {
        let before = now();
        assert_eq!(f.app().status().unwrap(), signed_in(true));
        let after = now();
        let tokens = f.tokens();
        let s = f.server.state.lock().unwrap();
        for field in ["access_token", "refresh_token", "id_token"] {
            assert_eq!(tokens[field], s.last_response[field]);
        }
        let expiry = tokens["expires_at"].as_u64().unwrap();
        assert!((before + 60 + generation..=after + 60 + generation).contains(&expiry));
        assert_ne!(tokens["expires_at"], original["expires_at"]);
        assert_eq!(
            tokens["scopes"],
            json!(s.last_response["scope"].as_str().unwrap().split_whitespace().collect::<Vec<_>>())
        );
        assert_ne!(tokens["scopes"], original["scopes"]);
        assert_eq!(s.token_requests.last().unwrap()["refresh_token"], format!("test-refresh-{}", generation - 1));
    }
    let s = f.server.state.lock().unwrap();
    assert_eq!(s.token_requests.len(), 3);
    assert_eq!(s.token_requests[1]["refresh_token"], "test-refresh-1");
    assert_eq!(s.token_requests[2]["refresh_token"], "test-refresh-2");
}

#[test]
fn chatgpt_refused_refresh_clears_tokens_and_keeps_email() {
    for error in [json!({"error": "invalid_grant"}), json!({"error": {"code": "refresh_token_reused"}})] {
        let f = Flow::new();
        f.server.state.lock().unwrap().expires_in = 60;
        f.sign_in().unwrap();
        f.server.state.lock().unwrap().refresh_error = Some((400, error));
        assert_eq!(f.app().status().unwrap(), signed_out(Some(EMAIL.into()), Some(Notice::Ended)));
        assert!(!f.tokens_path().exists());
        assert_eq!(f.app().status().unwrap(), signed_out(Some(EMAIL.into()), None));
        assert_eq!(f.server.state.lock().unwrap().token_requests.len(), 2);
    }
}

#[test]
fn chatgpt_refresh_503_preserves_sign_in_and_tokens() {
    let f = Flow::new();
    f.server.state.lock().unwrap().expires_in = 60;
    f.sign_in().unwrap();
    let before = fs::read(f.tokens_path()).unwrap();
    f.server.state.lock().unwrap().refresh_error = Some((503, json!({"error": "unavailable"})));
    assert_eq!(f.app().status().unwrap(), signed_in(true));
    assert_eq!(fs::read(f.tokens_path()).unwrap(), before);
    assert_eq!(f.server.state.lock().unwrap().token_requests.len(), 2);
}

#[test]
fn chatgpt_missing_plan_scope_requests_consent_next_time() {
    let f = Flow::new();
    f.server.state.lock().unwrap().plan = false;
    assert_eq!(f.sign_in().unwrap(), signed_in(false));
    assert_eq!(f.app().status().unwrap(), signed_in(false));
    assert_eq!(f.sign_in().unwrap(), signed_in(false));
    let s = f.server.state.lock().unwrap();
    assert!(!s.authorize[0].contains_key("prompt"));
    assert_eq!(s.authorize[1]["prompt"], "consent");
    assert_eq!(s.authorize[1]["client_id"], CLIENT);
    assert_eq!(s.authorize[1]["scope"], SCOPES);
}

#[test]
fn chatgpt_sign_out_revokes_and_clears_tokens_even_on_503() {
    for status in [200, 503] {
        let f = Flow::new();
        f.sign_in().unwrap();
        let tokens = f.tokens();
        f.server.state.lock().unwrap().revoke_status = status;
        assert_eq!(
            f.app().sign_out().unwrap(),
            signed_out(Some(EMAIL.into()), (status == 503).then_some(Notice::NotRevoked))
        );
        assert!(!f.tokens_path().exists());
        assert_eq!(f.app().status().unwrap(), signed_out(Some(EMAIL.into()), None));
        let s = f.server.state.lock().unwrap();
        assert_eq!(s.revokes.len(), if status == 200 { 1 } else { 3 });
        for revoke in &s.revokes {
            assert_eq!(
                revoke,
                &Params::from([
                    ("token".into(), tokens["refresh_token"].as_str().unwrap().into()),
                    ("token_type_hint".into(), "refresh_token".into()),
                    ("client_id".into(), CLIENT.into()),
                ])
            );
        }
    }
}

#[test]
fn chatgpt_browser_denial_stores_no_tokens() {
    let f = Flow::new();
    f.server.state.lock().unwrap().denied = true;
    assert!(f.sign_in().unwrap_err().to_string().starts_with("SIGN_IN_DENIED"));
    assert!(!f.tokens_path().exists());
    assert!(f.server.state.lock().unwrap().token_requests.is_empty());
}

#[test]
fn chatgpt_pre_cancelled_sign_in_returns_within_a_second() {
    let f = Flow::new();
    let start = Instant::now();
    let error = f.app().sign_in(false, &AtomicBool::new(true), |_| Ok(())).unwrap_err();
    assert!(error.to_string().starts_with("CANCELLED"));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!f.tokens_path().exists());
    assert!(f.server.state.lock().unwrap().token_requests.is_empty());
}
