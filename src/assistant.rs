use std::{env, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use async_openai::{Client, types::{CreateTranscriptionRequestArgs,
                                    AudioInput,
                                    ChatCompletionRequestSystemMessageArgs,
                                    CreateChatCompletionRequestArgs,
                                    ChatCompletionRequestMessage,
                                    ChatCompletionRequestUserMessageArgs,
                                    ChatCompletionRequestAssistantMessageArgs, ChatCompletionFunctions, FinishReason, ChatChoice, ChatCompletionMessageToolCall, FunctionCall}, config::OpenAIConfig};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::agent_speaker::AgentSpeaker;

#[derive(Clone)]
pub enum AssistantAction {
    MusicBot(MusicBotAction)
}

#[derive(Clone)]
pub enum MusicBotAction {
    Summon,
    Dismiss,
    Request(String)
}

pub struct DiscordAssistant {
    oai_client: Arc<Client<OpenAIConfig>>,
    pub speaker: AgentSpeaker,
    respondant: Option<u32>,
    is_responding: AtomicBool,
    messages: Vec<ChatCompletionRequestMessage>,
    functions: Vec<ChatCompletionFunctions>,
    assistant_model: String,
    assistant_pragma: String,
    action_channel: broadcast::Sender<AssistantAction>
}

impl DiscordAssistant {
    pub async fn new(oai_client: Arc<Client<OpenAIConfig>>, speaker: AgentSpeaker, action_channel: broadcast::Sender<AssistantAction>) -> DiscordAssistant {    
        let assistant_instructions = env::var("ASSISTANT_INSTRUCTIONS").unwrap();
        let assistant_model = env::var("ASSISTANT_MODEL").unwrap();

        DiscordAssistant {
            oai_client: oai_client,
            speaker: speaker,
            respondant: None,
            is_responding: AtomicBool::new(false),
            messages: Vec::default(),
            functions: Vec::default(),
            assistant_model: assistant_model,
            assistant_pragma: assistant_instructions,
            action_channel: action_channel
        }
    }

    pub async fn send_message(&mut self, audio_input: AudioInput) {
        self.is_responding.store(true, Ordering::SeqCst);

        let transcription_text = self.speech_to_text(audio_input).await;
        if !transcription_text.is_empty() {
            match self.get_response_choice(&transcription_text).await {
                Some(choice) => {
                    if let Some(finish_reason) = choice.finish_reason {
                        if FinishReason::FunctionCall == finish_reason {
                            self.handle_function_call(&choice.message.function_call.unwrap()).await;
                        } else {
                            let content = choice.message.content.clone().unwrap();
                            self.messages.push(ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessageArgs::default()
                                .content(content.clone())
                                .build().unwrap()));
                            self.speaker.speak(&content).await;
                        }
                    };
                },
                None => {
                    self.speaker.speak("Sorry, I'm a big dum guy and couldn't think of a response, tee hee!").await;
                }
            }
        }
        
        self.is_responding.store(false, Ordering::SeqCst);
    }

    pub fn try_grab_attention(&mut self, id: u32) -> bool {
        if let Some(respondant) = self.respondant {
            respondant == id
        }
        else {
            self.respondant = Some(id);
            true
        }
    }

    pub fn try_clear_attention(&mut self, id: u32) {
        if let Some(respondant) = self.respondant {
            if respondant == id {
                self.respondant = None;
            }
        }
    }

    pub fn get_attention_id(&self) -> Option<u32> {
        return self.respondant;
    }

    async fn get_response_choice(&mut self, message_text: &str) -> Option<ChatChoice> {
        self.messages.push(ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessageArgs::default()
            .content(message_text)
            .build().unwrap()));

        let request = CreateChatCompletionRequestArgs::default()
            .model(self.assistant_model.clone())
            .messages(self.messages.clone())
            .functions(self.functions.clone())
            .build().unwrap();

        let response = self.oai_client.chat().create(request).await;

        match response {
            Ok(response) => {
                if let Some(choice) = response.choices.first() {
                    return Some(choice.clone());
                } else {
                    None
                }
            },
            Err(_) => {
                None
            }
        }
    }
    
    async fn handle_function_call(&mut self, function_call: &FunctionCall) {
        match function_call.name.as_str() {
            "done" => {
                self.respondant = None;
                self.speaker.stop().await;
            },
            "summon_music_bot" => {
                self.action_channel.send(AssistantAction::MusicBot(MusicBotAction::Summon));
                self.speaker.speak(&function_call.arguments).await;
                self.respondant = None;
            },
            "dismiss_music_bot" => {
                self.action_channel.send(AssistantAction::MusicBot(MusicBotAction::Dismiss));
                self.speaker.speak(&function_call.arguments).await;
                self.respondant = None;
            }
            "request_music_bot" => {
                if let Ok(args) = serde_json::from_str::<Value>(&function_call.arguments) {
                    if let Some(title_value) = args.get("title") {
                        if let Some(title) = title_value.as_str() {
                            self.action_channel.send(AssistantAction::MusicBot(MusicBotAction::Request(title.into())));
                            self.speaker.speak("On it!").await;
                            self.respondant = None;
                            return;
                        }
                    }
                    self.speaker.speak("Sorry I made a fucky wucky!").await;
                    self.respondant = None;
                }    
            }            
            _ => {
                println!("unsupported function call: {}", function_call.name);
            }
        }
    }

    pub async fn flush(&mut self) {
        self.messages.clear();
        self.messages.push(ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessageArgs::default()
            .content(self.assistant_pragma.clone())
            .build().unwrap())
        );
        
        self.functions.clear();
        self.functions.push(ChatCompletionFunctions {
            name: "done".to_string(),
            description: Some("The user no longer wants to speak or already had their question answered.".to_string()),
            parameters: serde_json::from_str("{\"type\": \"object\", \"properties\": {}}").unwrap()
        });
        self.functions.push(ChatCompletionFunctions {
            name: "summon_music_bot".to_string(),
            description: Some("The user wants to add the music bot to the discord channel.".to_string()),
            parameters: serde_json::from_str("{\"type\": \"object\", \"properties\": {}}").unwrap()
        });
        self.functions.push(ChatCompletionFunctions {
            name: "dismiss_music_bot".to_string(),
            description: Some("The user wants to remove the music bot from the discord channel.".to_string()),
            parameters: serde_json::from_str("{\"type\": \"object\", \"properties\": {}}").unwrap()
        });
        self.functions.push(ChatCompletionFunctions {
            name: "request_music_bot".to_string(),
            description: Some("The user wants to add a song or video by title to the music bot queue.".to_string()),
            parameters: serde_json::from_str("{\"type\": \"object\", \"properties\": { \"title\": { \"type\": \"string\", \"description\": \"The title of the song or video, e.g. Abba - Dancing Queen\" } }}").unwrap()
        });
    }

    pub fn set_responding(&self) {
        self.is_responding.store(true, Ordering::SeqCst);
    }

    pub async fn is_responding(&self) -> bool {
        return self.is_responding.load(Ordering::SeqCst) || !self.speaker.is_finished().await;
    }

    pub async fn speech_to_text(&self, audio_input: AudioInput) -> String {
        let request = CreateTranscriptionRequestArgs::default()
            .file(audio_input)
            .model("whisper-1")
            .build().unwrap();

        let response = self.oai_client.audio().transcribe(request).await.unwrap();
        println!("stt: {}", response.text);
        return response.text;
    }
}