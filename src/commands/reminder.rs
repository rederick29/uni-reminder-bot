use std::{time::{SystemTime, Duration}, collections::HashMap, sync::{Arc, Mutex}};

use poise::{
    serenity_prelude::{
        self as serenity, CreateEmbed, FormattedTimestamp, FormattedTimestampStyle, Timestamp, parse_role_mention, RoleId, Mention, Http, Cache, ChannelId, GuildId,
    },
    CreateReply,
};
use tokio::task::JoinHandle;

use crate::{
    backend::data::{Interval, Reminder, Repeat, ReminderTable},
    commands::{get_data, send_reminder},
    Context, Error,
};

// Creates an async task to send a reminder at the correct time.
// Implicitly stores the task handle for the created task in the `task` parameter.
// awful way of doing this but I cannot thing of any better way without unsafe
async fn schedule_reminder_message(guild_id: GuildId, channel_id: ChannelId, cache_http: (Arc<Cache>, Arc<Http>), reminder: Reminder, reminders: Arc<Mutex<ReminderTable>>, tasks: Arc<Mutex<HashMap<Reminder, JoinHandle<()>>>>) -> Result<(), Error> {
    // destructure cache_http for cloning later
    let (cache, http) = cache_http;
    let target = reminder.target_date;
    // if the reminder is repeating, it is possible this is not the first time it is ran.
    // if so, use the timestamp from its next method instead
    let timestamp = match reminder.repeating {
        Some(repeat) => repeat.next(&target).timestamp(),
        None => target.timestamp(),
    };

    // hacky way of getting a duration from a unix timestamp
    let sleep_duration = (SystemTime::UNIX_EPOCH + Duration::from_secs(timestamp as u64)).duration_since(SystemTime::now())?;

    // cloning for use in the async move block
    let reminder_clone = reminder.clone();
    let tasks_clone = tasks.clone();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(sleep_duration).await;
        let _ = send_reminder(channel_id, (&(cache.clone()), &(http.clone())), &reminder_clone).await;

        // for repeating reminders, reminder needs to be updated and a new task needs to be spawned
        match reminder_clone.repeating {
            Some(mut repeat) => {
                // cloning the whole hashset for a single if-statement. not good.
                let reminders_locked = {
                    let mut r = reminders.lock().unwrap();
                    r.get_reminders_mut(guild_id, channel_id).cloned()
                };

                // re-create reminder with an increased repeat count
                let new_reminder = {
                    let mut r = reminder_clone.clone();
                    repeat.increment_index();
                    r.repeating = Some(repeat);
                    r
                };

                if reminders_locked.is_some() {
                    {
                        let mut lock = reminders.lock().unwrap();
                        let reminders_set = lock.get_reminders_mut(guild_id, channel_id).unwrap();
                        // replace old reminder with one with higher repeat count
                        reminders_set.replace(new_reminder.clone());
                    }

                    // https://github.com/rust-lang/rust/issues/78649#issuecomment-1264353351
                    // recursive aync is not allowed in rust. so I used the workaround above
                    #[inline(always)]
                    fn recurse_schedule(guild_id: GuildId, channel_id: ChannelId, cache: Arc<Cache>, http: Arc<Http>, new_reminder: Reminder, reminders: Arc<Mutex<ReminderTable>>, tasks_clone: Arc<Mutex<HashMap<Reminder, JoinHandle<()>>>>) -> poise::BoxFuture<'static, Result<(), Error>> {
                        Box::pin(schedule_reminder_message(guild_id, channel_id, (cache, http) , new_reminder, reminders, tasks_clone))
                    }
                    let _ = recurse_schedule(guild_id, channel_id, cache.clone(), http.clone(), new_reminder, reminders.clone(), tasks_clone).await;

                }
            }
            None => {
                let mut reminders = reminders.lock().unwrap();
                // if the reminder doesn't repeat it can be removed after its done
                let _ = reminders.remove_reminder(guild_id, channel_id, &reminder_clone);
            }
        };
    });

    {
        tasks.lock().unwrap().insert(reminder, handle);
    }
    Ok(())
}

#[poise::command(
    slash_command,
    subcommands("add", "remove", "list", "info"),
    subcommand_required
)]
pub(crate) async fn reminder(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command)]
pub(crate) async fn add(
    ctx: Context<'_>,
    #[min = 1]
    #[description = "Unix Timestamp"]
    datetime: i64,
    #[description = "Repeat interval"] interval: Option<Interval>,
    #[description = "Reminder name"] name: Option<String>,
    #[description = "Reminder text"] text: Option<String>,
    #[description = "Target Channel"]
    #[channel_types("Text")]
    channel: Option<serenity::GuildChannel>,
    #[description = "Space-separated list of roles to be mentioned."]
    roles: Option<String>
) -> Result<(), Error> {
    let mut reply = CreateReply::default();

    let roles = roles.map(|roles| roles
        .split_whitespace()
        .filter_map(parse_role_mention)
        .collect::<Vec<RoleId>>());

    if datetime <= chrono::Utc::now().timestamp() {
        reply = reply
            .content("Timestamp must be in the future!")
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    // maximum character count in a embed description is 4096
    if text.clone().is_some_and(|s| s.chars().count() > 4096) {
        reply = reply
            .content("The reminder text body is must be less than 4096 characters long!")
            .ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    let datetime = match Timestamp::from_unix_timestamp(datetime) {
        Ok(datetime) => datetime,
        Err(e) => {
            reply = reply
                .content(format!("Invalid timestamp provided: {}", e))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    let (guild_id, channel_id) = match get_data(&ctx, channel).await {
        Ok((guild_id, channel_id)) => (guild_id, channel_id),
        Err(error) => {
            reply = reply
                .content(format!("An error occured: {}", error))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    // create Repeat from Interval
    let mut repeat = None;
    if let Some(interval) = interval {
        repeat = Some(Repeat::new(interval));
    }

    let data = ctx.data();
    let http = data.http.clone();
    let cache = data.cache.clone();

    // create reminder and schedule it
    let reminder = Reminder::from_context(&ctx, datetime, repeat, name, roles, text);
    {
        if let Err(e) = data.lock().unwrap().add_reminder(guild_id, channel_id, reminder.clone()) {
            reply = reply.content(format!("An error occured: {}", e)).ephemeral(true);
        }
        let _ = schedule_reminder_message(guild_id, channel_id, (cache, http), reminder, data.data.clone(), data.tasks.clone()).await;
    }

    reply = reply.content("Added!").ephemeral(true);
    ctx.send(reply).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub(crate) async fn list(
    ctx: Context<'_>,
    #[description = "Target Channel"]
    #[channel_types("Text")]
    channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let mut reply = CreateReply::default();
    let mut embed = CreateEmbed::default();

    let (guild_id, channel_id) = match get_data(&ctx, channel).await {
        Ok((guild_id, channel_id)) => (guild_id, channel_id),
        Err(error) => {
            reply = reply
                .content(format!("An error occured: {}", error))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    let cache = serenity::CacheHttp::cache(&ctx).unwrap();
    let http = serenity::CacheHttp::http(&ctx);
    let channel_name = channel_id.to_channel((cache, http)).await?;

    let data = ctx.data();
    {
        let mut lock = data.lock().unwrap();
        match lock.get_reminders(guild_id, channel_id) {
            None => {
                reply = reply.content(format!("No reminders set for channel {}", channel_name));
                reply = reply.ephemeral(true);
            }
            Some(reminders) => {
                embed = embed
                    .title(format!("Reminders set for channel {}", channel_name))
                    .description(format!("Reminders: {}", reminders.len()))
                    .fields(reminders.iter().enumerate().map(|(n, v)| {
                        let title = match &v.name {
                            Some(name) => format!("{} ({})", n + 1, name),
                            None => (n + 1).to_string(),
                        };

                        (title, format!("{}", v), false)
                    }));
                reply = reply.embed(embed);
            }
        };
    }

    ctx.send(reply).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub(crate) async fn remove(
    ctx: Context<'_>,
    #[description = "Reminder number (from list command)"] id: u16,
    #[description = "Target Channel"]
    #[channel_types("Text")]
    channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let mut reply = CreateReply::default();

    // The reminders list command starts at 1
    if id < 1 {
        reply = reply.content("Reminder id must be 1 or greater.");
        reply = reply.ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    let (guild_id, channel_id) = match get_data(&ctx, channel).await {
        Ok((guild_id, channel_id)) => (guild_id, channel_id),
        Err(error) => {
            reply = reply
                .content(format!("An error occured: {}", error))
                .ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    let reminders = {
        let mut lock = ctx.data().lock().unwrap();
        lock.get_reminders(guild_id, channel_id).cloned()
    };

    if let Some(reminders) = reminders {
        if let Some((_, reminder)) = reminders
            .iter()
            .enumerate()
            .find(|(i, _)| *i == (id - 1) as usize)
        {
            let mut lock = ctx.data().lock().unwrap();
            lock.remove_reminder(guild_id, channel_id, reminder).unwrap();
            {
                let mut lock = ctx.data().tasks.lock().unwrap();
                let handle = lock.remove(reminder).unwrap();
                handle.abort();
            }
            reply = reply.content("Removed!");
        } else {
            reply = reply.content(format!("Reminder id {} was not found in this channel.", id)).ephemeral(true);
        }
    } else {
        reply = reply.content("No reminders have been set for this channel.").ephemeral(true);
    }

    ctx.send(reply).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub(crate) async fn info(
    ctx: Context<'_>,
    #[description = "Reminder number (from list command)"] id: u16,
    #[description = "Target Channel"]
    #[channel_types("Text")]
    channel: Option<serenity::GuildChannel>,
) -> Result<(), Error> {
    let mut reply = CreateReply::default();

    // The reminders list command starts at 1
    if id < 1 {
        reply = reply.content("Reminder id must be 1 or greater.");
        reply = reply.ephemeral(true);
        ctx.send(reply).await?;
        return Ok(());
    }

    let mut embed = CreateEmbed::default();

    let (guild_id, channel_id) = match get_data(&ctx, channel).await {
        Ok((guild_id, channel_id)) => (guild_id, channel_id),
        Err(error) => {
            reply = reply
                .content(format!("An error occured: {}", error))
                .ephemeral(true);
            reply = reply.ephemeral(true);
            ctx.send(reply).await?;
            return Ok(());
        }
    };

    let cache = serenity::CacheHttp::cache(&ctx).unwrap();
    let http = serenity::CacheHttp::http(&ctx);
    let channel_name = channel_id.to_channel((cache, http)).await?;

    // This clones the hashset for the current channel
    // Not ideal but we can't use .await otherwise
    // (needed for getting the user that created the reminder)
    let reminders = {
        let mut lock = ctx.data().lock().unwrap();
        lock.get_reminders(guild_id, channel_id).cloned()
    };

    embed = embed.title(format!(
        "Information for reminder {} in channel {}:",
        id, channel_name
    ));

    match reminders {
        None => {
            embed = embed.description("No reminders have been set for this channel!");
        }
        Some(reminders) => {
            if let Some(reminder) = reminders
                .iter()
                .enumerate()
                .find(|(i, _)| *i == (id - 1) as usize)
            {
                let (_, reminder) = reminder;
                let title = match reminder.name.clone() {
                    Some(name) => name,
                    None => "Not set".to_string(),
                };
                let text_body = match reminder.description.clone() {
                    Some(text) => text,
                    None => "Not set".to_string(),
                };
                let (created_at, created_by) = reminder.get_creation();
                let target_date = reminder.target_date;
                let repeating = reminder.repeating;

                let roles = match &reminder.roles {
                    Some(roles) => {
                        let mut text = String::from("Attached roles: ");
                        roles
                            .iter()
                            .map(|role| Mention::from(*role))
                            .for_each(|mention: Mention| text += &format!("{} ", mention));
                        text
                    }
                    None => "No roles attached.".to_string()
                };

                let repeat_info = match repeating {
                    Some(repeat) => {
                        format!(
                            "Repeating {}\nNext: {}",
                            repeat.interval,
                            FormattedTimestamp::new(
                                repeat.next(&target_date),
                                Some(FormattedTimestampStyle::RelativeTime)
                            )
                        )
                    }
                    None => "Single-time".to_string(),
                };

                let description = format!(
                    "Name: {}\n\
                    Text body: {}\n\n\
                    {}\n\
                    Created at: {}\n\
                    Created by: {}\n\
                    \n\
                    Registered for: {}\n\
                    {}",
                    title,
                    text_body,
                    roles,
                    FormattedTimestamp::new(
                        created_at,
                        Some(FormattedTimestampStyle::LongDateTime)
                    ),
                    created_by.to_user((cache, http)).await?,
                    FormattedTimestamp::new(
                        target_date,
                        Some(FormattedTimestampStyle::LongDateTime)
                    ),
                    repeat_info,
                );
                embed = embed.description(description);
            } else {
                embed =
                    embed.description(format!("Reminder id {} was not found in this channel.", id));
            }
        }
    };

    reply = reply.embed(embed);
    ctx.send(reply).await?;
    Ok(())
}
