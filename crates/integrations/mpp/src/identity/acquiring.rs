use super::*;
use crate::provider::Boxed;

#[inline(never)]
pub(super) fn acquire<'a>(
    owner: &'a Identity,
    key: &'a str,
    issuer: &'a str,
    audience: &'a str,
    rejected: Option<&'a str>,
) -> Boxed<'a, Result<Option<String>, String>> {
    Box::pin(async move {
        if let Some(entry) = owner.recall(key, rejected).await {
            let token = entry.token.clone();
            owner
                .held
                .update(|held| held.insert(key.to_owned(), entry.clone()));
            return Ok(Some(token));
        }
        let mut lent = Lent {
            inbox: owner.inbox.take(),
            slot: &owner.inbox,
        };
        let inbox = lent
            .inbox
            .as_mut()
            .ok_or("another email pass is being asked for")?;
        while inbox.try_recv().is_ok() {}
        let email = match (owner.email.read(Clone::clone), owner.asker) {
            (_, Asker::Nobody) | (None, Asker::Stdin) => return Err(UNASKED.to_owned()),
            (Some(email), Asker::Pass | Asker::Stdin) => email,
            (None, Asker::Pass) => {
                owner.notice(format!(
                    "{audience} asks for an email pass from {issuer}: /pass <email> to get a code, or /pass alone to decline"
                ));
                loop {
                    match inbox.recv().await {
                        Some(Answer::Email(email)) => break email,
                        Some(Answer::Code(_)) => owner.notice(
                            "an email pass needs an email address first: /pass <email>".to_owned(),
                        ),
                        Some(Answer::Decline) | None => return Ok(None),
                    }
                }
            }
        };
        let client = http_client(&owner.client)?;
        let id = post(
            &client,
            issuer,
            OTP_PATH,
            serde_json::json!({ "email": email, "audience": audience }),
        )
        .await?;
        let id = id["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("the issuer returned a malformed code request")?
            .to_owned();
        let mut failure = String::new();
        for attempt in 1..=CODE_ATTEMPTS {
            owner.notice(match owner.asker {
                Asker::Stdin => format!(
                    "{issuer} sent a code to {email} for {audience}: type it and press Enter (attempt {attempt} of {CODE_ATTEMPTS}), or an empty line to decline{failure}"
                ),
                Asker::Pass | Asker::Nobody => format!(
                    "{issuer} sent a code to {email} for {audience}: /pass <code> (attempt {attempt} of {CODE_ATTEMPTS}), or /pass alone to decline{failure}"
                ),
            });
            let code = loop {
                match inbox.recv().await {
                    Some(Answer::Code(code)) => break code,
                    Some(Answer::Email(_)) => owner.notice(format!(
                        "the code went to {email}: /pass <code>; the new address is used for the next email pass"
                    )),
                    Some(Answer::Decline) | None => return Ok(None),
                }
            };
            let verified = post(
                &client,
                issuer,
                VERIFY_PATH,
                serde_json::json!({ "id": id, "code": code }),
            )
            .await;
            let token = match verified {
                Ok(body) => body["attestation"]
                    .as_str()
                    .filter(|token| !token.is_empty())
                    .ok_or("the issuer returned a malformed attestation")?
                    .to_owned(),
                Err(error) if error.starts_with("400 ") => {
                    failure = format!("; {}", &error[4..]);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let entry = Held::decode(&token, Some(now_seconds()))?.owned_by(&email);
            if entry.claims.key() != key {
                return Err("the issuer returned an attestation for another audience".to_owned());
            }
            owner
                .held
                .update(|held| held.insert(key.to_owned(), entry.clone()));
            match owner.remember(key, &entry, &email).await {
                Ok(()) => owner.notice(format!("email pass for {audience} verified")),
                Err(error) => owner.notice(format!(
                    "email pass for {audience} verified, kept for this session only: {error}"
                )),
            }
            return Ok(Some(token));
        }
        Err(format!(
            "the code was rejected {CODE_ATTEMPTS} times{failure}"
        ))
    })
}

#[inline(never)]
pub(super) fn retry<'a>(owner: &'a Identity, rejected: Rejected) -> Boxed<'a, Option<HeaderValue>> {
    Box::pin(async move {
        let Rejected {
            audience,
            groups,
            sent,
        } = rejected;
        let mut obtained: Vec<(String, String)> = Vec::new();
        let mut failure = None;
        for group in groups {
            let alternatives = owner.trusted(&group, &audience);
            if alternatives
                .iter()
                .any(|(key, _)| obtained.iter().any(|(got, _)| got == key))
            {
                continue;
            }
            let keys: Vec<String> = alternatives.iter().map(|(key, _)| key.clone()).collect();
            let mut met = false;
            for (key, issuer) in alternatives {
                match owner.obtain(&key, &issuer, &audience, &sent).await {
                    Ok(Some(token)) => {
                        obtained.push((key, token));
                        met = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
            if !met {
                let now = now_seconds();
                if let Some(found) = keys.into_iter().find_map(|key| {
                    let entry = owner.held(&key)?;
                    (entry.claims.expires > now + CLOCK_TOLERANCE && !sent.contains(&entry.token))
                        .then_some((key, entry.token))
                }) {
                    obtained.push(found);
                }
            }
        }
        if obtained.is_empty() {
            if let Some(error) = failure {
                owner.notice(format!("email pass for {audience}: {error}"));
            }
            return None;
        }
        let mut value: Vec<String> = sent
            .into_iter()
            .filter(|token| {
                claims(token)
                    .is_none_or(|claims| !obtained.iter().any(|(key, _)| *key == claims.key()))
            })
            .collect();
        value.extend(obtained.into_iter().map(|(_, token)| token));
        HeaderValue::from_str(&value.join(", ")).ok()
    })
}

#[inline(never)]
pub(super) fn obtain<'a>(
    owner: &'a Identity,
    key: &'a str,
    issuer: &'a str,
    audience: &'a str,
    sent: &'a [String],
) -> Boxed<'a, Result<Option<String>, String>> {
    Box::pin(async move {
        let now = now_seconds();
        let rejected = match owner.held(key) {
            Some(entry)
                if entry.usable(now, true)
                    && !sent.contains(&entry.token)
                    && (!sent.is_empty() || entry.usable(now, false)) =>
            {
                return Ok(Some(entry.token));
            }
            Some(entry) if !entry.usable(now, true) || sent.contains(&entry.token) => {
                owner.forget(key, &entry.token).await;
                Some(entry.token)
            }
            _ => None,
        };
        loop {
            let (ran, outcome) = owner
                .flight
                .run(|| async {
                    let outcome = owner
                        .acquire(key, issuer, audience, rejected.as_deref())
                        .await;
                    (key.to_owned(), outcome)
                })
                .await;
            if ran == key {
                return outcome;
            }
            if let Some(entry) = owner
                .held(key)
                .filter(|entry| Some(&entry.token) != rejected.as_ref())
            {
                return Ok(Some(entry.token));
            }
        }
    })
}

#[inline(never)]
pub(super) fn forget_held<'a>(owner: &'a Identity, key: &'a str, token: &'a str) -> Boxed<'a, ()> {
    Box::pin(async move {
        let Some(directory) = owner.passes.clone() else {
            return;
        };
        let (key, token) = (key.to_owned(), token.to_owned());
        let forgotten =
            nanocode_platform::spawn_blocking(move || forget(&directory, &key, &token)).await;
        if let Ok(Err(error)) = forgotten {
            nanocode_trace::warn!(target: "nanocode", error = %error, "a refused email pass stays on disk");
        }
    })
}
