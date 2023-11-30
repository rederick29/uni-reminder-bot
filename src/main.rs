pub(crate) mod backend;
pub(crate) mod commands;

use crate::{backend::data::{ReminderTable, Reminder}, commands::reminder::reminder};
use poise::{
    serenity_prelude::{Cache, Client, FullEvent, GatewayIntents, Http},
    FrameworkContext,
};
use tokio::task::JoinHandle;
use std::{
    ops::Deref,
    path::PathBuf,
    sync::{Arc, Mutex}, collections::HashMap,
};

// Accept any error type as error type
type Error = Box<dyn std::error::Error + Send + Sync>;

type Context<'a> = poise::Context<'a, Data, Error>;
type Data = UserData;

/// All data needed by bot
struct UserData {
    pub data: Arc<Mutex<ReminderTable>>,
    pub tasks: Arc<Mutex<HashMap<Reminder, JoinHandle<()>>>>,
    pub cache: Arc<Cache>,
    pub http: Arc<Http>,
}

impl UserData {
    pub fn new(cache: Arc<Cache>, http: Arc<Http>) -> Self {
        Self {
            data: Default::default(),
            tasks: Default::default(),
            cache,
            http,
        }
    }
}

impl Deref for UserData {
    type Target = Arc<Mutex<ReminderTable>>;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

async fn event_handler(
    event: &FullEvent,
    _: FrameworkContext<'_, UserData, Error>,
    _: &UserData,
) -> Result<(), Error> {
    match event {
        FullEvent::Ready {
            ctx: _,
            data_about_bot: ready,
        } => {
            if let Some(shard) = ready.shard {
                println!(
                    "{} is connected on shard {}/{}!",
                    ready.user.name, shard.id, shard.total
                );
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[tokio::main]
async fn main() {
    let token = std::env::var("DISCORD_TOKEN").expect("Missing DISCORD_TOKEN");
    let intents = GatewayIntents::non_privileged();

    // Load saved reminders from disk
    let path = PathBuf::from("./reminder_table");
    let mut loaded_table = match backend::load_data_from_path(&path) {
        Ok(table) => table,
        Err(_) => ReminderTable::new(),
    };

    let data = Arc::new(Mutex::new(loaded_table.clone()));
    // clone of data for moving into setup
    let data_i = data.clone();

    let framework = poise::Framework::new(
        poise::FrameworkOptions {
            commands: vec![reminder()],
            event_handler: |event, ctx, data| {
                Box::pin(async move { event_handler(event, ctx, data).await })
            },
            ..Default::default()
        },
        |ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                // Create user data with fresh cache and http, but with loaded reminder table
                let mut user_data = UserData::new(ctx.cache.clone(), ctx.http.clone());
                user_data.data = data_i;
                Ok(user_data)
            })
        },
    );

    let mut client = Client::builder(token, intents)
        .framework(framework)
        .await
        .unwrap();
    let manager = client.shard_manager.clone();

    // Saves the reminder table to disk every minute if it has changed
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let copy = {
                let lock = data.lock().unwrap();
                lock.clone()
            };

            if copy != loaded_table {
                use std::fs::File;

                loaded_table = copy;
                let file = File::create("reminder_table").expect("Couldn't create file.");
                serde_cbor::to_writer(file, &loaded_table).expect("Couldn't write to file.");
            }
        }
    });

    // Run discord bot client
    tokio::spawn(async move {
        if let Err(e) = client.start_shard(0, 1).await {
            eprintln!("Client error: {}", e);
        }
    });

    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            // Prevents changes from being lost if they were made <60 seconds before shutting down
            // Not ideal
            println!("Shutting down. Waiting for 60 seconds...");
            manager.shutdown_all().await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
        Err(err) => {
            eprintln!("Unable to listen for shutdown signal: {}", err);
        }
    }
}
