//! Platform adapters: realistic webhook payloads to board events, and the whole loop over a board.

use std::{collections::BTreeMap, sync::Arc};

use nanocode_board::{Board, EventId, NewTask, TaskId};
use nanocode_board_sync::{
    Accounts, Author, Channels, Engine, Event, Inbound, Join, Mode, Plain, Platform, Settle, SyncOp, Target,
    fake::{Echo, Fake, Fault},
    github, linear, slack,
};
use nanocode_sync::block_on;
use nanocode_turnlog::{LogHost, MemoryLog};
use serde_json::{Value, json};

const OPENED: u64 = 1_795_083_630_000;
const EDITED: u64 = 1_795_083_900_123;
const LATER: u64 = 1_795_084_200_000;
const OFFSET: u64 = 1_795_076_700_000;

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
}

fn one(mut parsed: Vec<Inbound>) -> Inbound {
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    parsed.remove(0)
}

fn user(login: &str, kind: &str) -> Value {
    json!({"login": login, "id": 4242, "type": kind, "site_admin": false})
}

fn repo() -> Value {
    json!({"id": 99, "name": "widgets", "full_name": "acme/widgets", "private": false})
}

fn pull(action: &str, extra: Value) -> Value {
    let mut payload = json!({
        "action": action,
        "number": 7,
        "pull_request": {
            "number": 7,
            "state": "open",
            "title": "Fix the rounding bug",
            "body": "Rounds half to even.",
            "draft": false,
            "merged": false,
            "user": user("ada", "User"),
            "head": {"ref": "fix", "sha": "abc123"},
            "base": {"ref": "main"},
            "labels": [{"name": "Bug Fix"}, {"name": "p1"}],
            "assignees": [user("grace", "User")],
            "created_at": "2026-11-19T10:20:30Z",
            "updated_at": "2026-11-19T10:20:30Z",
        },
        "repository": repo(),
        "sender": user("ada", "User"),
    });
    for (key, value) in extra.as_object().expect("an object") {
        payload["pull_request"][key] = value.clone();
    }
    payload
}

fn issue(action: &str) -> Value {
    json!({
        "action": action,
        "issue": {
            "number": 12,
            "state": "open",
            "title": "Crash on start",
            "body": "",
            "user": user("ada", "User"),
            "labels": [],
            "assignees": [],
            "updated_at": "2026-11-19T10:20:30Z",
        },
        "repository": repo(),
        "sender": user("ada", "User"),
    })
}

fn comment(action: &str, id: u64, body: &str, by: &Value) -> Value {
    json!({
        "action": action,
        "issue": {"number": 7, "pull_request": {"url": "https://api.github.com/repos/acme/widgets/pulls/7"}},
        "comment": {"id": id, "body": body, "user": by, "created_at": "2026-11-19T10:20:30Z", "updated_at": "2026-11-19T10:25:00.123Z"},
        "repository": repo(),
        "sender": by,
    })
}

fn review_comment(action: &str, id: u64, reply: Option<u64>) -> Value {
    let mut comment = json!({
        "id": id,
        "body": "nit: rename this",
        "path": "src/round.rs",
        "line": 42,
        "user": user("grace", "User"),
        "updated_at": "2026-11-19T10:25:00.123Z",
    });
    if let Some(top) = reply {
        comment["in_reply_to_id"] = json!(top);
    }
    json!({"action": action, "pull_request": {"number": 7}, "comment": comment, "repository": repo()})
}

fn review(action: &str, state: &str, body: Value) -> Value {
    json!({
        "action": action,
        "review": {"id": 555, "state": state, "body": body, "user": user("grace", "User"), "submitted_at": "2026-11-19T10:25:00Z"},
        "pull_request": {"number": 7, "updated_at": "2026-11-19T10:30:00Z"},
        "repository": repo(),
    })
}

fn gh_target() -> Target {
    Target::new("github", "acme/widgets#7")
}

fn pr_task() -> TaskId {
    TaskId("github/pr/acme/widgets/7".to_owned())
}

#[test]
fn github_pull_request_opened_becomes_a_task() {
    let inbound = one(github::parse("d-1", &pull("opened", json!({}))));
    assert_eq!(inbound.key, "d-1/opened");
    let want = NewTask {
        id: pr_task(),
        parent: None,
        spec: "Fix the rounding bug\n\nRounds half to even.".to_owned(),
        labels: labels(&[
            ("pr", "7"),
            ("state", "open"),
            ("repo", "acme/widgets"),
            ("head", "abc123"),
            ("tag.bug-fix", "Bug Fix"),
            ("tag.p1", "p1"),
            ("assignee.grace", "grace"),
        ]),
        needs: Vec::new(),
    };
    assert_eq!(inbound.event, Event::Opened { task: want, thread: gh_target(), message: "pr:7".to_owned(), mode: Mode::Always });
}

#[test]
fn github_draft_pull_request_is_a_draft_and_empty_body_is_just_the_title() {
    let inbound = one(github::parse("d-2", &pull("opened", json!({"draft": true, "body": null}))));
    let Event::Opened { task, .. } = inbound.event else { panic!("opened") };
    assert_eq!(task.spec, "Fix the rounding bug");
    assert_eq!(task.labels["state"], "draft");
}

#[test]
fn github_issue_opened_becomes_a_task() {
    let inbound = one(github::parse("d-3", &issue("opened")));
    let Event::Opened { task, thread, message, mode } = inbound.event else { panic!("opened") };
    assert_eq!(task.id, TaskId("github/issue/acme/widgets/12".to_owned()));
    assert_eq!(task.spec, "Crash on start");
    assert_eq!(task.labels, labels(&[("issue", "12"), ("state", "open"), ("repo", "acme/widgets")]));
    assert_eq!(thread, Target::new("github", "acme/widgets#12"));
    assert_eq!((message.as_str(), mode), ("issue:12", Mode::Always));
}

#[test]
fn github_redelivery_gives_the_same_keys_and_distinct_deliveries_do_not() {
    let payload = pull("closed", json!({"merged": true, "state": "closed"}));
    let first = github::parse("d-4", &payload);
    assert_eq!(first, github::parse("d-4", &payload));
    assert_eq!(first.len(), 2);
    let keys = first.iter().map(|inbound| inbound.key.as_str()).collect::<Vec<_>>();
    assert_eq!(keys, ["d-4/closed", "d-4/labels"]);
    let other = github::parse("d-5", &payload);
    assert!(other.iter().all(|inbound| first.iter().all(|seen| seen.key != inbound.key)));
}

#[test]
fn github_issue_comment_is_a_message() {
    let inbound = one(github::parse("d-6", &comment("created", 901, "looks good", &user("grace", "User"))));
    assert_eq!(inbound.key, "d-6/comment");
    let want = Event::Message {
        thread: gh_target(),
        message: "c:901".to_owned(),
        author: Author { user: "grace".to_owned(), name: "grace".to_owned(), bot: false },
        body: "looks good".to_owned(),
        context: None,
        sub: None,
        marker: None,
    };
    assert_eq!(inbound.event, want);
}

#[test]
fn github_issue_comment_on_a_plain_issue_uses_the_issue_thread() {
    let mut payload = comment("created", 902, "same here", &user("grace", "User"));
    payload["issue"] = json!({"number": 12});
    let Event::Message { thread, .. } = one(github::parse("d-7", &payload)).event else { panic!("message") };
    assert_eq!(thread, Target::new("github", "acme/widgets#12"));
}

#[test]
fn github_bots_are_detected_by_type_and_by_configured_login() {
    let app = comment("created", 903, "build passed", &user("ci-app[bot]", "Bot"));
    let Event::Message { author, .. } = one(github::parse("d-8", &app)).event else { panic!("message") };
    assert!(author.bot);
    let ours = comment("created", 904, "hello", &user("board-bot", "User"));
    let Event::Message { author, .. } = one(github::parse("d-9", &ours)).event else { panic!("message") };
    assert!(!author.bot);
    let config = github::Config { bot: Some("board-bot".to_owned()), ..github::Config::default() };
    let Event::Message { author, .. } = one(github::parse_with(&config, "d-9", &ours)).event else { panic!("message") };
    assert!(author.bot);
}

#[test]
fn github_hidden_marker_is_extracted_and_stripped() {
    let body = "On it.\n\n<!-- board:ev-77 -->";
    let inbound = one(github::parse("d-10", &comment("created", 905, body, &user("board-bot", "Bot"))));
    let Event::Message { body, marker, author, .. } = inbound.event else { panic!("message") };
    assert_eq!(body, "On it.");
    assert_eq!(marker, Some(EventId("ev-77".to_owned())));
    assert!(author.bot);
    let unclosed = one(github::parse("d-11", &comment("created", 906, "text <!-- board:ev-1", &user("grace", "User"))));
    let Event::Message { body, marker, .. } = unclosed.event else { panic!("message") };
    assert_eq!((body.as_str(), marker), ("text <!-- board:ev-1", None));
}

#[test]
fn github_comment_edit_carries_the_updated_at_version_and_delete_maps() {
    let edited = one(github::parse("d-12", &comment("edited", 901, "looks great", &user("grace", "User"))));
    assert_eq!(edited.event, Event::Edited { thread: gh_target(), message: "c:901".to_owned(), body: "looks great".to_owned(), version: EDITED });
    let deleted = one(github::parse("d-13", &comment("deleted", 901, "looks great", &user("grace", "User"))));
    assert_eq!(deleted.event, Event::Deleted { thread: gh_target(), message: "c:901".to_owned() });
}

#[test]
fn github_timestamps_honour_zone_offsets() {
    let mut payload = comment("edited", 901, "x", &user("grace", "User"));
    payload["comment"]["updated_at"] = json!("2026-11-19T10:25:00+02:00");
    let Event::Edited { version, .. } = one(github::parse("d-14", &payload)).event else { panic!("edited") };
    assert_eq!(version, OFFSET);
    payload["comment"]["updated_at"] = json!("not a time");
    assert!(github::parse("d-15", &payload).is_empty());
}

#[test]
fn github_review_comment_carries_path_line_and_thread_root() {
    let top = one(github::parse("d-16", &review_comment("created", 300, None)));
    let Event::Message { thread, message, context, sub, author, .. } = top.event else { panic!("message") };
    assert_eq!((thread, message.as_str()), (gh_target(), "rc:300"));
    assert_eq!((context.as_deref(), sub.as_deref()), (Some("src/round.rs:42"), Some("rc:300")));
    assert!(!author.bot);
    let reply = one(github::parse("d-17", &review_comment("created", 301, Some(300))));
    let Event::Message { message, sub, .. } = reply.event else { panic!("message") };
    assert_eq!((message.as_str(), sub.as_deref()), ("rc:301", Some("rc:300")));
    let edited = one(github::parse("d-18", &review_comment("edited", 301, Some(300))));
    assert_eq!(edited.event, Event::Edited { thread: gh_target(), message: "rc:301".to_owned(), body: "nit: rename this".to_owned(), version: EDITED });
    let deleted = one(github::parse("d-19", &review_comment("deleted", 301, Some(300))));
    assert_eq!(deleted.event, Event::Deleted { thread: gh_target(), message: "rc:301".to_owned() });
}

#[test]
fn github_review_submitted_is_a_message_and_a_review_label() {
    let parsed = github::parse("d-20", &review("submitted", "APPROVED", json!("ship it")));
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].key, "d-20/message");
    let Event::Message { thread, message, body, author, .. } = &parsed[0].event else { panic!("message") };
    assert_eq!((thread, message.as_str(), body.as_str(), author.user.as_str()), (&gh_target(), "r:555", "approved\n\nship it", "grace"));
    let want = Event::Labels { task: pr_task(), set: labels(&[("review", "approved")]), unset: Vec::new(), own: Vec::new(), version: EDITED - 123 };
    assert_eq!((parsed[1].key.as_str(), &parsed[1].event), ("d-20/labels", &want));
}

#[test]
fn github_review_states_requested_changes_comment_and_dismissed() {
    let parsed = github::parse("d-21", &review("submitted", "CHANGES_REQUESTED", Value::Null));
    let Event::Message { body, .. } = &parsed[0].event else { panic!("message") };
    assert_eq!(body, "requested changes");
    let Event::Labels { set, .. } = &parsed[1].event else { panic!("labels") };
    assert_eq!(set, &labels(&[("review", "changes_requested")]));
    assert!(github::parse("d-22", &review("submitted", "COMMENTED", Value::Null)).is_empty());
    let parsed = github::parse("d-23", &review("submitted", "COMMENTED", json!("one thought")));
    assert_eq!(parsed.len(), 1);
    let dismissed = one(github::parse("d-24", &review("dismissed", "DISMISSED", Value::Null)));
    let want = Event::Labels { task: pr_task(), set: BTreeMap::new(), unset: vec!["review".to_owned()], own: Vec::new(), version: LATER };
    assert_eq!(dismissed.event, want);
}

#[test]
fn github_label_and_assignee_changes_reconcile_the_owned_prefixes() {
    for action in ["labeled", "unlabeled", "assigned", "unassigned"] {
        let inbound = one(github::parse("d-25", &pull(action, json!({}))));
        assert_eq!(inbound.key, "d-25/labels");
        let want = Event::Labels {
            task: pr_task(),
            set: labels(&[("tag.bug-fix", "Bug Fix"), ("tag.p1", "p1"), ("assignee.grace", "grace")]),
            unset: Vec::new(),
            own: vec!["tag.".to_owned(), "assignee.".to_owned()],
            version: OPENED,
        };
        assert_eq!(inbound.event, want, "{action}");
    }
}

#[test]
fn github_lifecycle_actions_update_labels() {
    let sync = one(github::parse("d-26", &pull("synchronize", json!({"head": {"sha": "def456"}}))));
    let Event::Labels { set, .. } = sync.event else { panic!("labels") };
    assert_eq!(set["head"], "def456");
    let reopened = one(github::parse("d-27", &pull("reopened", json!({}))));
    let Event::Labels { set, .. } = reopened.event else { panic!("labels") };
    assert_eq!(set["state"], "open");
    let drafted = one(github::parse("d-28", &pull("converted_to_draft", json!({"draft": true}))));
    let Event::Labels { set, .. } = drafted.event else { panic!("labels") };
    assert_eq!(set["state"], "draft");
}

#[test]
fn github_close_completes_when_merged_and_cancels_otherwise() {
    let merged = github::parse("d-29", &pull("closed", json!({"merged": true})));
    assert_eq!(merged[0].event, Event::Closed { task: pr_task(), outcome: Settle::Done("merged".to_owned()) });
    let Event::Labels { set, .. } = &merged[1].event else { panic!("labels") };
    assert_eq!(set["state"], "merged");
    let closed = github::parse("d-30", &pull("closed", json!({})));
    assert_eq!(closed[0].event, Event::Closed { task: pr_task(), outcome: Settle::Cancelled("closed".to_owned()) });
    let Event::Labels { set, .. } = &closed[1].event else { panic!("labels") };
    assert_eq!(set["state"], "closed");
}

#[test]
fn github_close_behaviour_is_configurable() {
    let config = github::Config { close: github::Close::Done, ..github::Config::default() };
    let closed = github::parse_with(&config, "d-31", &pull("closed", json!({})));
    assert_eq!(closed[0].event, Event::Closed { task: pr_task(), outcome: Settle::Done("closed".to_owned()) });
    let closed = github::parse_with(&config, "d-32", &issue("closed"));
    let task = TaskId("github/issue/acme/widgets/12".to_owned());
    assert_eq!(closed[0].event, Event::Closed { task, outcome: Settle::Done("closed".to_owned()) });
}

#[test]
fn github_ignores_what_it_does_not_map() {
    assert!(github::parse("d-33", &json!({"zen": "Keep it logically awesome.", "hook_id": 1})).is_empty());
    assert!(github::parse("d-34", &pull("review_requested", json!({}))).is_empty());
    assert!(github::parse("d-35", &pull("edited", json!({}))).is_empty());
    assert!(github::parse("d-36", &json!({"action": "created", "repository": repo(), "installation": {"id": 1}})).is_empty());
}

fn event(inner: Value) -> Value {
    json!({
        "token": "x",
        "team_id": "T1",
        "api_app_id": "A1",
        "type": "event_callback",
        "event_id": "Ev01",
        "event_time": 1_795_083_900,
        "event": inner,
    })
}

fn top() -> Value {
    json!({"type": "message", "channel": "C1", "user": "U1", "text": "ship it?", "ts": "1795083900.000100", "event_ts": "1795083900.000100", "channel_type": "channel"})
}

fn human(id: &str, name: &str) -> Author {
    Author { user: id.to_owned(), name: name.to_owned(), bot: false }
}

#[test]
fn slack_top_level_message_roots_its_own_thread() {
    let inbound = one(slack::parse("Ev01", &event(top())));
    assert_eq!(inbound.key, "Ev01/message");
    let want = Event::Message {
        thread: Target::new("slack", "C1:1795083900.000100"),
        message: "1795083900.000100".to_owned(),
        author: human("U1", "U1"),
        body: "ship it?".to_owned(),
        context: None,
        sub: None,
        marker: None,
    };
    assert_eq!(inbound.event, want);
}

#[test]
fn slack_reply_uses_the_thread_ts_and_profile_name() {
    let mut reply = top();
    reply["ts"] = json!("1795083901.000200");
    reply["thread_ts"] = json!("1795083900.000100");
    reply["user_profile"] = json!({"display_name": "ada", "real_name": "Ada L"});
    let Event::Message { thread, message, author, .. } = one(slack::parse("Ev02", &event(reply))).event else { panic!("message") };
    assert_eq!((thread, message.as_str()), (Target::new("slack", "C1:1795083900.000100"), "1795083901.000200"));
    assert_eq!(author, human("U1", "ada"));
}

#[test]
fn slack_redelivery_repeats_the_key() {
    let payload = event(top());
    assert_eq!(slack::parse("Ev01", &payload), slack::parse("Ev01", &payload));
    assert_ne!(slack::parse("Ev01", &payload)[0].key, slack::parse("Ev02", &payload)[0].key);
}

#[test]
fn slack_bots_are_detected_by_bot_id_subtype_and_configured_user() {
    let mut app = top();
    app["bot_id"] = json!("B1");
    app.as_object_mut().expect("an object").remove("user");
    let Event::Message { author, .. } = one(slack::parse("Ev03", &event(app))).event else { panic!("message") };
    assert!(author.bot);
    assert_eq!(author.user, "B1");
    let mut posted = top();
    posted["subtype"] = json!("bot_message");
    posted["bot_id"] = json!("B2");
    posted["username"] = json!("deploys");
    let Event::Message { author, .. } = one(slack::parse("Ev04", &event(posted))).event else { panic!("message") };
    assert_eq!((author.bot, author.name.as_str()), (true, "deploys"));
    let Event::Message { author, .. } = one(slack::parse("Ev05", &event(top()))).event else { panic!("message") };
    assert!(!author.bot);
    let config = slack::Config { bot: Some("U1".to_owned()) };
    let Event::Message { author, .. } = one(slack::parse_with(&config, "Ev05", &event(top()))).event else { panic!("message") };
    assert!(author.bot);
}

#[test]
fn slack_marker_comes_from_metadata_or_a_hidden_suffix() {
    let mut tagged = top();
    tagged["bot_id"] = json!("B1");
    tagged["metadata"] = json!({"event_type": "board", "event_payload": {"board": "ev-9"}});
    let Event::Message { marker, body, .. } = one(slack::parse("Ev06", &event(tagged))).event else { panic!("message") };
    assert_eq!((marker, body.as_str()), (Some(EventId("ev-9".to_owned())), "ship it?"));
    let mut hidden = top();
    hidden["text"] = json!("ship it? &lt;!-- board:ev-10 --&gt;");
    let Event::Message { marker, body, .. } = one(slack::parse("Ev07", &event(hidden))).event else { panic!("message") };
    assert_eq!((marker, body.as_str()), (Some(EventId("ev-10".to_owned())), "ship it?"));
}

#[test]
fn slack_text_is_unescaped() {
    let mut escaped = top();
    escaped["text"] = json!("a &lt; b &amp;&amp; c &gt; d");
    let Event::Message { body, .. } = one(slack::parse("Ev08", &event(escaped))).event else { panic!("message") };
    assert_eq!(body, "a < b && c > d");
}

#[test]
fn slack_edit_carries_the_edit_timestamp_as_micros() {
    let changed = json!({
        "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "1795083950.000300", "hidden": true,
        "message": {"type": "message", "user": "U1", "text": "ship it now", "ts": "1795083900.000100", "thread_ts": "1795083800.000050", "edited": {"user": "U1", "ts": "1795083950.000300"}},
        "previous_message": {"type": "message", "user": "U1", "text": "ship it?", "ts": "1795083900.000100"},
    });
    let inbound = one(slack::parse("Ev09", &event(changed)));
    assert_eq!(inbound.key, "Ev09/edit");
    let want = Event::Edited {
        thread: Target::new("slack", "C1:1795083800.000050"),
        message: "1795083900.000100".to_owned(),
        body: "ship it now".to_owned(),
        version: 1_795_083_950_000_300,
    };
    assert_eq!(inbound.event, want);
}

#[test]
fn slack_unfurl_changes_without_an_edit_are_ignored() {
    let changed = json!({
        "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "1795083950.000300",
        "message": {"type": "message", "user": "U1", "text": "see link", "ts": "1795083900.000100"},
    });
    assert!(slack::parse("Ev10", &event(changed)).is_empty());
}

#[test]
fn slack_delete_maps_to_deleted_in_its_thread() {
    let gone = json!({
        "type": "message", "subtype": "message_deleted", "channel": "C1", "deleted_ts": "1795083901.000200", "ts": "1795083960.000400",
        "previous_message": {"type": "message", "user": "U1", "text": "x", "ts": "1795083901.000200", "thread_ts": "1795083900.000100"},
    });
    let inbound = one(slack::parse("Ev11", &event(gone)));
    assert_eq!(inbound.key, "Ev11/delete");
    assert_eq!(inbound.event, Event::Deleted { thread: Target::new("slack", "C1:1795083900.000100"), message: "1795083901.000200".to_owned() });
    let bare = json!({"type": "message", "subtype": "message_deleted", "channel": "C1", "deleted_ts": "1795083900.000100"});
    let Event::Deleted { thread, .. } = one(slack::parse("Ev12", &event(bare))).event else { panic!("deleted") };
    assert_eq!(thread, Target::new("slack", "C1:1795083900.000100"));
}

#[test]
fn slack_reaction_maps_to_reaction_on_the_message() {
    let added = json!({"type": "reaction_added", "user": "U2", "reaction": "white_check_mark", "item": {"type": "message", "channel": "C1", "ts": "1795083900.000100"}, "event_ts": "1795083970.000500"});
    let inbound = one(slack::parse("Ev13", &event(added)));
    assert_eq!(inbound.key, "Ev13/reaction");
    let want = Event::Reaction {
        thread: Target::new("slack", "C1:1795083900.000100"),
        message: "1795083900.000100".to_owned(),
        author: human("U2", "U2"),
        emoji: "white_check_mark".to_owned(),
    };
    assert_eq!(inbound.event, want);
    let file = json!({"type": "reaction_added", "user": "U2", "reaction": "eyes", "item": {"type": "file", "file": "F1"}});
    assert!(slack::parse("Ev14", &event(file)).is_empty());
}

fn action(id: &str, values: Value) -> Value {
    json!({
        "type": "block_actions",
        "user": {"id": "U3", "username": "grace", "name": "grace", "team_id": "T1"},
        "container": {"type": "message", "message_ts": "1795083980.000600", "channel_id": "C1", "thread_ts": "1795083900.000100"},
        "channel": {"id": "C1", "name": "eng"},
        "message": {"ts": "1795083980.000600", "thread_ts": "1795083900.000100"},
        "state": {"values": values},
        "actions": [{"type": "button", "action_id": id, "block_id": "b1", "text": {"type": "plain_text", "text": "Go"}, "action_ts": "1795083990.000700"}],
    })
}

#[test]
fn slack_button_click_maps_to_clicked_on_the_container_message() {
    let inbound = one(slack::parse("click-1", &action("approve", json!({}))));
    assert_eq!(inbound.key, "click-1/click");
    let want = Event::Clicked {
        thread: Target::new("slack", "C1:1795083900.000100"),
        message: "1795083980.000600".to_owned(),
        author: human("U3", "grace"),
        control: "approve".to_owned(),
        values: BTreeMap::new(),
    };
    assert_eq!(inbound.event, want);
}

#[test]
fn slack_form_submission_collects_the_forms_inputs_only() {
    let values = json!({
        "ask/reason": {"reason": {"type": "plain_text_input", "value": "flaky"}},
        "ask/level": {"level": {"type": "static_select", "selected_option": {"value": "high"}}},
        "other/note": {"note": {"type": "plain_text_input", "value": "ignored"}},
    });
    let Event::Clicked { values, control, .. } = one(slack::parse("click-2", &action("ask", values))).event else { panic!("clicked") };
    assert_eq!(control, "ask");
    assert_eq!(values, labels(&[("reason", "flaky"), ("level", "high")]));
}

#[test]
fn slack_message_clicks_without_a_thread_use_their_own_ts() {
    let mut payload = action("approve", json!({}));
    payload["container"].as_object_mut().expect("an object").remove("thread_ts");
    payload["message"].as_object_mut().expect("an object").remove("thread_ts");
    let Event::Clicked { thread, .. } = one(slack::parse("click-3", &payload)).event else { panic!("clicked") };
    assert_eq!(thread, Target::new("slack", "C1:1795083980.000600"));
}

#[test]
fn slack_ignores_handshakes_and_unrelated_events() {
    assert!(slack::parse("h-1", &json!({"type": "url_verification", "challenge": "abc", "token": "x"})).is_empty());
    assert!(slack::parse("h-2", &event(json!({"type": "channel_created", "channel": {"id": "C9"}}))).is_empty());
    assert!(slack::parse("h-3", &event(json!({"type": "app_mention", "user": "U1", "channel": "C1", "text": "hi", "ts": "1.1"}))).is_empty());
    let mut joined = top();
    joined["subtype"] = json!("channel_join");
    assert!(slack::parse("h-4", &event(joined)).is_empty());
    let mut empty = top();
    empty["text"] = json!("");
    assert!(slack::parse("h-5", &event(empty)).is_empty());
    assert!(slack::parse("h-6", &json!({"type": "view_submission"})).is_empty());
    assert!(slack::parse("h-7", &json!({})).is_empty());
}

#[test]
fn slack_blocks_render_buttons_and_forms() {
    let controls = [
        nanocode_board_sync::Control::Button { id: "approve".to_owned(), label: "Approve".to_owned() },
        nanocode_board_sync::Control::Form { id: "ask".to_owned(), label: "Send".to_owned(), fields: vec!["reason".to_owned()] },
    ];
    let blocks = slack::blocks("hello", &controls);
    let kinds = blocks.as_array().expect("an array").iter().map(|block| block["type"].as_str().expect("a type")).collect::<Vec<_>>();
    assert_eq!(kinds, ["section", "actions", "input", "actions"]);
    assert_eq!(blocks[2]["block_id"], "ask/reason");
    assert_eq!(slack::blocks("plain", &[]).as_array().expect("an array").len(), 1);
}

fn lin_issue(action: &str, data: Value) -> Value {
    let mut base = json!({
        "id": "uuid-1",
        "identifier": "ENG-12",
        "title": "Crash on start",
        "description": "Stack trace attached.",
        "state": {"name": "Todo", "type": "unstarted"},
        "assignee": {"id": "u-1", "name": "Ada Lovelace"},
        "labels": [{"name": "Bug"}, {"name": "Needs Triage"}],
        "createdAt": "2026-11-19T10:20:30.000Z",
        "updatedAt": "2026-11-19T10:20:30.000Z",
    });
    for (key, value) in data.as_object().expect("an object") {
        base[key] = value.clone();
    }
    json!({"action": action, "type": "Issue", "data": base, "url": "https://linear.app/acme/issue/ENG-12/crash-on-start", "webhookId": "wh-1", "createdAt": "2026-11-19T10:20:30.000Z"})
}

fn lin_comment(action: &str, body: &str) -> Value {
    json!({
        "action": action,
        "type": "Comment",
        "data": {
            "id": "c-1",
            "body": body,
            "issueId": "uuid-1",
            "issue": {"id": "uuid-1", "identifier": "ENG-12", "title": "Crash on start"},
            "user": {"id": "u-2", "name": "Grace Hopper"},
            "updatedAt": "2026-11-19T10:25:00.123Z",
        },
        "url": "https://linear.app/acme/issue/ENG-12/crash-on-start#comment-c-1",
    })
}

fn lin_thread() -> Target {
    Target::new("linear", "ENG-12")
}

#[test]
fn linear_issue_create_becomes_a_task() {
    let inbound = one(linear::parse("wh-d1", &lin_issue("create", json!({}))));
    assert_eq!(inbound.key, "wh-d1/opened");
    let want = NewTask {
        id: TaskId("linear/ENG-12".to_owned()),
        parent: None,
        spec: "Crash on start\n\nStack trace attached.".to_owned(),
        labels: labels(&[("issue", "ENG-12"), ("status", "Todo"), ("tag.bug", "Bug"), ("tag.needs-triage", "Needs Triage"), ("assignee.ada-lovelace", "Ada Lovelace")]),
        needs: Vec::new(),
    };
    assert_eq!(inbound.event, Event::Opened { task: want, thread: lin_thread(), message: "issue:ENG-12".to_owned(), mode: Mode::Always });
}

#[test]
fn linear_issue_update_reconciles_status_assignee_and_tags_with_a_version() {
    let payload = lin_issue("update", json!({"state": {"name": "In Progress"}, "updatedAt": "2026-11-19T10:25:00.123Z", "assignee": null, "labels": [{"name": "Bug"}]}));
    let inbound = one(linear::parse("wh-d2", &payload));
    assert_eq!(inbound.key, "wh-d2/labels");
    let want = Event::Labels {
        task: TaskId("linear/ENG-12".to_owned()),
        set: labels(&[("status", "In Progress"), ("tag.bug", "Bug")]),
        unset: Vec::new(),
        own: vec!["tag.".to_owned(), "assignee.".to_owned()],
        version: EDITED,
    };
    assert_eq!(inbound.event, want);
}

#[test]
fn linear_partial_update_leaves_unsent_fields_alone() {
    let mut payload = lin_issue("update", json!({"state": {"name": "Done"}}));
    for field in ["labels", "assignee"] {
        payload["data"].as_object_mut().expect("an object").remove(field);
    }
    let Event::Labels { set, own, .. } = one(linear::parse("wh-d3", &payload)).event else { panic!("labels") };
    assert_eq!(set, labels(&[("status", "Done")]));
    assert!(own.is_empty());
}

#[test]
fn linear_comment_create_update_remove() {
    let created = one(linear::parse("wh-d4", &lin_comment("create", "looks wrong")));
    assert_eq!(created.key, "wh-d4/comment");
    let want = Event::Message {
        thread: lin_thread(),
        message: "c:c-1".to_owned(),
        author: Author { user: "u-2".to_owned(), name: "Grace Hopper".to_owned(), bot: false },
        body: "looks wrong".to_owned(),
        context: None,
        sub: None,
        marker: None,
    };
    assert_eq!(created.event, want);
    let updated = one(linear::parse("wh-d5", &lin_comment("update", "looks right")));
    assert_eq!(updated.event, Event::Edited { thread: lin_thread(), message: "c:c-1".to_owned(), body: "looks right".to_owned(), version: EDITED });
    let removed = one(linear::parse("wh-d6", &lin_comment("remove", "looks right")));
    assert_eq!(removed.event, Event::Deleted { thread: lin_thread(), message: "c:c-1".to_owned() });
}

#[test]
fn linear_comment_finds_its_issue_from_the_url_when_the_identifier_is_missing() {
    let mut payload = lin_comment("create", "hi");
    payload["data"]["issue"] = json!({"id": "uuid-1"});
    let Event::Message { thread, .. } = one(linear::parse("wh-d7", &payload)).event else { panic!("message") };
    assert_eq!(thread, lin_thread());
}

#[test]
fn linear_bots_are_detected_by_bot_actor_and_configured_user() {
    let mut payload = lin_comment("create", "automated\n<!-- board:ev-5 -->");
    payload["data"].as_object_mut().expect("an object").remove("user");
    payload["data"]["botActor"] = json!({"id": "app-1", "name": "Board"});
    let Event::Message { author, marker, body, .. } = one(linear::parse("wh-d8", &payload)).event else { panic!("message") };
    assert!(author.bot);
    assert_eq!((body.as_str(), marker), ("automated", Some(EventId("ev-5".to_owned()))));
    let config = linear::Config { bot: Some("u-2".to_owned()) };
    let Event::Message { author, .. } = one(linear::parse_with(&config, "wh-d9", &lin_comment("create", "x"))).event else { panic!("message") };
    assert!(author.bot);
}

#[test]
fn linear_redelivery_repeats_keys_and_unrelated_payloads_are_ignored() {
    let payload = lin_issue("create", json!({}));
    assert_eq!(linear::parse("wh-d10", &payload), linear::parse("wh-d10", &payload));
    assert!(linear::parse("wh-d11", &lin_issue("remove", json!({}))).is_empty());
    assert!(linear::parse("wh-d12", &json!({"action": "create", "type": "Project", "data": {"id": "p"}})).is_empty());
    assert!(linear::parse("wh-d13", &json!({"type": "Issue"})).is_empty());
}

struct Loop {
    log: Arc<dyn LogHost>,
    board: Board,
    engine: Engine<Plain, Channels, Accounts>,
    slack: Arc<Fake>,
    github: Arc<Fake>,
    clock: u64,
}

impl Loop {
    fn new(slack: Fake) -> Self {
        let log: Arc<dyn LogHost> = Arc::new(MemoryLog::new());
        let board = block_on(Board::open(Arc::clone(&log), "adapters", "w1")).expect("board opens");
        let slack = Arc::new(slack);
        let github = Arc::new(Fake::new("github"));
        let platforms = BTreeMap::from([
            ("slack".to_owned(), Arc::clone(&slack) as Arc<dyn Platform>),
            ("github".to_owned(), Arc::clone(&github) as Arc<dyn Platform>),
        ]);
        let router = Channels { mirrored: BTreeMap::new(), bot: BTreeMap::new(), start: false };
        let engine = Engine::new(Plain, router, Accounts::default(), platforms);
        Self { log, board, engine, slack, github, clock: 1_000 }
    }

    fn webhook(&mut self, inbound: Vec<Inbound>) -> (u64, u64) {
        let mut totals = (0, 0);
        for one in &inbound {
            self.clock += 1;
            let done = block_on(self.engine.ingest(&mut self.board, self.clock, one)).expect("ingests");
            totals = (totals.0 + done.written, totals.1 + done.skipped);
        }
        totals
    }

    fn drain(&mut self) -> nanocode_board_sync::Drained {
        self.clock += 1;
        block_on(self.engine.drain(&mut self.board, self.clock)).expect("drains")
    }

    fn entries(&mut self) -> usize {
        block_on(self.board.refresh()).expect("refreshes");
        self.board.fold().entries().len()
    }

    fn mirror_to_slack(&mut self) {
        self.clock += 1;
        let link = SyncOp::Link { task: pr_task(), target: Target::new("slack", "C1"), join: Join::Channel, mode: Mode::Always };
        block_on(self.engine.apply(&mut self.board, "link-slack", self.clock, link)).expect("links");
    }
}

fn echo_payload(echo: &Echo, thread: &str) -> Value {
    let Echo::Posted { external, marker, text, .. } = echo else { panic!("a post echo") };
    event(json!({
        "type": "message", "channel": "C1", "bot_id": "B1", "text": text, "ts": external, "thread_ts": thread,
        "metadata": {"event_type": "board", "event_payload": {"board": marker.0}},
    }))
}

#[test]
fn end_to_end_github_pull_request_mirrors_to_slack_and_back() {
    let mut run = Loop::new(Fake::new("slack"));
    let opened = github::parse("gh-1", &pull("opened", json!({})));
    assert_eq!(run.webhook(opened.clone()), (2, 0));
    run.mirror_to_slack();
    let drained = run.drain();
    assert_eq!((drained.posted, drained.failed, drained.dead), (1, 0, 0));
    let copies = run.slack.copies();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].to, Target::new("slack", "C1"));
    assert!(copies[0].rendered.text.contains("Fix the rounding bug"), "{:?}", copies[0].rendered);
    assert!(run.github.copies().is_empty(), "the origin is never echoed to");

    let before = run.entries();
    let calls = run.slack.lock().calls;
    run.webhook(opened);
    assert_eq!(run.entries(), before, "a redelivery writes nothing");
    assert_eq!(run.drain().posted, 0);
    assert_eq!(run.slack.lock().calls, calls);

    let thread = copies[0].thread.clone().expect("a slack thread");
    let reply = event(json!({"type": "message", "channel": "C1", "user": "U1", "text": "needs a test", "ts": "1795084000.000900", "thread_ts": copies[0].external}));
    run.webhook(slack::parse("Ev-reply", &reply));
    let drained = run.drain();
    assert_eq!((drained.posted, drained.failed), (1, 0));
    let copies = run.github.copies();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].to, gh_target());
    assert!(copies[0].rendered.text.contains("needs a test"));
    assert!(thread.starts_with("C1:"));

    let before = run.entries();
    run.webhook(slack::parse("Ev-reply", &reply));
    assert_eq!(run.entries(), before);
    assert_eq!(run.drain().posted, 0);
    assert_eq!(run.github.copies().len(), 1);

    let comment = comment("created", 910, "thanks", &user("grace", "User"));
    run.webhook(github::parse("gh-2", &comment));
    assert_eq!(run.drain().posted, 1);
    assert_eq!(run.slack.copies().len(), 2);
    let before = run.entries();
    run.webhook(github::parse("gh-2", &comment));
    assert_eq!(run.entries(), before);
    assert_eq!(run.drain().posted, 0);

    drop(run.log);
}

#[test]
fn end_to_end_bot_echo_with_a_marker_is_a_receipt_not_a_message() {
    let mut calls = 0;
    let slack = Fake::with_faults("slack", move |_, _| {
        calls += 1;
        if calls == 2 { Fault::Lose } else { Fault::None }
    });
    let mut run = Loop::new(slack);
    run.webhook(github::parse("gh-1", &pull("opened", json!({}))));
    run.mirror_to_slack();
    assert_eq!(run.drain().posted, 1);
    let root = run.slack.copies()[0].external.clone();

    run.webhook(github::parse("gh-2", &comment("created", 911, "second opinion", &user("grace", "User"))));
    let drained = run.drain();
    assert_eq!((drained.posted, drained.failed), (0, 1));
    let echoes = run.slack.echoes();
    assert_eq!(echoes.len(), 2, "the lost post still reached slack");

    let before = run.entries();
    let payload = echo_payload(&echoes[1], &root);
    let parsed = slack::parse("Ev-echo", &payload);
    let Event::Message { author, marker, .. } = &parsed[0].event else { panic!("message") };
    assert!(author.bot);
    assert!(marker.is_some());
    run.webhook(parsed.clone());
    let after = run.entries();
    assert_eq!(after, before + 1, "one receipt, no imported message");
    let replies = run.board.fold().task(&pr_task()).expect("a task").replies.len();
    assert_eq!(replies, 1, "only the github comment is a reply, not the echo");

    let drained = run.drain();
    assert_eq!((drained.posted, drained.recovered, drained.failed), (0, 0, 0));
    assert_eq!(run.slack.copies().len(), 2, "nothing is posted twice");

    run.webhook(parsed);
    assert_eq!(run.entries(), after, "the echo redelivery is a no-op");
}
