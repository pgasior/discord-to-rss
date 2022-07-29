mod text2html;

use std::net::SocketAddr;
use std::ops::Deref;
use std::sync::Arc;

use atom_syndication::{
    ContentBuilder, EntryBuilder, Feed, FeedBuilder, FixedDateTime, LinkBuilder, PersonBuilder,
};
use axum::http::header::{self, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use clap::Parser;
use log::{debug, error, info};
use ringbuffer::{AllocRingBuffer, RingBufferExt, RingBufferWrite};
use serenity::async_trait;
use serenity::client::Cache;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::model::id::ChannelId;
use serenity::model::Timestamp;
use serenity::prelude::*;
use substring::Substring;
use text2html::text2html;
use tokio::signal;

struct MessageHolderKey;

impl TypeMapKey for MessageHolderKey {
    type Value = Arc<RwLock<AllocRingBuffer<ReceivedMessage>>>;
}

#[derive(Clone, Debug)]
struct ReceivedMessage {
    content: String,
    author: String,
    channel_name: String,
    id: String,
    created_timestamp: Timestamp,
    edited_timestamp: Timestamp,
    message_url: String,
}

impl ReceivedMessage {
    async fn from_discord_message(item: &Message, cache: &Cache) -> Self {
        Self {
            content: text2html(&item.content),
            author: item.author.name.clone(),
            channel_name: item
                .channel_id
                .name(cache)
                .await
                .unwrap_or_else(|| "Unknown Channel".into()),
            created_timestamp: item.timestamp,
            edited_timestamp: item.edited_timestamp.unwrap_or(item.timestamp),
            id: item.id.as_u64().to_string(),
            message_url: item.link(),
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
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/atom+xml; charset=utf-8"),
            )],
            self.to_string(),
        )
            .into_response()
    }
}

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Cli {
    #[clap(long, value_parser, env)]
    discord_token: String,
    #[clap(long, value_parser, env)]
    channel_id: String,
    #[clap(long, value_parser, env, default_value = "127.0.0.1")]
    bind_address: String,
    #[clap(long, value_parser, env, default_value = "3000")]
    bind_port: String,
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    let cli = Cli::parse();

    let buffer = Arc::new(RwLock::new(
        AllocRingBuffer::<ReceivedMessage>::with_capacity(32),
    ));

    let app = Router::new().route(
        "/",
        get({
            let buffer = buffer.clone();
            move || httphandler(buffer.clone())
        }),
    );

    // run it
    let addr_string = format!("{}:{}", &cli.bind_address, &cli.bind_port);
    let addr = addr_string
        .parse::<SocketAddr>()
        .unwrap_or_else(|_| panic!("Invalid  bind address {}", &addr_string));
    info!("listening on {}", addr);
    let axum_server = axum::Server::bind(&addr).serve(app.into_make_service());

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILDS;

    let mut client = Client::builder(&cli.discord_token, intents)
        .event_handler(Handler {
            channel_id: ChannelId(cli.channel_id.parse::<u64>().expect("Wrong ChannelId")),
        })
        .await
        .expect("Err creating client");
    {
        let mut data = client.data.write().await;
        data.insert::<MessageHolderKey>(buffer.clone());
    }

    tokio::select! {
        axum_result = axum_server => {
            error!("Axum stopped {:?}", axum_result);
        }
        serenity_result = client.start() => {
            error!("Serenity stopped {:?}", serenity_result.err());
        }
        _ = signal::ctrl_c() => {
            info!("Exiting on Ctrl-C");
            client.shard_manager.lock().await.shutdown_all().await;
            info!("Shard manager shut down");
        }
    };
}

// #[axum_macros::debug_handler]
async fn httphandler(buffer_lock: Arc<RwLock<AllocRingBuffer<ReceivedMessage>>>) -> AtomFeed {
    let items: Vec<ReceivedMessage> = {
        let buffer = buffer_lock.read().await;
        buffer.iter().cloned().collect()
    };

    let mut feed_builder = FeedBuilder::default();
    feed_builder.title("Discord messages");

    for item in items.iter().rev() {
        feed_builder.entry(
            EntryBuilder::default()
                .title(format!(
                    "{}: {}",
                    &item.author,
                    item.content.to_string().substring(0, 80)
                ))
                .content(Some(
                    ContentBuilder::default()
                        .value(Some(item.content.clone()))
                        .build(),
                ))
                .authors([PersonBuilder::default().name(item.author.clone()).build()])
                .published(
                    FixedDateTime::parse_from_rfc3339(&item.created_timestamp.to_rfc3339()).ok(),
                )
                .updated(
                    FixedDateTime::parse_from_rfc3339(&item.edited_timestamp.to_rfc3339()).unwrap(),
                )
                .links([LinkBuilder::default()
                    .href(item.message_url.clone())
                    .build()])
                .id(item.id.clone())
                .build(),
        );
    }
    AtomFeed(feed_builder.build())
}

struct Handler {
    channel_id: ChannelId,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.channel_id == self.channel_id {
            debug!("{:?}", msg);
            let buffer_lock = {
                let data_read = ctx.data.read().await;
                data_read.get::<MessageHolderKey>().unwrap().clone()
            };

            {
                let mut buffer = buffer_lock.write().await;
                buffer.push(ReceivedMessage::from_discord_message(&msg, &ctx.cache).await);
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);

        let messages_reversed = self
            .channel_id
            .messages(ctx.http, |retriever| retriever.limit(20))
            .await
            .unwrap()
            .into_iter()
            .rev()
            .collect::<Vec<Message>>();

        let buffer_lock = {
            let data_read = ctx.data.read().await;
            data_read.get::<MessageHolderKey>().unwrap().clone()
        };

        {
            let mut buffer = buffer_lock.write().await;
            for i in &messages_reversed {
                buffer.push(ReceivedMessage::from_discord_message(i, &ctx.cache).await);
            }
        }

        debug!("Messages: {:?}", messages_reversed);
    }
}
