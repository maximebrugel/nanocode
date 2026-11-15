use super::*;
use crate::provider::Boxed;

#[inline(never)]
pub(super) fn pay_with_context<'a>(
    this: &'a Payer,
    challenge: &'a Challenge,
    context: PaymentContext,
) -> Boxed<'a, Result<Credential, Fault>> {
    Box::pin(async move {
        if challenge.intent == INTENT_SESSION {
            return this
                .session_provider_for(challenge, Some(context.url.as_str()))?
                .pay_with_context(challenge, context)
                .await;
        }
        this.pay_charge(challenge).await
    })
}

#[inline(never)]
pub(super) fn pay_with_management<'a>(
    this: &'a Payer,
    challenge: &'a Challenge,
    context: PaymentContext,
    management: &'a dyn ManagementTransport,
) -> Boxed<'a, Result<Credential, Fault>> {
    Box::pin(async move {
        if challenge.intent == INTENT_SESSION {
            return this
                .session_provider_for(challenge, Some(context.url.as_str()))?
                .pay_with_management(challenge, context, management)
                .await;
        }
        this.pay_charge(challenge).await
    })
}

#[inline(never)]
pub(super) fn commit_payment<'a>(
    this: &'a Payer,
    challenge: &'a Challenge,
    credential: &'a Credential,
) -> Boxed<'a, Result<(), Fault>> {
    Box::pin(async move {
        if challenge.intent == INTENT_SESSION {
            return this
                .session_provider_holding(challenge, credential)?
                .commit_payment(challenge, credential)
                .await;
        }
        this.forget_funding(challenge);
        this.take_authorization(challenge, credential)?;
        Ok(())
    })
}

#[inline(never)]
pub(super) fn rollback_payment<'a>(
    this: &'a Payer,
    challenge: &'a Challenge,
    credential: &'a Credential,
) -> Boxed<'a, Result<(), Fault>> {
    Box::pin(async move {
        if challenge.intent == INTENT_SESSION {
            return this
                .session_provider_holding(challenge, credential)?
                .rollback_payment(challenge, credential)
                .await;
        }
        this.forget_funding(challenge);
        if let Some(reservation) = this.take_authorization(challenge, credential)? {
            this.accounts.release_authorization(reservation);
        }
        Ok(())
    })
}

#[inline(never)]
pub(super) fn invalidate_payment<'a>(
    this: &'a Payer,
    challenge: &'a Challenge,
    credential: &'a Credential,
) -> Boxed<'a, Result<(), Fault>> {
    Box::pin(async move {
        if challenge.intent == INTENT_SESSION {
            return this
                .session_provider_holding(challenge, credential)?
                .invalidate_payment(challenge, credential)
                .await;
        }
        this.rollback_payment(challenge, credential).await
    })
}

#[inline(never)]
pub(super) fn prepaid_credential<'a>(
    this: &'a Payer,
    context: PaymentContext,
) -> Boxed<'a, Option<(Challenge, Credential)>> {
    Box::pin(async move {
        this.session.as_ref()?;
        let provider = this
            .configured_session_provider(Some(context.url.as_str()))
            .ok()?;
        let (challenge, credential) = provider.prepaid_credential(context).await?;
        if this.check_expected_chain(&challenge).is_err() {
            provider.abandon_payment(&challenge, &credential);
            return None;
        }
        Some((challenge, credential))
    })
}

#[inline(never)]
pub(super) fn preconnect<'a>(
    this: &'a Payer,
    client: &'a nanocode_http::Client,
    url: &'a str,
    body: String,
) -> Boxed<'a, Result<(), Fault>> {
    Box::pin(async move {
        let challenges = this.probe(client, url, body.clone(), None).await?;
        let candidates: Vec<&Challenge> = challenges.iter().collect();
        let Some(challenge) = this
            .select_challenge(&candidates)
            .filter(|challenge| challenge.intent == INTENT_SESSION)
        else {
            return Ok(());
        };
        let session = this.session_provider_for(challenge, Some(url))?;
        session.bind_origin(url)?;
        if !session.restore(challenge).await? {
            return Ok(());
        }
        let Some(channel_id) = session.restored_channel_id(challenge) else {
            return Ok(());
        };
        let described = this
            .probe(client, url, body, Some((&channel_id, &session)))
            .await?;
        for challenge in &described {
            if this.check_expected_chain(challenge).is_ok() && session.prime_prepaid(url, challenge)
            {
                break;
            }
        }
        Ok(())
    })
}
