use std::{env, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use async_openai::{Client, types::{CreateThreadRequestArgs,
     CreateAssistantRequestArgs,
      AssistantObject,
       ThreadObject,
        CreateMessageRequestArgs,
         CreateRunRequestArgs,
          RunStatus,
           MessageContent,
            CreateTranscriptionRequestArgs,
             AudioInput,
              ChatCompletionRequestSystemMessageArgs, CreateChatCompletionRequestArgs, ChatCompletionRequestMessage}, config::OpenAIConfig};

use crate::agent_speaker::AgentSpeaker;

pub struct DiscordAssistant {
    oai_client: Arc<Client<OpenAIConfig>>,
    thread: ThreadObject,
    assistant: AssistantObject,
    pub is_in_conversation: bool,
    pub speaker: AgentSpeaker,
    is_responding: AtomicBool,
    messages: Vec<ChatCompletionRequestMessage>,
    assistant_model: String,
    assistant_pragma: String
}

impl DiscordAssistant {
    pub async fn new(oai_client: Arc<Client<OpenAIConfig>>, speaker: AgentSpeaker) -> DiscordAssistant {    
        let assistant_name = env::var("ASSISTANT_NAME").unwrap();
        let assistant_instructions = env::var("ASSISTANT_INSTRUCTIONS").unwrap();
        let assistant_model = env::var("ASSISTANT_MODEL").unwrap();
    
        let assistant_request = CreateAssistantRequestArgs::default()
            .name(&assistant_name)
            .instructions(&assistant_instructions)
            .model(&assistant_model)
            .build().unwrap();
        let assistant = oai_client.assistants().create(assistant_request).await.unwrap();

        let thread_request = CreateThreadRequestArgs::default().build().unwrap();
        let thread = oai_client.threads().create(thread_request.clone()).await.unwrap();

        DiscordAssistant {
            oai_client: oai_client,
            thread: thread,
            assistant: assistant,
            is_in_conversation: false,
            speaker: speaker,
            is_responding: AtomicBool::new(false),
            messages: Vec::default(),
            assistant_model: assistant_model,
            assistant_pragma: assistant_instructions
        }
    }

    pub async fn send_message(&mut self, audio_input: AudioInput) {
        self.is_responding.store(true, Ordering::SeqCst);

        let transcription_text = self.speech_to_text(audio_input).await;
        if !transcription_text.is_empty() {
            match self.get_response(&transcription_text).await {
                Some(text) => {
                    self.speaker.speak(&text);
                },
                None => {
                    self.speaker.speak("Sorry, I'm a big dum guy and couldn't think of a response, tee hee!");
                }
            }
        }
        
        self.is_responding.store(false, Ordering::SeqCst);
    }

    // pub async fn send_message(&mut self, message_text: &str) {
    //     self.is_responding.store(true, Ordering::SeqCst);
    //     match self.get_response(message_text).await {
    //         Some(text) => {
    //             self.speaker.speak(&text);
    //         },
    //         None => {
    //             self.speaker.speak("Sorry, I'm a big dum guy and couldn't think of a response, tee hee!");
    //         }
    //     }
    //     self.is_responding.store(false, Ordering::SeqCst);
    // }

    async fn get_response_completion(&self, message_text: &str) -> Option<String> {
        let request = CreateChatCompletionRequestArgs::default()
            .model("gpt-3.5-turbo")
            .messages([
                ChatCompletionRequestSystemMessageArgs::default()
                    .content("You are a helpful assistant.")
                    .build().unwrap()
                    .into(),
                ChatCompletionRequestUserMessageArgs::default()
                    .content("Who won the world series in 2020?")
                    .build()?
                    .into(),
                ChatCompletionRequestAssistantMessageArgs::default()
                    .content("The Los Angeles Dodgers won the World Series in 2020.")
                    .build()?
                    .into(),
                ChatCompletionRequestUserMessageArgs::default()
                    .content("Where was it played?")
                    .build()?
                    .into(),
            ])
            .build()?;

        println!("{}", serde_json::to_string(&request).unwrap());

        let response = client.chat().create(request).await?;

        None
    }

    async fn get_response(&self, message_text: &str) -> Option<String> {
        let query = [("limit", "1")];

        let message = CreateMessageRequestArgs::default()
            .role("user")
            .content(message_text)
            .build().unwrap();

        self.oai_client.threads().messages(&self.thread.id).create(message).await.unwrap();

        let run_request = CreateRunRequestArgs::default()
            .assistant_id(&self.assistant.id)
            .build().unwrap();

        let run = self.oai_client
            .threads()
            .runs(&self.thread.id)
            .create(run_request)
            .await.unwrap();

        loop {
            let run = self.oai_client
                .threads()
                .runs(&self.thread.id)
                .retrieve(&run.id)
                .await.unwrap();

            match run.status {
                RunStatus::Completed => {
                    let response = self.oai_client
                        .threads()
                        .messages(&self.thread.id)
                        .list(&query)
                        .await.unwrap();

                    let message_id = response
                        .data.get(0).unwrap()
                        .id.clone();

                    let message = self.oai_client
                        .threads()
                        .messages(&self.thread.id)
                        .retrieve(&message_id)
                        .await.unwrap();

                    let content = message
                        .content.get(0).unwrap();
                    
                    let text = match content {
                        MessageContent::Text(text) => text.text.value.clone(),
                        MessageContent::ImageFile(_) => panic!("Images are not supported"),
                    };

                    return Some(text);
                }
                RunStatus::Failed => {
                    println!("--- Run Failed: {:#?}", run);
                    return None;
                }
                RunStatus::Queued => {
                    println!("--- Run Queued");
                },
                RunStatus::Cancelling => {
                    println!("--- Run Cancelling");
                },
                RunStatus::Cancelled => {
                    println!("--- Run Cancelled");
                },
                RunStatus::Expired => {
                    println!("--- Run Expired");
                },
                RunStatus::RequiresAction => {
                    println!("--- Run Requires Action");
                },
                RunStatus::InProgress => {
                    println!("--- Waiting for response...");
                }
            }

            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
    
    pub async fn flush(&mut self) {
        let thread_request = CreateThreadRequestArgs::default().build().unwrap();
        self.thread = self.oai_client.threads().create(thread_request.clone()).await.unwrap();

        self.messages.clear();
        self.messages.append(ChatCompletionRequestSystemMessageArgs::default()
        .content(self.assistant_pragma)
        .build().unwrap()
        .into());
    }

    pub async fn is_responding(&self) -> bool {
        return self.is_responding.load(Ordering::SeqCst) || self.speaker.is_speaking().await;
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