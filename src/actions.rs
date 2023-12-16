#[derive(Clone)]
#[derive(Debug)]
pub enum AssistantAction {
    MusicBot(MusicBotAction)
}

#[derive(Clone)]
#[derive(Debug)]
pub enum MusicBotAction {
    Summon,
    Dismiss,
    Request(String),
    Skip,
    Shuffle,
    Loop,
    Clear,
    BassBoost,
    PlayPlaylist(String)
}