use std::sync::Arc;

use songbird::{Songbird, model::id::GuildId};

pub struct Speaker {
    songbird: Arc<Songbird>,
    guild_id: GuildId
}

impl Speaker {
    pub fn new(songbird: &mut Arc<Songbird>, guild_id: GuildId) -> Self {
        Speaker {
            songbird: songbird.clone(),
            guild_id: guild_id
        }
    }

    pub fn speak(text: &str) {
        
    }

    pub async fn stop() {

    }
}