//! Sign in with ChatGPT, so Nuzky's AI can run on the user's ChatGPT plan without an API key or a
//! terminal (OpenAI's flow for open-source apps, https://developers.openai.com/siwc/token-sharing-open-source).
//! The system browser signs in, a listener on 127.0.0.1 takes the code back, and the tokens live
//! only in the system keychain; the app gets the status, never a token. See docs/AI-ARCHITECTURE.md,
//! "ChatGPT sign-in".

mod listener;
mod oauth;
mod store;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use listener::Listener;
use oauth::{Attempt, Authorize, Callback, CallbackError, TokenError};
pub use store::Vault;
use store::{Registration, Tokens};

pub const ISSUER: &str = "https://auth.openai.com";
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Tokens this close to expiring are renewed first; access tokens live an hour.
const RENEW_BEFORE: u64 = 5 * 60;

/// What the ChatGPT dialog shows. It never carries a token.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(rename = "ChatGptStatus"))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    /// `email`: the account Continue with ChatGPT signs in to again, when there was one.
    SignedOut { email: Option<String>, notice: Option<Notice> },
    /// `plan`: ChatGPT allowed Nuzky to use the plan, not only to know who signed in.
    SignedIn { email: Option<String>, plan: bool },
}

#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(rename = "ChatGptNotice"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Notice {
    /// The sign-in could not be renewed: it expired, or Nuzky was disconnected in ChatGPT's settings.
    Ended,
    /// Signed out on this computer, but ChatGPT did not confirm ending the session.
    NotRevoked,
}

pub struct ChatGpt {
    issuer: String,
    /// Holds `chatgpt.json` (this computer's host id and the account it signed in to) and the lock.
    dir: PathBuf,
    vault: Vault,
}

impl ChatGpt {
    /// OpenAI's sign-in, with the tokens in the system keychain.
    pub fn new(dir: PathBuf) -> Self {
        Self::with(ISSUER.into(), dir, Vault::Keychain)
    }

    /// Another issuer, such as a test server on 127.0.0.1, and another vault.
    pub fn with(issuer: String, dir: PathBuf, vault: Vault) -> Self {
        Self { issuer, dir, vault }
    }

    /// The account as stored, its tokens renewed first when they expire within five minutes.
    pub fn status(&self) -> Result<Status> {
        let _lock = store::lock(&self.dir)?;
        let Some(mut reg) = Registration::load(&self.dir) else { return Ok(signed_out(None, None)) };
        let Some(client_id) = reg.client_id.clone() else { return Ok(signed_out(None, None)) };
        let Some(tokens) = self.vault.load(&reg.host_id)?.filter(|t| t.client_id == client_id) else {
            return Ok(signed_out(reg.email, None));
        };
        if tokens.expires_at > now() + RENEW_BEFORE {
            return Ok(Status::SignedIn { email: reg.email, plan: tokens.plan() });
        }
        match oauth::refresh(&self.http(), &self.issuer, &client_id, &tokens.refresh_token) {
            Ok(fresh) => {
                let tokens = tokens.renewed(fresh, now());
                self.vault.save(&reg.host_id, &tokens)?;
                Ok(Status::SignedIn { email: reg.email, plan: tokens.plan() })
            }
            Err(TokenError::Refused { code, .. }) if oauth::UNUSABLE_REFRESH.contains(&code.as_str()) => {
                self.vault.delete(&reg.host_id)?;
                Ok(signed_out(reg.email, Some(Notice::Ended)))
            }
            // The registration itself is gone, so the next sign-in registers Nuzky again.
            Err(TokenError::Refused { code, .. }) if code == "invalid_client" => {
                self.vault.delete(&reg.host_id)?;
                reg.client_id = None;
                reg.subject = None;
                reg.save(&self.dir)?;
                Ok(signed_out(None, Some(Notice::Ended)))
            }
            // Offline or a server hiccup: the sign-in stays, and the next look renews it.
            Err(error) => {
                log::warn!("Renewing the ChatGPT sign-in failed: {error}");
                Ok(Status::SignedIn { email: reg.email, plan: tokens.plan() })
            }
        }
    }

    /// Signs in through the system browser, which `open` shows the authorization page in, and waits
    /// up to five minutes for it to come back. `another_account` registers anew instead of signing
    /// in to the account signed in to before.
    pub fn sign_in(
        &self,
        another_account: bool,
        cancel: &AtomicBool,
        open: impl FnOnce(&str) -> Result<()>,
    ) -> Result<Status> {
        self.vault.check()?;
        let (reg, hint_tokens) = {
            let _lock = store::lock(&self.dir)?;
            let reg = Registration::load_or_create(&self.dir)?;
            let tokens = match &reg.client_id {
                Some(client_id) if !another_account => {
                    self.vault.load(&reg.host_id)?.filter(|t| &t.client_id == client_id)
                }
                _ => None,
            };
            (reg, tokens)
        };
        let saved_client = reg.client_id.as_deref().filter(|_| !another_account);
        let listener = Listener::bind()?;
        let attempt = Attempt::new(listener.redirect_uri()?);
        let request = Authorize {
            client_id: saved_client,
            host_id: &reg.host_id,
            id_token_hint: hint_tokens.as_ref().map(|t| t.id_token.as_str()),
            login_hint: saved_client.and(reg.email.as_deref()),
            // Asked again only when the user chose to allow the plan after an earlier sign-in did not.
            consent: hint_tokens.as_ref().is_some_and(|t| !t.plan()),
        };
        // The launcher's error quotes the address, which can hold the last ID token, so it goes nowhere.
        if open(&oauth::authorize_url(&self.issuer, &request, &attempt)).is_err() {
            bail!("SIGN_IN_FAILED: Could not open the browser");
        }

        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            let request = listener.next(deadline, cancel)?;
            match oauth::parse_callback(&request.query, &attempt.state, saved_client) {
                // Not this attempt's answer, such as a tab left from an earlier one: keep waiting.
                Err(CallbackError::WrongState) => request.reply(400, listener::PAGE_STALE),
                Err(CallbackError::Failed(why)) => {
                    request.reply(400, listener::PAGE_FAILED);
                    bail!("SIGN_IN_FAILED: {why}");
                }
                Ok(Callback::Denied) => {
                    request.reply(200, listener::PAGE_DENIED);
                    bail!("SIGN_IN_DENIED: Signing in to ChatGPT was cancelled in the browser.");
                }
                Ok(Callback::Code { code, client_id }) => {
                    let result = self.finish(&reg, another_account, &attempt, &client_id, &code, cancel);
                    request.reply(200, if result.is_ok() { listener::PAGE_DONE } else { listener::PAGE_FAILED });
                    return result;
                }
            }
        }
    }

    /// Exchanges the code, checks who signed in and keeps the tokens, replacing those of the
    /// account signed in to before only once this one is valid.
    fn finish(
        &self,
        saved: &Registration,
        another_account: bool,
        attempt: &Attempt,
        client_id: &str,
        code: &str,
        cancel: &AtomicBool,
    ) -> Result<Status> {
        let http = self.http();
        if saved.client_id.is_none() {
            // A first registration keeps its client before the exchange, so a retry after a failed
            // exchange signs in to it instead of registering Nuzky again.
            let _lock = store::lock(&self.dir)?;
            let mut reg = Registration::load_or_create(&self.dir)?;
            if reg.client_id.is_none() {
                reg.client_id = Some(client_id.into());
                reg.save(&self.dir)?;
            }
        }
        let response = oauth::exchange(&http, &self.issuer, client_id, code, attempt).map_err(|error| match error {
            TokenError::Refused { code, description } => {
                anyhow!(
                    "SIGN_IN_FAILED: ChatGPT did not accept the sign-in ({}). Try again.",
                    description.unwrap_or(code)
                )
            }
            TokenError::Other(error) => anyhow!("SIGN_IN_FAILED: Could not reach ChatGPT: {error}"),
        })?;
        let id_token = response.id_token.clone().context("SIGN_IN_FAILED: ChatGPT sent no ID token")?;
        let jwks = oauth::jwks(&http, &self.issuer).context("SIGN_IN_FAILED: Could not read OpenAI's signing keys")?;
        let identity = oauth::verify_id_token(&id_token, &jwks, &self.issuer, client_id, &attempt.nonce, now())
            .context("SIGN_IN_FAILED: ChatGPT's answer could not be verified")?;
        let returning = !another_account && saved.client_id.as_deref() == Some(client_id);
        if returning && saved.subject.as_ref().is_some_and(|subject| *subject != identity.subject) {
            bail!(
                "WRONG_ACCOUNT: Sign in as {} again, or choose Use another account.",
                saved.email.as_deref().unwrap_or("the account you used before")
            );
        }
        let tokens = Tokens::issued(client_id, id_token, response, now())
            .context("SIGN_IN_FAILED: ChatGPT did not let Nuzky stay signed in")?;

        let (status, replaced) = {
            let _lock = store::lock(&self.dir)?;
            // Cancel, or another sign-in started meanwhile, wins over this answer.
            if cancel.load(Ordering::Relaxed) {
                bail!("CANCELLED: Signing in was cancelled.");
            }
            let mut reg = Registration::load_or_create(&self.dir)?;
            let replaced = self.vault.load(&reg.host_id).ok().flatten().filter(|old| old.client_id != tokens.client_id);
            self.vault.save(&reg.host_id, &tokens)?;
            reg.client_id = Some(tokens.client_id.clone());
            reg.subject = Some(identity.subject);
            reg.email = identity.email;
            reg.save(&self.dir)?;
            (Status::SignedIn { email: reg.email, plan: tokens.plan() }, replaced)
        };
        // Another account's session ends on ChatGPT's side too, rather than staying open unused.
        if let Some(old) = replaced {
            oauth::revoke(&http, &self.issuer, &old.client_id, &old.refresh_token);
        }
        Ok(status)
    }

    /// Ends the session on ChatGPT's side and removes the tokens. The account stays known, so the
    /// next sign-in reuses Nuzky's registration with it.
    pub fn sign_out(&self) -> Result<Status> {
        let _lock = store::lock(&self.dir)?;
        let Some(reg) = Registration::load(&self.dir) else { return Ok(signed_out(None, None)) };
        let revoked = match self.vault.load(&reg.host_id) {
            Ok(Some(tokens)) => oauth::revoke(&self.http(), &self.issuer, &tokens.client_id, &tokens.refresh_token),
            Ok(None) => true,
            Err(error) => {
                log::warn!("Reading the ChatGPT sign-in to end it failed: {error:#}");
                false
            }
        };
        self.vault.delete(&reg.host_id)?;
        Ok(signed_out(reg.email, (!revoked).then_some(Notice::NotRevoked)))
    }

    fn http(&self) -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(15)))
            .user_agent(concat!("Nuzky/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    }
}

fn signed_out(email: Option<String>, notice: Option<Notice>) -> Status {
    Status::SignedOut { email, notice }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}
