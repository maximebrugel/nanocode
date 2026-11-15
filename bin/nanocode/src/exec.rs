use super::*;

#[inline(never)]
pub(super) fn exec(
    args: Box<SessionArgs>,
    clients: ModelClients,
) -> nanocode_sync::future::BoxFuture<'static, Result<()>> {
    Box::pin(async move {
        let piped = match clients.answering() {
            Some(_) => None,
            None => piped_prompt().await?,
        };
        let prompt = [
            args.prompt
                .clone()
                .filter(|prompt| !prompt.trim().is_empty()),
            piped.clone().filter(|piped| !piped.trim().is_empty()),
        ]
        .into_iter()
        .flatten()
        .reduce(|prompt, piped| format!("{prompt}\n{piped}"))
        .ok_or("exec requires a prompt, as an argument or on stdin")?;
        if let Some(count) = args.attempts {
            let piped = piped.filter(|piped| !piped.trim().is_empty());
            return attempts::run(&args, count, piped).boxed().await;
        }
        let prepared = prepare(
            &args,
            clients,
            &mut |message| {
                eprintln!("nanocode: {message}");
            },
            true,
        )
        .boxed()
        .await?;
        let mut runner_events = prepared.runner_events;
        nanocode_platform::spawn_task(async move {
            while let Some(event) = runner_events.recv().await {
                if let Some(failure) = runner_failure(&event) {
                    eprintln!("nanocode: {failure}");
                }
            }
        });
        let host = prepared.host;
        let escalation = match (&args.escalate_model, args.escalate_after) {
            (Some(name), Some(after)) => Some((
                escalation_model(&host, name, prepared.route, &prepared.model).await?,
                after,
            )),
            _ => None,
        };
        let resume = match (&args.resume, args.continue_last) {
            (Some(id), _) if !id.is_empty() => Some(id.clone()),
            (_, true) => Some(latest_session(&host).boxed().await?),
            _ => None,
        };
        let (mut agent, events, pending) = match resume.clone() {
            Some(id) => {
                let ready = app::open_session(
                    &host,
                    &id,
                    args.thinking
                        .as_deref()
                        .map(host::parse_thinking)
                        .transpose()?,
                    args.tier.as_deref().map(host::parse_tier).transpose()?,
                )
                .boxed()
                .await?;
                let (asked, runs) = (
                    prepared.route.label(&prepared.model),
                    ready.route.label(&ready.model),
                );
                if args.model.is_some() && asked != runs {
                    drop(ready.agent.shutdown().await);
                    return Err(format!(
                        "session {id} runs {runs}, and a session keeps its model; resume it without --model {asked}, or start a new session"
                    )
                    .into());
                }
                (ready.agent, ready.events, ready.pending)
            }
            None => {
                let (agent, events) = host
                    .start(&prepared.model, prepared.thinking, prepared.route)
                    .boxed()
                    .await?;
                app::apply_tier(&agent, &prepared.model, prepared.tier).await?;
                (agent, events, Vec::new())
            }
        };
        if let Some(reason) = host.runtime.computer_use_unavailable() {
            eprintln!("nanocode: computer use unavailable, continuing without it: {reason}");
        }
        drop(events);
        if let Some(mut notices) = host.config.clients.notifying() {
            nanocode_platform::spawn_task(async move {
                while let Some(notice) = notices.next().await {
                    eprintln!("{notice}");
                }
            });
        }
        if let Some(answer) = host.config.clients.answering() {
            std::thread::spawn(move || {
                for line in std::io::stdin().lines().map_while(std::io::Result::ok) {
                    answer(&line);
                }
                answer("");
            });
        }
        let outcome: Result<()> = async {
            for pending in pending {
                eprintln!("nanocode: resuming interrupted turn {}", pending.id);
                let turn = agent
                    .prompt(nanocode::PromptRequest::new(pending.prompt).request_id(&pending.id))
                    .await?;
                stream_turn(
                    turn,
                    std::io::stderr(),
                    std::io::stderr().is_terminal(),
                    None,
                )
                .await?;
            }
            let terminal = std::io::stdout().is_terminal();
            let stalled = stream_turn(
                agent.prompt(prompt.as_str()).await?,
                std::io::stdout(),
                terminal,
                escalation.as_ref().map(|(_, after)| *after),
            )
            .await?;
            if let (true, Some((model, after))) = (stalled, &escalation) {
                let (next, _) = host
                    .runtime
                    .switch(&agent, model.clone(), Some(prepared.thinking))
                    .await?;
                app::apply_tier(&next, model, prepared.tier).await?;
                drop(agent.flush().await);
                drop(agent.shutdown().await);
                agent = next;
                eprintln!(
                    "nanocode: {after} tool calls in a row wrote no file; continuing on {}",
                    prepared.route.label(model)
                );
                stream_turn(
                    agent.prompt(ESCALATION_PROMPT).await?,
                    std::io::stdout(),
                    terminal,
                    None,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        let mut stderr = std::io::stderr();
        if let Some(status) = host.config.clients.describe() {
            drop(writeln!(stderr, "{status}"));
        }
        if let Some(panel) = host.config.clients.render_panel() {
            drop(writeln!(stderr, "{}", panel.title));
            for row in panel.aligned() {
                drop(writeln!(stderr, "  {}", row.join("  ").trim_end()));
            }
        }
        let flushed = agent.flush().await;
        let unanswered = outcome.is_err()
            && resume.is_none()
            && unanswered(&host, agent.session_id()).boxed().await;
        if unanswered {
            drop(writeln!(
                stderr,
                "session {} has only the prompt: the turn failed before any reply",
                agent.session_id()
            ));
        } else {
            drop(writeln!(stderr, "session {}", agent.session_id()));
        }
        let shutdown = host.runtime.shutdown().await;
        let archived = host::archive(&host.config).boxed().await;
        outcome?;
        flushed?;
        shutdown?;
        archived?;
        Ok(())
    })
}
