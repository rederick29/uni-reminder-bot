use std::sync::Arc;

use poise::serenity_prelude::{self as serenity, ChannelId, GuildId, CreateEmbed, Mention, FormattedTimestamp, FormattedTimestampStyle, CreateMessage, Http, Cache};

use crate::{Context, Error, backend::data::Reminder};

pub(crate) mod reminder;

/// Helper function to get the guild and channel ids
pub(crate) async fn get_data(
    ctx: &Context<'_>,
    channel: Option<serenity::GuildChannel>,
) -> Result<(GuildId, ChannelId), Error> {
    let (guild_id, channel_id) = match channel {
        Some(channel) => (channel.guild_id, channel.id),
        None => match ctx.guild_id() {
            Some(guild_id) => (guild_id, ctx.channel_id()),
            None => {
                return Err("This command is only available in servers!".into());
            }
        },
    };

    Ok((guild_id, channel_id))
}

/// Creates and sends the message for a reminder.
pub(crate) async fn send_reminder(channel_id: ChannelId, cache_http: (&Arc<Cache>, &Http), reminder: &Reminder) -> Result<(), Error> {
    let guild_channel = match channel_id.to_channel(cache_http).await?.guild() {
        Some(guild_channel) => guild_channel,
        None => return Err("Failed to find channel for reminder!".into()),
    };

    let mut reply = CreateMessage::default();
    let mut embed = CreateEmbed::default();

    let title = match reminder.name.clone() {
        Some(title) => title,
        None => "Reminder".to_string(),
    };

    let roles = match &reminder.roles {
        Some(roles) => {
            let mut text = String::from("Ping ");
                roles
                    .iter()
                    .map(|role| Mention::from(*role))
                    .for_each(|mention: Mention| text += &format!("{} ", mention));
                text
        }
        None => String::new(),
    };

    let mut description = format!("Set for {}", FormattedTimestamp::new(reminder.target_date, Some(FormattedTimestampStyle::LongDateTime)));
    if let Some(text) = &reminder.description {
        description = format!("{}\n{}", text, description);
    }

    embed = embed
        .title(title)
        .description(description);

    reply = reply
        .content(roles)
        .embed(embed);

    guild_channel.send_message(cache_http, reply).await?;
    Ok(())
}
