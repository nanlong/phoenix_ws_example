use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Query, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use futures::StreamExt;
use serde_json::Value;
use tokio::sync::Mutex;

use super::socket::{Socket, SocketState};
use super::user_channel::UserChannel;

pub struct UserSocket {
    channels: HashMap<String, Arc<Mutex<UserChannel>>>,
}

impl UserSocket {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    pub fn channel(&mut self, channel: UserChannel) {
        self.channels
            .insert(channel.topic(), Arc::new(Mutex::new(channel)));
    }

    async fn websocket(
        ws: WebSocketUpgrade,
        Query(params): Query<Value>,
        Extension(state): Extension<Arc<Mutex<SocketState>>>,
        Extension(user_socket): Extension<Arc<Mutex<UserSocket>>>,
    ) -> impl IntoResponse {
        // todo: handle join failed
        tracing::info!("params: {:?}", params["token"]);
        let socket = Arc::new(Mutex::new(Socket::default()));
        ws.on_upgrade(|stream| Self::handle_stream(stream, state, socket, user_socket))
    }

    async fn handle_stream(
        stream: WebSocket,
        state: Arc<Mutex<SocketState>>,
        socket: Arc<Mutex<Socket>>,
        user_socket: Arc<Mutex<UserSocket>>,
    ) {
        let (sender, mut receiver) = stream.split();
        let mut rx = state.lock().await.tx.subscribe();
        let socket1 = socket.clone();
        let socket2 = socket.clone();

        socket
            .lock()
            .await
            .update(Arc::new(Mutex::new(sender)), state);

        let mut recv_task = tokio::spawn(async move {
            while let Some(Ok(Message::Text(message))) = receiver.next().await {
                let message: Value = serde_json::from_str(&message).unwrap();
                let mut user_socket = user_socket.lock().await;
                socket1.lock().await.from_message(message.clone());

                match (message[2].as_str().unwrap(), message[3].as_str().unwrap()) {
                    (_, "heartbeat") => {
                        socket1.lock().await.reply("ok", Value::Null).await;
                    }
                    // phx_join | phx_leave | other_event
                    (topic, event) => {
                        if let Some(channel) = user_socket.channels.get_mut(topic) {
                            let mut channel = channel.lock().await;
                            let payload = message[4].clone();
                            channel.dispatch(event, payload, socket1.clone()).await;
                        }
                    }
                }
            }
        });

        let mut send_task = tokio::spawn(async move {
            while let Ok(message) = rx.recv().await {
                let message: Value = serde_json::from_str(&message).unwrap();
                let action = message["action"].as_str().unwrap();

                match action {
                    "boardcast" => socket2.lock().await.send(message["payload"].clone()).await,
                    "boardcast_from" => {
                        let from = message["from"].as_str().unwrap();

                        if from != socket2.lock().await.id().as_str() {
                            socket2.lock().await.send(message["payload"].clone()).await;
                        }
                    }
                    _ => (),
                }
            }
        });

        tokio::select! {
            _ = (&mut send_task) => recv_task.abort(),
            _ = (&mut recv_task) => send_task.abort(),
        }
    }

    pub fn router(user_socket: Arc<Mutex<UserSocket>>) -> Router {
        Router::new()
            .route("/socket/websocket", get(Self::websocket))
            .layer(Extension(user_socket))
            .layer(Extension(Arc::new(Mutex::new(SocketState::default()))))
    }
}
