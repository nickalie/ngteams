//! What the server's other services do in the chat service: open a thread of their own (a meeting's chat), change
//! who is in it, and post or change a message as an account that is in it. Each goes through the same rows and
//! the same fan-out as a client's request, so every reader and every sync sees it as one.

use std::sync::Mutex;

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Map, Value, json};

use super::activity::Told;
use super::repo::Chat;
pub(crate) use super::repo::set_topic;
use super::schema::{chat_members, chat_messages, chat_properties, chats};
use super::{Caller, events, message_repo, property_repo, repo, streams, wire};
use crate::app::App;
use crate::db::schema::users;
use crate::db::users::User;
use crate::db::{Db, DbError, with_conn};

fn caller(app: &App, user: &User) -> Caller {
    Caller {
        id: user.id.clone(),
        tenant: app.issuer.tenant().to_owned(),
        name: user.display_name.clone(),
    }
}

/// Opens the thread `id` with `creator` alone in it.
pub(crate) async fn open_thread(db: &Db, id: &str, creator: &str, topic: Option<&str>) -> Result<(), DbError> {
    let chat = Chat {
        id: id.to_owned(),
        creator: creator.to_owned(),
        topic: topic.map(str::to_owned),
        unique_roster: false,
        created_at: 0,
        version: 0,
        roster_version: 0,
    };
    if repo::create(db, &chat, &[creator.to_owned()]).await? {
        Ok(())
    } else {
        Err(DbError::Conflict)
    }
}

/// Tells everyone in the thread that it changed, as its creation told them.
pub(crate) async fn announce(app: &App, thread: &str, as_user: &User) -> Result<(), DbError> {
    let Some(chat) = repo::chat(&app.db, thread).await? else {
        return Ok(());
    };
    let members = repo::members(&app.db, thread).await?;
    let caller = caller(app, as_user);
    let mut resource = wire::thread(app, &caller, &chat, &members);
    crate::calendar::describe_thread(app, thread, &mut resource["properties"]).await?;
    events::publish(app, &members, "ThreadUpdate", &resource).await;
    Ok(())
}

/// A message the server posts for `author`, who is in the thread.
pub(crate) struct Post<'a> {
    pub thread: &'a str,
    pub author: &'a User,
    pub kind: &'a str,
    pub content: &'a str,
    pub properties: Map<String, Value>,
    /// In a channel: the post it replies under, or `None` for a post of its own.
    pub under: Option<i64>,
}

/// Who is in the thread.
pub(crate) async fn members(db: &Db, thread: &str) -> Result<Vec<String>, DbError> {
    let members = repo::members(db, thread).await?;
    Ok(members.into_iter().map(|member| member.user_id).collect())
}

/// Posts it and tells the thread's people; its id, or `None` when the author isn't in the thread.
pub(crate) async fn post(app: &App, post: Post<'_>) -> Result<Option<i64>, DbError> {
    let Some(posted) = keep(app, post).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(Some(posted.row.id))
}

/// Keeps it without telling anyone yet: for a message that names its own id, which [`set_property`] then tells
/// as the new message it is. Its id, or `None` when the author isn't in the thread.
pub(crate) async fn post_untold(app: &App, post: Post<'_>) -> Result<Option<i64>, DbError> {
    let Some(posted) = keep(app, post).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &Value::Null, &[], &posted.told).await;
    Ok(Some(posted.row.id))
}

/// A message the server posts as an app (`28:<id>`), which is none of the thread's people; its row names the app's
/// own account (`users.kind` `app`).
pub(crate) struct AppPost<'a> {
    pub thread: &'a str,
    /// The app's account id.
    pub app: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub content: &'a str,
    pub properties: Map<String, Value>,
}

/// Posts it (a new post of its own in a channel) and tells the thread's people; its id, or `None` when the thread
/// isn't there.
pub(crate) async fn post_as_app(app: &App, post: AppPost<'_>) -> Result<Option<i64>, DbError> {
    let in_channel = super::channels::place(&app.db, post.thread).await?.is_some();
    keep_as_app(app, post, in_channel.then_some(0)).await
}

async fn keep_as_app(app: &App, post: AppPost<'_>, root_id: Option<i64>) -> Result<Option<i64>, DbError> {
    let base = wire::base(app);
    let draft = app_draft(post, &base, root_id);
    let Some(posted) = message_repo::insert_by(&app.db, &draft, &base, message_repo::Poster::App).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(Some(posted.row.id))
}

fn app_draft(post: AppPost<'_>, base: &str, root_id: Option<i64>) -> message_repo::Message {
    let body = json!({
        "type": "Message", "conversationid": post.thread,
        "conversationLink": format!("{base}/v1/users/ME/conversations/{}", post.thread),
        "from": format!("{base}/v1/users/ME/contacts/28:{}", post.app),
        "fromDisplayNameInToken": post.name, "imdisplayname": post.name,
        "messagetype": post.kind, "contenttype": "Text", "content": post.content,
        "properties": Value::Object(post.properties),
    });
    message_repo::Message {
        chat_id: post.thread.to_owned(),
        id: 0,
        owner_id: post.app.to_owned(),
        client_id: None,
        version: 0,
        body: body.to_string(),
        root_id,
    }
}

/// Keeps an app's reply in the caller's transaction.
pub(crate) async fn reply_in<C: message_repo::MessageConnection>(
    conn: &mut C,
    app: &App,
    post: AppPost<'_>,
    root: i64,
) -> Result<Option<message_repo::Posted>, DbError> {
    let base = wire::base(app);
    let draft = app_draft(post, &base, Some(root));
    conn.insert_message(&draft, &base, message_repo::Poster::App, wire::now())
        .await
}

/// Tells the readers about a message after its transaction commits.
pub(crate) async fn announce_posted(app: &App, posted: &message_repo::Posted) -> Result<(), DbError> {
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(())
}

async fn keep(app: &App, post: Post<'_>) -> Result<Option<message_repo::Posted>, DbError> {
    let caller = caller(app, post.author);
    let base = wire::base(app);
    let body = json!({
        "type": "Message", "conversationid": post.thread,
        "conversationLink": format!("{base}/v1/users/ME/conversations/{}", post.thread),
        "from": format!("{base}/v1/users/ME/contacts/{}", caller.mri()),
        "fromDisplayNameInToken": caller.name, "imdisplayname": caller.name,
        "messagetype": post.kind, "contenttype": "Text", "content": post.content,
        "properties": post.properties,
    });
    let root_id = super::channels::root(&app.db, post.thread, post.under).await?;
    let draft = message_repo::Message {
        chat_id: post.thread.to_owned(),
        id: 0,
        owner_id: caller.id.clone(),
        client_id: None,
        version: 0,
        body: body.to_string(),
        root_id,
    };
    message_repo::insert(&app.db, &draft, &base).await
}

/// One property of a message its author posted, set.
pub(crate) struct Property<'a> {
    pub thread: &'a str,
    pub message: i64,
    pub author: &'a User,
    pub name: &'a str,
    pub value: &'a Value,
    /// What the thread's people are told it is: `MessageUpdate`, or `NewMessage` after [`post_untold`].
    pub told_as: &'a str,
}

/// Sets it and tells the thread's people. Whether the message was there.
pub(crate) async fn set_property(app: &App, set: Property<'_>) -> Result<bool, DbError> {
    let Property {
        thread,
        message,
        author,
        name,
        value,
        told_as,
    } = set;
    let base = wire::base(app);
    let change = message_repo::Change {
        chat: thread,
        id: message,
        actor: &author.id,
        who: message_repo::Who::Owner,
        effect: message_repo::Effect::None,
        base: &base,
    };
    let changed = message_repo::update(&app.db, change, |body, _| {
        body["properties"][name] = value.clone();
        Ok(true)
    })
    .await?;
    let message_repo::Updated::Message(changed) = changed else {
        return Ok(false);
    };
    events::tell(app, told_as, &changed.row.value()?, &changed.readers, &changed.told).await;
    Ok(true)
}

/// Something another service tells accounts of in their activity feeds.
pub(crate) struct Activity<'a> {
    pub people: &'a [String],
    /// The thread it happened in. An account that muted the thread is told only of what is `personal`.
    pub thread: Option<&'a str>,
    /// It names the account itself, as a mention of it does.
    pub personal: bool,
    /// The feed message for an account, under the id the feed gives it.
    pub item: &'a (dyn Fn(&str, i64) -> Value + Sync),
}

/// Writes one item into the feed of each of the people whose account is in use and whose settings of the thread
/// want it, and tells each on its push channel. An item is a message of the account's own stream, as a mention's
/// is: it is synced, marked read and removed the same way.
pub(crate) async fn tell_activity(app: &App, activity: Activity<'_>) -> Result<(), DbError> {
    keep_items(app, streams::FEED, activity).await
}

/// The account's activity feed and its call log, as [`stream_message`] names them.
pub(crate) const FEED: &str = streams::FEED;
pub(crate) const CALL_LOG: &str = streams::CALL_LOG;

/// Writes one entry into the call log of each of `people` whose account is in use, and tells each on its push
/// channel, as [`tell_activity`] does an activity item.
pub(crate) async fn tell_call_log(
    app: &App,
    people: &[String],
    entry: &(dyn Fn(&str, i64) -> Value + Sync),
) -> Result<(), DbError> {
    let logged = Activity {
        people,
        thread: None,
        personal: true,
        item: entry,
    };
    keep_items(app, streams::CALL_LOG, logged).await
}

/// A message of the account's own stream `stream` (its feed or its call log) under the id `at` the stream gives it,
/// from `from` (an MRI), as the chat service delivers one.
pub(crate) fn stream_message(
    app: &App,
    stream: &str,
    (at, from): (i64, &str),
    content: &str,
    properties: &Value,
) -> Value {
    let base = wire::base(app);
    let time = wire::iso(at);
    json!({
        "type": "Message", "id": at.to_string(), "version": at.to_string(), "sequenceId": at,
        "clientmessageid": at.to_string(), "messagetype": "Text", "contenttype": "Text", "content": content,
        "conversationid": stream, "to": stream,
        "threadtype": format!("streamof{}", stream.trim_start_matches("48:")),
        "conversationLink": format!("{base}/v1/users/ME/conversations/{stream}"),
        "from": format!("{base}/v1/users/ME/contacts/{from}"), "imdisplayname": "",
        "composetime": time, "originalarrivaltime": time,
        "properties": properties,
    })
}

async fn keep_items(app: &App, stream_name: &'static str, activity: Activity<'_>) -> Result<(), DbError> {
    let at = wire::now();
    let told: Vec<Told> = with_conn!(&app.db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let mut told = Vec::new();
            for user in activity.people {
                let muted = match activity.thread {
                    Some(thread) if !activity.personal => {
                        let own: Option<String> = chat_properties::table
                            .find((thread, user))
                            .select(chat_properties::body)
                            .first(conn)
                            .await
                            .optional()?;
                        property_repo::decode(own)?.get("alerts").and_then(Value::as_str) == Some("false")
                    }
                    _ => false,
                };
                let active: i64 = users::table
                    .filter(users::id.eq(user))
                    .filter(users::disabled.eq(false))
                    .count()
                    .get_result(conn)
                    .await?;
                if muted || active == 0 {
                    continue;
                }
                let version: i64 = repo::advance!(conn, at);
                let stream = streams::stored_id(stream_name, user);
                streams::ensure_in!(conn, &stream, user, version);
                let mut resource = (activity.item)(user, version);
                resource["version"] = version.to_string().into();
                let row = message_repo::Message {
                    chat_id: stream.clone(),
                    id: version,
                    owner_id: user.clone(),
                    client_id: None,
                    version,
                    body: resource.to_string(),
                    root_id: None,
                };
                diesel::insert_into(chat_messages::table)
                    .values(&row)
                    .execute(conn)
                    .await?;
                diesel::update(chats::table.find(&stream))
                    .set(chats::version.eq(version))
                    .execute(conn)
                    .await?;
                told.push(Told {
                    user: user.clone(),
                    kind: "NewMessage",
                    resource,
                });
            }
            Ok(told)
        })
        .await)?;
    events::tell(app, "NewMessage", &Value::Null, &[], &told).await;
    Ok(())
}

/// A message a bot posts in a chat it is in, as the member it is (`28:<id>`).
pub(crate) struct BotPost<'a> {
    pub thread: &'a str,
    /// The bot's account id.
    pub bot: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub content: &'a str,
    pub pictures: &'a [String],
}

/// Posts the message and tells the thread's people. Returns its id, or `None` if the bot is absent.
/// Returns `DbError::Conflict` if a picture reference no longer exists for this chat.
pub(crate) async fn bot_post(app: &App, post: BotPost<'_>) -> Result<Option<i64>, DbError> {
    let base = wire::base(app);
    let mut body = json!({
        "type": "Message", "conversationid": post.thread,
        "conversationLink": format!("{base}/v1/users/ME/conversations/{}", post.thread),
        "from": format!("{base}/v1/users/ME/contacts/28:{}", post.bot),
        "fromDisplayNameInToken": post.name, "imdisplayname": post.name,
        "messagetype": post.kind, "contenttype": "text", "content": post.content,
        "properties": {},
    });
    picture_references(&mut body, post.pictures);
    let root_id = super::channels::root(&app.db, post.thread, None).await?;
    let draft = message_repo::Message {
        chat_id: post.thread.to_owned(),
        id: 0,
        owner_id: post.bot.to_owned(),
        client_id: None,
        version: 0,
        body: body.to_string(),
        root_id,
    };
    let Some(posted) = message_repo::insert_by(&app.db, &draft, &base, message_repo::Poster::Bot).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(Some(posted.row.id))
}

fn picture_references(body: &mut Value, pictures: &[String]) {
    if let Some(body) = body.as_object_mut() {
        body.remove("amsreferences");
        if !pictures.is_empty() {
            body.insert("amsreferences".into(), json!(pictures));
        }
    }
}

/// What became of a bot's change of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BotChange {
    Done,
    /// The message is absent or deleted, or a picture reference no longer exists for this chat.
    Missing,
    /// Not the bot's own message, or a thread it isn't in.
    Denied,
}

async fn bot_change<F>(
    app: &App,
    thread: &str,
    bot: &str,
    id: i64,
    effect: message_repo::Effect<'_>,
    transform: F,
) -> Result<BotChange, DbError>
where
    F: Fn(&mut Value, i64) -> Result<bool, DbError> + Sync,
{
    let base = wire::base(app);
    let change = message_repo::Change {
        chat: thread,
        id,
        actor: bot,
        who: message_repo::Who::BotOwner,
        effect,
        base: &base,
    };
    let shown = Mutex::new(Vec::new());
    let changed = match message_repo::update(&app.db, change, |body, at| {
        super::mutations::keep(&shown, body);
        transform(body, at)
    })
    .await
    {
        Err(DbError::Conflict) => return Ok(BotChange::Missing),
        other => other?,
    };
    match changed {
        message_repo::Updated::Missing => Ok(BotChange::Missing),
        message_repo::Updated::Denied => Ok(BotChange::Denied),
        message_repo::Updated::Unchanged(_) => Ok(BotChange::Done),
        message_repo::Updated::Message(changed) => {
            crate::media::release(app, bot, &shown.into_inner().unwrap_or_default()).await;
            events::tell(
                app,
                "MessageUpdate",
                &changed.row.value()?,
                &changed.readers,
                &changed.told,
            )
            .await;
            Ok(BotChange::Done)
        }
    }
}

/// The bot's own message `id`, with the content and pictures of `post`.
pub(crate) async fn bot_edit(app: &App, post: BotPost<'_>, id: i64) -> Result<BotChange, DbError> {
    bot_change(
        app,
        post.thread,
        post.bot,
        id,
        message_repo::Effect::Edited,
        |body, at| {
            if body["properties"].get("deletetime").is_some() {
                return Err(DbError::Conflict);
            }
            body["content"] = post.content.into();
            body["messagetype"] = post.kind.into();
            picture_references(body, post.pictures);
            body["properties"]["edittime"] = json!(at);
            Ok(true)
        },
    )
    .await
}

/// The bot's own message `id`, soft-deleted as a person's is.
pub(crate) async fn bot_delete(app: &App, thread: &str, bot: &str, id: i64) -> Result<BotChange, DbError> {
    bot_change(app, thread, bot, id, message_repo::Effect::Deleted, |body, at| {
        if body["properties"].get("deletetime").is_some() {
            return Err(DbError::Conflict);
        }
        body["content"] = "".into();
        body["properties"] = json!({"deletetime": at});
        picture_references(body, &[]);
        Ok(true)
    })
    .await
}

/// The bot typing in `thread`, told to its people; whether the bot is in it.
pub(crate) async fn bot_typing(app: &App, thread: &str, bot: &str, name: &str) -> Result<bool, DbError> {
    let Some((at, people)) = repo::tick(&app.db, thread, bot).await? else {
        return Ok(false);
    };
    let base = wire::base(app);
    let resource = json!({
        "type": "Message", "id": at.to_string(), "version": at.to_string(), "clientmessageid": at.to_string(),
        "conversationid": thread, "conversationLink": format!("{base}/v1/users/ME/conversations/{thread}"),
        "from": format!("{base}/v1/users/ME/contacts/28:{bot}"), "imdisplayname": name,
        "messagetype": "Control/Typing", "contenttype": "Application/Message", "content": "",
        "properties": {"importance": "", "subject": ""},
    });
    events::send(
        app,
        people.iter().map(String::as_str).filter(|person| *person != bot),
        "NewMessage",
        &resource,
    )
    .await;
    Ok(true)
}

/// The people of `thread` when the bot is in it.
pub(crate) async fn bot_roster(app: &App, thread: &str, bot: &str) -> Result<Option<Vec<User>>, DbError> {
    let members = repo::members(&app.db, thread).await?;
    if !members.iter().any(|member| member.user_id == bot) {
        return Ok(None);
    }
    let mut people = Vec::new();
    for member in members.iter().filter(|member| !member.app) {
        if let Some(user) = crate::db::users::by_id(&app.db, &member.user_id).await? {
            people.push(user);
        }
    }
    Ok(Some(people))
}

/// The 1:1 chat of `person` and `bot` (`19:<a>_<b>@unq.gbl.spaces`, the two ids in order), made when it isn't there
/// and the person's account is in use; its id, or `None` for an account that isn't.
pub(crate) async fn bot_chat(app: &App, person: &str, bot: &str) -> Result<Option<String>, DbError> {
    let id = crate::bots::personal_chat(person, bot);
    let at = wire::now();
    let made: Option<bool> = with_conn!(&app.db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = repo::advance!(conn, at);
            let active: i64 = users::table
                .filter(users::id.eq(person))
                .filter(users::disabled.eq(false))
                .filter(crate::db::users::person())
                .count()
                .get_result(conn)
                .await?;
            if active == 0 {
                return Ok(None);
            }
            let chat = Chat {
                id: id.clone(),
                creator: person.to_owned(),
                topic: None,
                unique_roster: true,
                created_at: version,
                version,
                roster_version: version,
            };
            let inserted = diesel::insert_into(chats::table)
                .values(&chat)
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            if inserted == 0 {
                return Ok(Some(false));
            }
            for (position, user) in [person, bot].into_iter().enumerate() {
                diesel::insert_into(chat_members::table)
                    .values((
                        chat_members::chat_id.eq(&id),
                        chat_members::user_id.eq(user),
                        chat_members::position.eq(i32::try_from(position).unwrap_or(i32::MAX)),
                        chat_members::joined_at.eq(version),
                    ))
                    .execute(conn)
                    .await?;
            }
            Ok(Some(true))
        })
        .await)?;
    match made {
        None => Ok(None),
        Some(made) => {
            if made && let Some(user) = crate::db::users::by_id(&app.db, person).await? {
                announce(app, &id, &user).await?;
            }
            Ok(Some(id))
        }
    }
}

/// Where a person's invoke of a bot acts ([`invoke_place`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InvokePlace {
    /// A 1:1 chat or a group chat both are in now.
    Chat { personal: bool },
    /// A channel both are in now: its id, and its team's id and group.
    Channel {
        channel: String,
        team: String,
        group: String,
    },
    /// One of them isn't in the thread, or there is no such thread.
    Denied,
    /// The message named isn't one the person may read there.
    NoMessage,
}

/// Where `person` asks `bot` in `thread` (a chat, or a channel's thread).
/// Each of `messages` (the channel post and the invoke's `messageId`) must be readable by `person` there.
pub(crate) async fn invoke_place(
    app: &App,
    thread: &str,
    person: &str,
    bot: &str,
    messages: &[i64],
) -> Result<InvokePlace, DbError> {
    let Some(chat) = repo::chat(&app.db, thread).await? else {
        return Ok(InvokePlace::Denied);
    };
    let members = repo::members(&app.db, thread).await?;
    let person_in = members.iter().any(|member| member.user_id == person && !member.app);
    let bot_in = members.iter().any(|member| member.user_id == bot && member.app);
    if !person_in || !bot_in {
        return Ok(InvokePlace::Denied);
    }
    let readable = message_repo::readable(&app.db, thread, person, messages).await?;
    if messages.iter().any(|id| !readable.contains(id)) {
        return Ok(InvokePlace::NoMessage);
    }
    Ok(match super::channels::place(&app.db, thread).await? {
        Some(place) => InvokePlace::Channel {
            channel: place.channel.id,
            team: place.team.id,
            group: place.team.group_id,
        },
        None => InvokePlace::Chat {
            personal: chat.unique_roster,
        },
    })
}
