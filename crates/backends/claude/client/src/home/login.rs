use super::*;

#[inline(never)]
pub(super) fn start() -> ClaudeAuthFuture<'static, Result<ClaudeHomeLogin, ClaudeSubscriptionError>>
{
    Box::pin(async move {
        let subscription = ClaudeSubscription::home()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| ClaudeSubscriptionError::Host)?;
        let port = listener
            .local_addr()
            .map_err(|_| ClaudeSubscriptionError::Host)?
            .port();
        let origin = format!("http://localhost:{port}");
        let login = subscription
            .begin_login(ClaudeLoginMode::Callback {
                redirect_uri: format!("{origin}{CALLBACK_PATH}"),
            })
            .await?;
        Ok(ClaudeHomeLogin {
            subscription,
            listener,
            origin,
            authorization_url: login.authorization_url,
            expires_at: login.expires_at,
        })
    })
}

#[inline(never)]
pub(super) fn complete(
    this: ClaudeHomeLogin,
) -> ClaudeAuthFuture<'static, Result<ClaudeSubscriptionStatus, ClaudeSubscriptionError>> {
    Box::pin(async move {
        let remaining = Duration::from_millis(this.expires_at.saturating_sub(now()));
        let (mut stream, target) = nanocode_platform::timeout(remaining, callback(&this.listener))
            .await
            .map_err(|_| ClaudeSubscriptionError::LoginRequired)?
            .map_err(|_| ClaudeSubscriptionError::Host)?;
        let result = this
            .subscription
            .complete_login(&format!("{}{target}", this.origin))
            .await;
        reply(
            &mut stream,
            200,
            if result.is_ok() { SIGNED_IN } else { FAILED },
        )
        .await;
        result
    })
}
