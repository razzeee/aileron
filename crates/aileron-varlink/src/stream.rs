//! Connection ownership and Send-compatible streaming for zlink 0.7.
//!
//! This is the only handwritten client transport adaptation. Method declarations
//! and messages come from zlink-codegen; callers cannot reuse an abandoned call.
use serde::Serialize;

use crate::inference::Error;
type Connection = zlink::tokio::unix::Connection;

#[derive(Debug)]
pub struct InferenceReplyStream<R> {
    connection: Option<Connection>,
    finished: bool,
    reply: std::marker::PhantomData<R>,
}

impl<R: serde::de::DeserializeOwned + std::fmt::Debug> InferenceReplyStream<R> {
    pub async fn next(&mut self) -> Option<zlink::Result<Result<R, Error>>> {
        if self.finished {
            return None;
        }
        let received = self
            .connection
            .as_mut()
            .expect("active stream has a connection")
            .receive_reply::<R, Error>()
            .await;
        Some(match received {
            Ok((Ok(reply), _fds)) => {
                self.finished = reply.continues() != Some(true);
                match reply.into_parameters() {
                    Some(parameters) => Ok(Ok(parameters)),
                    None => {
                        self.finished = true;
                        self.connection.take();
                        Err(zlink::Error::MissingParameters)
                    }
                }
            }
            Ok((Err(error), _fds)) => {
                self.finished = true;
                Ok(Err(error))
            }
            Err(error) => {
                self.finished = true;
                self.connection.take();
                Err(error)
            }
        })
    }
}

impl<R> InferenceReplyStream<R> {
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Only terminal success or an application error permits connection reuse.
    pub fn into_connection(self) -> Option<Connection> {
        if self.finished { self.connection } else { None }
    }
}

pub(crate) async fn start_stream<R>(
    mut connection: Connection,
    method: &'static str,
    parameters: serde_json::Value,
) -> zlink::Result<InferenceReplyStream<R>> {
    #[derive(Debug, Serialize)]
    struct MethodCall {
        method: &'static str,
        parameters: serde_json::Value,
    }
    let call = zlink::Call::new(MethodCall { method, parameters }).set_more(true);
    connection.send_call(&call, Vec::new()).await?;
    Ok(InferenceReplyStream {
        connection: Some(connection),
        finished: false,
        reply: std::marker::PhantomData,
    })
}
