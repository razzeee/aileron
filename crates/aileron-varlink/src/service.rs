//! Compatibility with clients that explicitly send empty Varlink parameters.

use serde::Deserialize;
use serde_json::Value;
use zlink::service::{HandleResult, MethodReply};

/// zlink 0.7 deserializes argument-free methods as unit enum variants, which
/// reject `parameters: {}`. Older systemd clients always send that valid form.
/// Normalize it before dispatch, preserving call flags and native streaming.
pub struct CompatibleService<S>(pub S);

impl<S, Sock> zlink::Service<Sock> for CompatibleService<S>
where
    Sock: zlink::connection::Socket,
    S: zlink::Service<Sock>,
{
    type MethodCall<'de> = Value;
    type ReplyParams<'ser>
        = Value
    where
        Self: 'ser;
    type ReplyError<'ser>
        = Value
    where
        Self: 'ser;
    type ReplyStreamParams = S::ReplyStreamParams;
    type ReplyStreamError = S::ReplyStreamError;
    type ReplyStream = S::ReplyStream;

    async fn handle<'ser>(
        &'ser mut self,
        method: &'ser zlink::Call<Self::MethodCall<'_>>,
        connection: &mut zlink::Connection<Sock>,
        fds: Vec<std::os::fd::OwnedFd>,
    ) -> HandleResult<Value, Self::ReplyStream, Value> {
        let mut message = method.method().clone();
        if message
            .get("parameters")
            .and_then(Value::as_object)
            .is_some_and(|p| p.is_empty())
        {
            message.as_object_mut().unwrap().remove("parameters");
        }
        let parsed = match S::MethodCall::deserialize(&message) {
            Ok(parsed) => parsed,
            Err(_) => {
                return (
                    MethodReply::Error(serde_json::json!({
                        "error": "org.varlink.service.InvalidParameter",
                        "parameters": {"parameter": "parameters"}
                    })),
                    vec![],
                );
            }
        };
        let call = zlink::Call::new(parsed)
            .set_more(method.more())
            .set_oneway(method.oneway())
            .set_upgrade(method.upgrade());
        let (reply, fds) = self.0.handle(&call, connection, fds).await;
        let reply = match reply {
            MethodReply::Single(parameters) => {
                match parameters.map(serde_json::to_value).transpose() {
                    Ok(parameters) => MethodReply::Single(parameters),
                    Err(_) => MethodReply::Error(serialization_error()),
                }
            }
            MethodReply::Error(error) => MethodReply::Error(
                serde_json::to_value(error).unwrap_or_else(|_| serialization_error()),
            ),
            MethodReply::Multi(stream) => MethodReply::Multi(stream),
        };
        (reply, fds)
    }
}

fn serialization_error() -> Value {
    serde_json::json!({
        "error": "org.varlink.service.InternalError",
        "parameters": {}
    })
}
