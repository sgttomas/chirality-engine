//! The person's Codex account: read it, sign in with ChatGPT through Codex,
//! cancel a sign-in, sign out. The engine never sees a credential.

use serde_json::json;

use crate::api::*;
use crate::protocol as p;
use crate::session::Inner;

impl Inner {
    pub(crate) async fn sign_in(&self) -> Result<()> {
        let session = self.session()?;
        match session.call::<p::LoginResponse>("account/login/start", json!({ "type": "chatgpt" })).await? {
            p::LoginResponse::Chatgpt { login_id, auth_url } => {
                *self.login.lock().unwrap() = Some(login_id);
                self.store.set_account(Account::SigningIn { url: auth_url });
                Ok(())
            }
            p::LoginResponse::Other => Err(EngineError::Codex { message: "Codex did not start a ChatGPT sign-in".into() }),
        }
    }

    pub(crate) async fn cancel_sign_in(&self) -> Result<()> {
        let session = self.session()?;
        let Some(login_id) = self.login.lock().unwrap().take() else { return Ok(()) };
        session.call_value("account/login/cancel", json!({ "loginId": login_id })).await?;
        self.read_account().await;
        Ok(())
    }

    pub(crate) async fn sign_out(&self) -> Result<()> {
        let session = self.session()?;
        session.call_value("account/logout", json!({})).await?;
        self.store.set_account(Account::SignedOut);
        Ok(())
    }
}
