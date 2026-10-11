//! The OAuth and OpenID Connect requests of OpenAI's sign-in for open-source apps, and the checks
//! on what comes back.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use ring::{digest, signature};
use serde::Deserialize;

/// The client id of a first sign-in, which registers Nuzky with the account. Never saved.
pub(crate) const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
/// The name ChatGPT suggests for Nuzky's registration; the user can change it before agreeing.
const AGENT_NAME: &str = "Nuzky";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const RESOURCE: &str = "https://api.openai.com/v1";
/// Without it, the sign-in tells who the user is but does not let Nuzky use their plan.
pub(crate) const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
/// Refresh errors after which the refresh token is no use; the user has to sign in again.
pub(crate) const UNUSABLE_REFRESH: [&str; 6] = [
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];
/// Allowed difference between this computer's clock and OpenAI's when checking expiry.
const CLOCK_SKEW: u64 = 60;
/// Token and key answers are a few kilobytes.
const MAX_BYTES: u64 = 256 * 1024;

/// The secrets of one sign-in attempt, fresh each time.
pub(crate) struct Attempt {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect_uri: String,
}

impl Attempt {
    pub fn new(redirect_uri: String) -> Self {
        Self { state: random(), nonce: random(), verifier: random(), redirect_uri }
    }
}

/// 32 random bytes as base64url: 43 characters, also a valid PKCE verifier.
fn random() -> String {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).expect("the system's random number generator works");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256: the base64url SHA-256 of the verifier, without padding.
pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

pub(crate) struct Authorize<'a> {
    /// The client issued to Nuzky for this account; None registers Nuzky with a new one.
    pub client_id: Option<&'a str>,
    pub host_id: &'a str,
    /// The account's last ID token, so ChatGPT skips choosing the account.
    pub id_token_hint: Option<&'a str>,
    pub login_hint: Option<&'a str>,
    /// Asks the user again to let Nuzky use their plan.
    pub consent: bool,
}

/// The authorization page. It can hold the last ID token, so it is never logged.
pub(crate) fn authorize_url(issuer: &str, request: &Authorize, attempt: &Attempt) -> String {
    let challenge = challenge(&attempt.verifier);
    let mut query = vec![("client_id", request.client_id.unwrap_or(DYNAMIC_CLIENT))];
    if request.client_id.is_none() {
        query.push(("agent_name_hint", AGENT_NAME));
    }
    query.push(("ext_agent_host_id", request.host_id));
    query.extend(request.id_token_hint.map(|hint| ("id_token_hint", hint)));
    query.extend(request.login_hint.map(|hint| ("login_hint", hint)));
    query.extend([
        ("response_type", "code"),
        ("redirect_uri", &attempt.redirect_uri),
        ("scope", SCOPE),
        ("resource", RESOURCE),
        ("state", &attempt.state),
        ("nonce", &attempt.nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ]);
    if request.consent {
        query.push(("prompt", "consent"));
    }
    let query: Vec<String> = query.iter().map(|(key, value)| format!("{key}={}", encode(value))).collect();
    format!("{issuer}/api/accounts/authorize?{}", query.join("&"))
}

/// Percent-encoding with spaces as %20, which every server reads as a space.
fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>().replace('+', "%20")
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Callback {
    Code { code: String, client_id: String },
    Denied,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CallbackError {
    /// Not the answer to this attempt.
    WrongState,
    Failed(String),
}

/// The browser's return to /auth/callback. `saved_client` is the client this attempt signed in
/// with; without one, ChatGPT names the client it issued.
pub(crate) fn parse_callback(query: &str, state: &str, saved_client: Option<&str>) -> Result<Callback, CallbackError> {
    let param = |name: &str| {
        url::form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == name).map(|(_, value)| value.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Err(CallbackError::WrongState);
    }
    if let Some(error) = param("error") {
        if error == "access_denied" {
            return Ok(Callback::Denied);
        }
        let why = param("error_description").unwrap_or(error);
        return Err(CallbackError::Failed(format!("ChatGPT refused the sign-in: {why}")));
    }
    let code = param("code").filter(|code| !code.is_empty());
    let code = code.ok_or_else(|| CallbackError::Failed("ChatGPT sent no authorization code".into()))?;
    let issued = param("client_id").filter(|id| !id.is_empty());
    let client_id = match saved_client {
        Some(saved) if issued.as_deref().is_none_or(|issued| issued == saved) => saved.to_owned(),
        Some(_) => return Err(CallbackError::Failed("ChatGPT answered for another registration of Nuzky".into())),
        None => issued
            .filter(|issued| issued != DYNAMIC_CLIENT)
            .ok_or_else(|| CallbackError::Failed("ChatGPT did not finish registering Nuzky".into()))?,
    };
    Ok(Callback::Code { code, client_id })
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_in: Option<u64>,
    /// The granted scopes, separated by spaces.
    pub scope: Option<String>,
}

pub(crate) enum TokenError {
    /// The token endpoint answered with an OAuth error code.
    Refused { code: String, description: Option<String> },
    /// The network, or an answer that is no OAuth error.
    Other(anyhow::Error),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Refused { code, description } => {
                write!(f, "{code}{}", description.as_ref().map(|d| format!(": {d}")).unwrap_or_default())
            }
            TokenError::Other(error) => write!(f, "{error:#}"),
        }
    }
}

pub(crate) fn exchange(
    http: &ureq::Agent,
    issuer: &str,
    client_id: &str,
    code: &str,
    attempt: &Attempt,
) -> Result<TokenResponse, TokenError> {
    token_request(
        http,
        issuer,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", &attempt.verifier),
            ("redirect_uri", &attempt.redirect_uri),
            ("resource", RESOURCE),
        ],
    )
}

/// Without `scope`, so the grant stays as it was.
pub(crate) fn refresh(
    http: &ureq::Agent,
    issuer: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, TokenError> {
    token_request(
        http,
        issuer,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("resource", RESOURCE),
        ],
    )
}

fn token_request(http: &ureq::Agent, issuer: &str, form: &[(&str, &str)]) -> Result<TokenResponse, TokenError> {
    let (status, body) =
        post_form(http, &format!("{issuer}/api/accounts/oauth/token"), form).map_err(TokenError::Other)?;
    if status == 200 {
        // serde's message names the line and column, never the text, which holds the tokens.
        return serde_json::from_str(&body).map_err(|error| TokenError::Other(anyhow!("Reading the tokens: {error}")));
    }
    match oauth_error(&body) {
        Some((code, description)) if (400..500).contains(&status) => Err(TokenError::Refused { code, description }),
        _ => Err(TokenError::Other(anyhow!("The token endpoint answered {status}"))),
    }
}

/// `{"error": "code"}` as OAuth has it, or `{"error": {"code": …}}` as OpenAI's API does.
fn oauth_error(body: &str) -> Option<(String, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = |v: &serde_json::Value| v.as_str().map(str::to_owned);
    match &value["error"] {
        serde_json::Value::String(code) => Some((code.clone(), text(&value["error_description"]))),
        error @ serde_json::Value::Object(_) => Some((text(&error["code"])?, text(&error["message"]))),
        _ => None,
    }
}

/// Ends the renewable session. True once OpenAI confirmed it; a failed connection or a server
/// error is tried three times.
pub(crate) fn revoke(http: &ureq::Agent, issuer: &str, client_id: &str, refresh_token: &str) -> bool {
    let form = [("token", refresh_token), ("token_type_hint", "refresh_token"), ("client_id", client_id)];
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500 * attempt));
        }
        match post_form(http, &format!("{issuer}/api/accounts/oauth/revoke"), &form) {
            Ok((200, _)) => return true,
            Ok((status, _)) if status < 500 => {
                log::warn!("Ending the ChatGPT session was refused ({status})");
                return false;
            }
            Ok((status, _)) => log::warn!("Ending the ChatGPT session failed ({status})"),
            Err(error) => log::warn!("Ending the ChatGPT session failed: {error:#}"),
        }
    }
    false
}

fn post_form(http: &ureq::Agent, url: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
    let mut response = http.post(url).header("Accept", "application/json").send_form(form.iter().copied())?;
    let status = response.status().as_u16();
    let body = response.body_mut().with_config().limit(MAX_BYTES).read_to_string()?;
    Ok((status, body))
}

#[derive(Deserialize)]
pub(crate) struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

/// OpenAI's signing keys, read fresh for every sign-in, so a new key is never missing.
pub(crate) fn jwks(http: &ureq::Agent, issuer: &str) -> Result<Jwks> {
    let mut response = http.get(&format!("{issuer}/.well-known/jwks.json")).call()?;
    ensure!(response.status() == 200, "the keys answered {}", response.status());
    let body = response.body_mut().with_config().limit(MAX_BYTES).read_to_string()?;
    serde_json::from_str(&body).context("Reading the keys")
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    azp: Option<String>,
    exp: u64,
    nonce: Option<String>,
    sub: String,
    email: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub subject: String,
    pub email: Option<String>,
}

/// Who signed in, once the ID token's RS256 signature matches one of OpenAI's keys and it was
/// issued by `issuer` for `client_id`, has not expired and carries this attempt's nonce.
pub(crate) fn verify_id_token(
    token: &str,
    jwks: &Jwks,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: u64,
) -> Result<Identity> {
    let [header, payload, sig] = token.split('.').collect::<Vec<_>>()[..] else { bail!("The ID token is not a JWT") };
    let header: Header = serde_json::from_slice(&decode(header)?).context("Reading the ID token's header")?;
    ensure!(header.alg == "RS256", "The ID token is signed with {}, not RS256", header.alg);
    let signed = &token[..token.len() - sig.len() - 1];
    let sig = decode(sig)?;
    let verified = jwks
        .keys
        .iter()
        .filter(|key| key.kty == "RSA" && key.alg.as_deref().is_none_or(|alg| alg == "RS256"))
        .filter(|key| key.usage.as_deref().is_none_or(|usage| usage == "sig"))
        .filter(|key| header.kid.is_none() || key.kid == header.kid)
        .any(|key| {
            let (Some(Ok(n)), Some(Ok(e))) = (key.n.as_deref().map(decode), key.e.as_deref().map(decode)) else {
                return false;
            };
            signature::RsaPublicKeyComponents { n, e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &sig)
                .is_ok()
        });
    ensure!(verified, "The ID token's signature matches none of OpenAI's keys");
    let claims: Claims = serde_json::from_slice(&decode(payload)?).context("Reading the ID token")?;
    ensure!(claims.iss == issuer, "The ID token comes from {}, not {issuer}", claims.iss);
    let audience = match &claims.aud {
        Audience::One(aud) => aud == client_id,
        Audience::Many(auds) => auds.iter().any(|aud| aud == client_id),
    };
    ensure!(audience && claims.azp.as_deref().is_none_or(|azp| azp == client_id), "The ID token is for another app");
    ensure!(claims.exp + CLOCK_SKEW > now, "The ID token has expired");
    ensure!(claims.nonce.as_deref() == Some(nonce), "The ID token is from another sign-in");
    ensure!(!claims.sub.is_empty(), "The ID token names no account");
    Ok(Identity { subject: claims.sub, email: claims.email })
}

fn decode(part: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(part).context("The ID token is not base64url")
}
