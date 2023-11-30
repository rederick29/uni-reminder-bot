use crate::{Context, Error};
use poise::serenity_prelude::{
    ChannelId, FormattedTimestamp, FormattedTimestampStyle, GuildId, Timestamp, UserId, RoleId,
};
use serde::Deserialize;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    hash::Hash,
};

/// Possible times between repeats
#[non_exhaustive]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, poise::ChoiceParameter,
)]
pub(crate) enum Interval {
    #[cfg(debug_assertions)] FiveMinutesly, /// For testing purposes
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

// The debug formatter is good enough for display here
impl Display for Interval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, Clone, Copy, Eq, Serialize, Deserialize)]
pub(crate) struct Repeat {
    pub interval: Interval,
    index: u32,
}

impl Repeat {
    pub fn new(interval: Interval) -> Self {
        Self { interval, index: 0 }
    }

    /// Increases the internal index for keeping track of how many times a timer has repeated
    pub fn increment_index(&mut self) {
        self.index += 1;
    }

    /// Retrieves the next timestamp accounting for repeats from an initial timestamp
    pub fn next(&self, timestamp: &Timestamp) -> Timestamp {
        use chrono::Days;
        use chrono::Months;
        use chrono::NaiveDateTime;
        use Interval::*;

        let naive_date = timestamp.naive_utc();

        #[allow(unreachable_patterns)]
        let offset_date = match self.interval {
            #[cfg(debug_assertions)]
            FiveMinutesly => NaiveDateTime::from_timestamp_opt(timestamp.timestamp() + (300 * self.index as i64), 0),
            Hourly => NaiveDateTime::from_timestamp_opt(timestamp.timestamp(), 0),
            Daily => naive_date.checked_add_days(Days::new(self.index as u64)),
            Weekly => naive_date.checked_add_days(Days::new(7 * self.index as u64)),
            Monthly => naive_date.checked_add_months(Months::new(self.index)),
            Yearly => naive_date.checked_add_months(Months::new(12 * self.index)),
            _ => None,
        }
        .unwrap_or(naive_date);

        Timestamp::from_unix_timestamp(offset_date.timestamp()).unwrap()
    }
}

// The index field should be ignored when comparing
impl PartialEq for Repeat {
    fn eq(&self, other: &Self) -> bool {
        self.interval == other.interval
    }
}

// The index field should be ignored when hashing
impl Hash for Repeat {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.interval.hash(state);
    }
}

/// A reminder reminder containing a target timestamp and metadata at a minimum
#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
pub(crate) struct Reminder {
    registered_at: Timestamp,
    registered_by: UserId,
    /// Timestamp at which reminder is due
    pub target_date: Timestamp,
    pub repeating: Option<Repeat>,
    /// Short title/tag
    pub name: Option<String>,
    /// Attached discord roles
    pub roles: Option<Vec<RoleId>>,
    /// Main description/body text
    pub description: Option<String>,
}

// Reminders with the same target timestamp and repeat state should be considered the same.
impl PartialEq for Reminder {
    fn eq(&self, other: &Self) -> bool {
        (self.target_date == other.target_date) && (self.repeating == other.repeating)
    }
}

impl Hash for Reminder {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.target_date.hash(state);
        self.repeating.hash(state);
    }
}

impl Display for Reminder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{}eminder for {}{}",
            if let Some(repeating) = self.repeating {
                format!("{} r", repeating.interval)
            } else {
                "R".to_string()
            },
            FormattedTimestamp::new(
                self.target_date,
                Some(FormattedTimestampStyle::LongDateTime)
            ),
            if let Some(repeating) = self.repeating {
                format!(
                    " (next in {})",
                    FormattedTimestamp::new(
                        repeating.next(&self.target_date),
                        Some(FormattedTimestampStyle::RelativeTime)
                    )
                )
            } else {
                "".to_string()
            }
        )
    }
}

impl Reminder {
    pub fn from_context(
        ctx: &Context<'_>,
        target_date: Timestamp,
        repeating: Option<Repeat>,
        name: Option<String>,
        roles: Option<Vec<RoleId>>,
        description: Option<String>,
    ) -> Self {
        Self {
            registered_at: ctx.created_at(),
            registered_by: ctx.author().id,
            target_date,
            repeating,
            name,
            // put None if empty
            roles: if roles.as_ref().is_some_and(|v| !v.is_empty()) { roles } else { None },
            description: if description.as_ref().is_some_and(|s| !s.is_empty()) { description } else { None },
        }
    }

    pub fn get_creation(&self) -> (Timestamp, UserId) {
        (self.registered_at, self.registered_by)
    }
}

/// HashMap of reminders for each guild and channel pair
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ReminderTable {
    map: HashMap<(GuildId, ChannelId), HashSet<Reminder>>,
}

impl ReminderTable {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Add a reminder to the table for a guild/channel pair
    pub fn add_reminder(
        &mut self,
        guild_id: GuildId,
        channel_id: ChannelId,
        reminder: Reminder,
    ) -> Result<(), Error> {
        let Some(reminders) = self.get_reminders_mut(guild_id, channel_id) else {
            let mut set = HashSet::new();
            set.insert(reminder);
            self.map.insert((guild_id, channel_id), set);
            return Ok(());
        };

        reminders.insert(reminder);
        Ok(())
    }

    /// Remove a reminder already in the table
    /// Requires a copy of the originally added reminder, or at least one with the same timestamp and repeat.
    pub fn remove_reminder(
        &mut self,
        guild_id: GuildId,
        channel_id: ChannelId,
        reminder: &Reminder,
    ) -> Result<(), Error> {
        let Some(reminders) = self.get_reminders_mut(guild_id, channel_id) else {
            return Err(format!(
                "Guild {}, Channel {} does not have any reminders set.",
                guild_id, channel_id
            )
            .into());
        };

        match (reminders.remove(reminder), reminders.is_empty()) {
            (false, false) => Err(format!(
                "This reminder not present in Guild {}, Channel {}",
                guild_id, channel_id
            )
            .into()),
            (false, true) => {
                // there should never be a case when there is an empty set in the hashmap
                unreachable!();
            }
            (true, false) => Ok(()),
            (true, true) => {
                self.map.remove(&(guild_id, channel_id));
                Ok(())
            }
        }
    }

    /// Mutable reference to HashSet for guild/channel pair.
    /// Allows direct changes, but if the set becomes empty, it is not set to None in the map.
    pub fn get_reminders_mut(
        &mut self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Option<&mut HashSet<Reminder>> {
        self.map.get_mut(&(guild_id, channel_id))
    }

    pub fn get_reminders(
        &mut self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Option<&HashSet<Reminder>> {
        self.map.get(&(guild_id, channel_id))
    }
}

impl Default for ReminderTable {
    fn default() -> Self {
        ReminderTable::new()
    }
}
