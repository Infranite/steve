//! tungstenite (the Node's dialler, and tests) as a [`Frame`] link.

use futures_util::{SinkExt, StreamExt, future};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::envelope::{Frame, FrameSink, FrameStream, MAX_FRAME};

/// Oversize messages are refused by the socket before anything parses them.
pub fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
}

fn io(e: WsError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

pub fn split<S>(ws: WebSocketStream<S>) -> (FrameSink, FrameStream)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (tx, rx) = ws.split();
    let tx = tx.sink_map_err(io).with(|f: Frame| {
        future::ready(Ok::<_, std::io::Error>(match f {
            Frame::Text(t) => Message::Text(t.into()),
            Frame::Close(code, reason) => Message::Close(Some(CloseFrame {
                code: CloseCode::from(code),
                reason: reason.into(),
            })),
        }))
    });
    let rx = rx.filter_map(|m| {
        future::ready(match m {
            Ok(Message::Text(t)) => Some(Ok(Frame::Text(t.to_string()))),
            Ok(Message::Close(c)) => Some(Ok(match c {
                Some(c) => Frame::Close(c.code.into(), c.reason.to_string()),
                None => Frame::Close(1005, String::new()),
            })),
            Ok(Message::Binary(_)) => Some(Err(std::io::Error::other("binary frame"))),
            Ok(_) => None,
            Err(e) => Some(Err(io(e))),
        })
    });
    (Box::pin(tx), Box::pin(rx))
}
