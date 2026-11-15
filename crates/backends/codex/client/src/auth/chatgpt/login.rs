use super::*;

#[inline(never)]
pub(super) fn start_with_issuer<'a>(
    auth_file: PathBuf,
    issuer: &'a str,
) -> OpenAiAuthFuture<'a, Result<ChatGptLogin, ChatGptAuthError>> {
    Box::pin(async move {
        let listener = bind_callback().await?;
        let port = listener
            .local_addr()
            .map_err(|_| ChatGptAuthError::CallbackUnavailable)?
            .port();
        let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
        let state = random_urlsafe()?;
        let code_verifier = random_urlsafe()?;
        let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
        let authorization_url = authorize_url(issuer, &redirect_uri, &state, &code_challenge)?;

        Ok(ChatGptLogin {
            issuer: issuer.to_owned(),
            authorization_url,
            redirect_uri,
            state,
            code_verifier,
            auth_file,
            listener,
            client: auth_client()?,
        })
    })
}

#[inline(never)]
pub(super) fn complete(
    this: ChatGptLogin,
) -> OpenAiAuthFuture<'static, Result<ChatGptAuthStatus, ChatGptAuthError>> {
    Box::pin(async move {
        let callback = timeout(LOGIN_TIMEOUT, receive_callback(&this.listener))
            .await
            .map_err(|_| ChatGptAuthError::CallbackTimeout)??;
        let result = this.complete_callback(&callback.target).await;
        let reply = callback.reply(result.is_ok()).await;
        match (result, reply) {
            (Ok(status), Ok(())) => Ok(status),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    })
}
