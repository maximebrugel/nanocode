use super::*;
use crate::provider::Boxed;

#[inline(never)]
pub(super) fn close<'a>(
    this: &'a SessionProvider,
    client: &'a nanocode_http::Client,
    url: &'a str,
) -> Boxed<'a, Result<Option<Receipt>, Fault>> {
    Box::pin(async move {
        this.bind_origin(url)?;
        let Some(challenge) = this.shared.last_challenge.read(Clone::clone) else {
            return Ok(None);
        };
        if !this
            .shared
            .channels
            .read(|channels| channels.values().any(|entry| entry.opened))
        {
            return Ok(None);
        }
        let _process = this.acquire_payment_lock(&challenge).await?;
        let (expected_key, expected_chain_id) = this.expected_channel_key(&challenge)?;
        let _store = this.channel_store.acquire(&expected_key).await?;
        let Some(entry) = this.channel(&expected_key).filter(|entry| entry.opened) else {
            return Ok(None);
        };
        if entry.chain_id != expected_chain_id {
            return Err(Fault::config("channel does not match active session"));
        }
        ensure_redirect_safe_credential_header(&challenge)?;
        let channel_id = entry.id();
        let credential = this.close_credential_for_challenge(&challenge, &channel_id)?;
        let mut response = this
            .post_credential(client, url, &challenge, &credential, "close request failed")
            .await?;
        if let Some(refreshed) = SessionProvider::refreshed_challenge(&response) {
            ensure_redirect_safe_credential_header(&refreshed)?;
            let credential = this.close_credential_for_challenge(&refreshed, &channel_id)?;
            response = this
                .post_credential(client, url, &refreshed, &credential, "close request failed")
                .await?;
        }
        let status = response.status();
        let receipt = response_receipt(&response);
        let native_gone = status == nanocode_http::StatusCode::GONE
            && is_precompile_escrow(entry.escrow_contract);
        if !status.is_success() && !native_gone {
            let body = this.management_body(response).await;
            return Err(Fault::http(format!(
                "close request returned {status}: {body}"
            )));
        }
        if receipt.is_some() || native_gone {
            this.forget_closed_channel(&entry, &expected_key).await?;
        }
        Ok(receipt)
    })
}
