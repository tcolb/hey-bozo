use std::{env, string};
use async_openai::{Client, types::{CreateThreadRequestArgs, CreateAssistantRequestArgs, AssistantObject, ThreadObject, CreateMessageRequestArgs, CreateRunRequestArgs, RunStatus, MessageContent}, config::OpenAIConfig};

pub struct DiscordAssistant {
    client: Client<OpenAIConfig>,
    thread: ThreadObject,
    assistant: AssistantObject,
    pub busy: bool
}

impl DiscordAssistant {
    pub async fn new() -> DiscordAssistant {
        let client = Client::new();
    
        let assistant_name = env::var("ASSISTANT_NAME").unwrap();
        let assistant_instructions = env::var("ASSISTANT_INSTRUCTIONS").unwrap();
        let assistant_model = env::var("ASSISTANT_MODEL").unwrap();
    
        let assistant_request = CreateAssistantRequestArgs::default()
            .name(&assistant_name)
            .instructions(&assistant_instructions)
            .model(&assistant_model)
            .build().unwrap();
        let assistant = client.assistants().create(assistant_request).await.unwrap();

        let thread_request = CreateThreadRequestArgs::default().build().unwrap();
        let thread = client.threads().create(thread_request.clone()).await.unwrap();

        DiscordAssistant {
            client: client,
            thread: thread,
            assistant: assistant,
            busy: false
        }
    }

    pub async fn send_message(&self, message_text: &str) -> Option<String> {
        let query = [("limit", "1")];

        let message = CreateMessageRequestArgs::default()
            .role("user")
            .content(message_text)
            .build().unwrap();

        self.client.threads().messages(&self.thread.id).create(message).await.unwrap();

        let run_request = CreateRunRequestArgs::default()
            .assistant_id(&self.assistant.id)
            .build().unwrap();

        let run = self.client
            .threads()
            .runs(&self.thread.id)
            .create(run_request)
            .await.unwrap();

        loop {
            let run = self.client
                .threads()
                .runs(&self.thread.id)
                .retrieve(&run.id)
                .await.unwrap();

            match run.status {
                RunStatus::Completed => {
                    let response = self.client
                        .threads()
                        .messages(&self.thread.id)
                        .list(&query)
                        .await.unwrap();

                    let message_id = response
                        .data.get(0).unwrap()
                        .id.clone();

                    let message = self.client
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
        self.thread = self.client.threads().create(thread_request.clone()).await.unwrap();
    }
}