use std::sync::Arc;

use serenity::{model::{channel::GuildChannel, id::ChannelId}, http::Http};
use tokio::sync::broadcast;

use crate::assistant::{AssistantAction, MusicBotAction};

pub async fn action_handler_loop(http: Arc<Http>, channel: ChannelId, mut action_rx: broadcast::Receiver<AssistantAction>) {
    loop {
        let action = action_rx.recv().await.unwrap();
    
        match action {
            AssistantAction::MusicBot(music_bot_action) => {
                match music_bot_action {
                    MusicBotAction::Summon => {
                        channel.say(&http, "=join").await;
                    }
                    MusicBotAction::Dismiss => {
                        channel.say(&http, "=leave").await;
                    }
                    MusicBotAction::Request(title) => {
                        channel.say(&http, format!("=p {}", title)).await;
                    }
                    MusicBotAction::Skip => {
                        channel.say(&http, "=skip").await;
                    }
                    MusicBotAction::Shuffle => {
                        channel.say(&http, "=shuffle").await;
                    }
                    MusicBotAction::Loop => {
                        channel.say(&http, "=loop").await;
                    }
                    MusicBotAction::Clear => {
                        channel.say(&http, "=clear").await;
                    }
                    MusicBotAction::BassBoost => {
                        channel.say(&http, "=bb").await;
                    }
                    MusicBotAction::PlayPlaylist(playlist) => {
                        channel.say(&http, format!("=playlist play {}", playlist)).await;
                    }
                }
            }
        }
    }
}
