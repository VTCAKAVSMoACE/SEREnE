#![cfg(target_os = "linux")]

mod sandbox;

use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::framework::standard::{
    macros::{command, group},
    Args, CommandResult, StandardFramework,
};
use serenity::model::channel::Message;

use crate::sandbox::SandboxManager;
use serde::Deserialize;
use serenity::http::AttachmentType;
use serenity::prelude::TypeMapKey;
use std::borrow::Cow;
use std::error::Error;
use std::sync::Arc;
use thrussh_keys::key::KeyPair;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::RwLock;

#[derive(Deserialize)]
struct SereneConfig {
    token: String,
    host: String,
}

#[group]
#[commands(ping)]
struct General;

#[group]
#[commands(destroy_sandbox, spawn_sandbox)]
struct Sandbox;

struct SandboxWrapper;

impl TypeMapKey for SandboxWrapper {
    type Value = Arc<RwLock<SandboxManager>>;
}

struct Host;

impl TypeMapKey for Host {
    type Value = Arc<String>;
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let framework = StandardFramework::new()
        .configure(|c| c.prefix("~"))
        .group(&GENERAL_GROUP)
        .group(&SANDBOX_GROUP);

    let mut config = File::open("serene.toml").await?;
    let mut config_content = String::new();
    config.read_to_string(&mut config_content).await?;
    let config: SereneConfig = toml::from_slice(config_content.as_ref()).unwrap();

    let mut client = Client::builder(config.token)
        .event_handler(Handler)
        .framework(framework)
        .await
        .expect("Error creating client");
    {
        let mut data = client.data.write().await;

        data.insert::<SandboxWrapper>(Arc::new(RwLock::new(SandboxManager::new().await?)));
        data.insert::<Host>(Arc::new(config.host));
    }

    if let Err(why) = client.start().await {
        println!("An error occurred while running the client: {:?}", why);
    }
    {
        let data = client.data.read().await;

        data.get::<SandboxWrapper>()
            .unwrap()
            .clone()
            .write()
            .await
            .teardown()
            .await?;
    }
    Ok(())
}

#[command]
async fn ping(ctx: &Context, msg: &Message) -> CommandResult {
    msg.reply(ctx, "Pong!").await?;

    Ok(())
}

#[command("spawn")]
async fn spawn_sandbox(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let (sandbox_lock, host) = {
        let data_read = ctx.data.read().await;

        (
            data_read
                .get::<SandboxWrapper>()
                .expect("Expected SandboxWrapper in TypeMap.")
                .clone(),
            data_read
                .get::<Host>()
                .expect("Expected Host in TypeMap.")
                .clone(),
        )
    };

    let existing = {
        let manager = sandbox_lock.read().await;
        manager.find_sandbox(msg.author.id.0)
    };
    if let Some(port) = existing {
        msg.channel_id
            .send_message(ctx, |m| {
                m.content(format!(
                    "A message has already been made available to you on port {}",
                    port
                ));
                m
            })
            .await?;
        Ok(())
    } else {
        let mut keypair = None;
        let pubkey;
        if args.is_empty() {
            keypair = Some(Arc::new(
                KeyPair::generate_ed25519().expect("keypair generation is supposed to be stable!"),
            ));
            pubkey = keypair.clone().unwrap().clone_public_key();
        } else {
            let _algo = args.single::<String>()?;
            let data = args.single::<String>()?;
            pubkey = thrussh_keys::parse_public_key_base64(&data)?;
        }

        let port = {
            let mut manager = sandbox_lock.write().await;

            manager.create_sandbox(msg.author.id.0, pubkey).await?
        };

        if port.is_some() {
            msg.channel_id
                .send_message(ctx, |m| {
                    if keypair.is_some() {
                        let mut s = Vec::new();
                        let writable = Box::new(&mut s);
                        thrussh_keys::encode_pkcs8_pem(
                            &*match keypair {
                                Some(x) => x,
                                None => unimplemented!(),
                            },
                            writable,
                        )
                        .unwrap();
                        m.add_file(AttachmentType::Bytes {
                            data: Cow::from(s),
                            filename: "serene-id_ed25519".to_string(),
                        });
                        m.content(format!(
                            "Started a sandbox for you; connect with: ```ssh -i serene-id_ed25519 -p {} serene@{}```",
                            port.unwrap(),
                            host
                        ));
                    } else {
                        m.content(format!(
                            "Started a sandbox for you; connect with: ```ssh -p {} serene@{}```",
                            port.unwrap(),
                            host
                        ));
                    }
                    m
                })
                .await?;
        }
        Ok(())
    }
}

#[command("destroy")]
async fn destroy_sandbox(ctx: &Context, msg: &Message) -> CommandResult {
    let sandbox_lock = {
        let data_read = ctx.data.read().await;

        data_read
            .get::<SandboxWrapper>()
            .expect("Expected SandboxWrapper in TypeMap.")
            .clone()
    };

    let destroyed = {
        let mut manager = sandbox_lock.write().await;
        manager.destroy_sandbox(msg.author.id.0).await?
    };

    msg.channel_id
        .send_message(ctx, |m| {
            if destroyed {
                m.content("Sandbox destroyed.");
            } else {
                m.content("No sandbox to destroy; ignoring.");
            }
            m
        })
        .await?;
    Ok(())
}