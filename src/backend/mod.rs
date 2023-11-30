pub(crate) mod data;

use std::fs::File;
use std::path::PathBuf;

use crate::{backend::data::ReminderTable, Error};

pub(crate) fn load_data_from_path(path: &PathBuf) -> Result<ReminderTable, Error> {
    let file = File::open(path)?;
    let reminders: ReminderTable = serde_cbor::from_reader(file)?;
    Ok(reminders)
}

