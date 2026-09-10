use aileron_varlink::inference::{EmbedOptions, VarlinkStreamingClientInterface};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn abandoned_stream_closes_socket_and_cannot_return_connection() {
    for read_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.socket");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let connection = zlink::tokio::unix::connect(&path).await.unwrap();
        let mut stream = connection
            .stream_embed(
                "session".into(),
                "text".into(),
                EmbedOptions {
                    execution_mode: "interactive".into(),
                },
            )
            .await
            .unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(peer);
        let mut request = Vec::new();
        peer.read_until(0, &mut request).await.unwrap();
        assert_eq!(request.pop(), Some(0));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request).unwrap(),
            serde_json::json!({
                "method": "aileron.Inference.StreamEmbed", "more": true,
                "parameters": {"session_id": "session", "text": "text", "options": {"execution_mode": "interactive"}}
            })
        );
        if read_first {
            peer.get_mut().write_all(b"{\"parameters\":{\"embedding\":[1.0],\"embedding_pipeline_id\":\"old\"},\"continues\":true}\0{\"parameters\":{\"embedding\":[2.0],\"embedding_pipeline_id\":\"old\"}}\0").await.unwrap();
            assert_eq!(
                stream.next().await.unwrap().unwrap().unwrap().embedding,
                [1.0]
            );
        }
        assert!(stream.into_connection().is_none());
        let mut byte = [0];
        match tokio::time::timeout(std::time::Duration::from_secs(1), peer.read(&mut byte))
            .await
            .unwrap()
        {
            Ok(count) => assert_eq!(count, 0),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
        }
    }
}

#[tokio::test]
async fn stream_connection_reuse_requires_a_valid_terminal_reply() {
    for (reply, reusable) in [
        (
            "{\"parameters\":{\"embedding\":[1.0],\"embedding_pipeline_id\":\"done\"}}\0",
            true,
        ),
        (
            "{\"error\":\"aileron.Inference.GenerationFailed\",\"parameters\":{\"reason\":\"failed\"}}\0",
            true,
        ),
        ("{\"parameters\":{\"unexpected\":1}}\0", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.socket");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let connection = zlink::tokio::unix::connect(&path).await.unwrap();
        let mut stream = connection
            .stream_embed(
                "session".into(),
                "text".into(),
                EmbedOptions {
                    execution_mode: "interactive".into(),
                },
            )
            .await
            .unwrap();
        let (peer, _) = listener.accept().await.unwrap();
        let mut peer = BufReader::new(peer);
        peer.read_until(0, &mut Vec::new()).await.unwrap();
        peer.get_mut().write_all(reply.as_bytes()).await.unwrap();
        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_none());
        assert_eq!(stream.into_connection().is_some(), reusable);
    }
}
