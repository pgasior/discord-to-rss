use std::env;
use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;

use atom_syndication::{Feed, FeedBuilder};
use axum::http::header::{self, HeaderValue};
use axum::{Router, Extension};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use clap::Parser;
use log::{info, error, debug};
use ringbuffer::{AllocRingBuffer, RingBufferWrite, RingBufferExt};
use serenity::async_trait;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::id::ChannelId;
use serenity::prelude::*;

struct MessageHolderKey;

impl TypeMapKey for MessageHolderKey {
    type Value = Arc<RwLock<AllocRingBuffer<ReceivedMessage>>>;
}

#[derive(Clone, Debug)]
struct ReceivedMessage {
    content: String,
    author: String
}

impl From<&Message> for ReceivedMessage {
    fn from(item: &Message) -> Self {
        ReceivedMessage {
            content: item.content.clone(),
            author: item.author.name.clone()

        }
    }
}

struct AtomFeed(Feed);

impl Deref for AtomFeed {
    type Target = Feed;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}


impl IntoResponse for AtomFeed {
    fn into_response(self) -> Response {
        ([(header::CONTENT_TYPE, HeaderValue::from_static("application/atom+xml; charset=utf-8".as_ref()))], self.to_string()).into_response()
    }
}

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(long, value_parser)]
    discord_token: String,
    channel_id: String
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let cli = Cli::parse();

    let buffer = Arc::new(RwLock::new(AllocRingBuffer::<ReceivedMessage>::with_capacity(32)));

    let app = Router::new()
        .route("/", get(httphandler))
        .layer(Extension(buffer.clone()));

    // run it
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    info!("listening on {}", addr);
    let axum_server = axum::Server::bind(&addr)
        .serve(app.into_make_service());

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&cli.discord_token, intents).event_handler(Handler {channelID: cli.channel_id}).await.expect("Err creating client");
    {
        let mut data = client.data.write().await;
        data.insert::<MessageHolderKey>(buffer.clone());
    }

    tokio::select! {
        axum_result = axum_server => {
            error!("Axum stopped {:?}", axum_result)
        }
        serenity_result = client.start() => {
            error!("Serenity stopped {:?}", serenity_result.err())
        }
    };
}


// #[axum_macros::debug_handler]
async fn httphandler(Extension(buffer_lock): Extension<Arc<RwLock<AllocRingBuffer<ReceivedMessage>>>>) -> AtomFeed {
    let items: Vec<ReceivedMessage> = {
        let buffer = buffer_lock.read().await;
        buffer.iter().map(|i| i.clone()).collect()
    };

    let feed = FeedBuilder::default()
        .title("Feed title").build();
    AtomFeed(feed)
}

struct Handler {
    channelID: String
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        debug!("{:?}", msg);
        let buffer_lock = {
            let data_read = ctx.data.read().await;
            data_read.get::<MessageHolderKey>().unwrap().clone()
        };

        {
            let mut buffer = buffer_lock.write().await;
            buffer.push(ReceivedMessage::from(&msg));
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);
        let channel_id = ChannelId(996857942315913318);
        let messages_reversed = channel_id.messages(ctx.http, |retriever| {
            retriever.limit(20)
        }).await.unwrap().into_iter().rev().map(|element| element).collect::<Vec<Message>>();

        let buffer_lock = {
            let data_read = ctx.data.read().await;
            data_read.get::<MessageHolderKey>().unwrap().clone()
        };

        {
            let mut buffer = buffer_lock.write().await;
            for i in &messages_reversed {
                buffer.push(i.into())
            }
        }

        debug!("Messages: {:?}", messages_reversed)
    }
}
