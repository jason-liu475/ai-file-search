use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, timeout_at};

pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const READ_BUFFER_BYTES: usize = 8 * 1024;

pub(crate) async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<String> {
    read_request_with_timeout(stream, IO_TIMEOUT).await
}

pub(crate) async fn write_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    response: &str,
) -> io::Result<()> {
    write_response_with_timeout(stream, response, IO_TIMEOUT).await
}

pub(crate) async fn send_request<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    request: &str,
) -> io::Result<String> {
    send_request_with_timeout(stream, request, IO_TIMEOUT).await
}

async fn read_request_with_timeout<S: AsyncRead + Unpin>(
    stream: &mut S,
    timeout: Duration,
) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    timeout_at(deadline, read_frame(stream, MAX_REQUEST_BYTES, deadline))
        .await
        .map_err(|_| timeout_error())?
}

async fn write_response_with_timeout<S: AsyncWrite + Unpin>(
    stream: &mut S,
    response: &str,
    timeout: Duration,
) -> io::Result<()> {
    timeout_at(Instant::now() + timeout, async {
        stream.write_all(response.as_bytes()).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| timeout_error())?
}

async fn send_request_with_timeout<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    request: &str,
    timeout: Duration,
) -> io::Result<String> {
    let deadline = Instant::now() + timeout;
    let needs_newline = !request.ends_with('\n');
    if request.len() > MAX_REQUEST_BYTES - usize::from(needs_newline) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "request exceeds frame size limit",
        ));
    }

    let response = timeout_at(deadline, async {
        stream.write_all(request.as_bytes()).await?;
        if needs_newline {
            stream.write_all(b"\n").await?;
        }
        stream.flush().await?;
        let response = read_frame(&mut stream, MAX_RESPONSE_BYTES, deadline).await?;
        if matches!(response.as_str(), "\n" | "\r\n") {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "empty response"));
        }
        Ok(response)
    })
    .await
    .map_err(|_| timeout_error())??;

    // Cleanup shares the transaction deadline and cannot invalidate a received frame.
    let _ = timeout_at(deadline, stream.shutdown()).await;
    Ok(response)
}

async fn read_frame<S: AsyncRead + Unpin>(
    stream: &mut S,
    max_bytes: usize,
    deadline: Instant,
) -> io::Result<String> {
    let mut frame = Vec::with_capacity(max_bytes.min(1024));
    let mut buffer = [0; READ_BUFFER_BYTES];
    loop {
        if Instant::now() >= deadline {
            return Err(timeout_error());
        }
        let read_limit = READ_BUFFER_BYTES.min(max_bytes - frame.len());
        let bytes_read = stream.read(&mut buffer[..read_limit]).await?;
        if Instant::now() >= deadline {
            return Err(timeout_error());
        }
        if bytes_read == 0 {
            let kind = if frame.is_empty() {
                io::ErrorKind::UnexpectedEof
            } else {
                io::ErrorKind::InvalidData
            };
            return Err(io::Error::new(kind, "frame ended before newline"));
        }
        let chunk = &buffer[..bytes_read];
        if let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            // Managed connections carry one frame; any prefetched tail is discarded.
            frame.extend_from_slice(&chunk[..=newline]);
            return String::from_utf8(frame)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
        frame.extend_from_slice(chunk);
        if frame.len() == max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame exceeds size limit including newline",
            ));
        }
    }
}

fn timeout_error() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "frame I/O deadline elapsed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf, duplex};
    use tokio::time::{sleep, timeout};

    const SHORT_TIMEOUT: Duration = Duration::from_millis(100);
    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    async fn read_bytes(bytes: &[u8]) -> io::Result<String> {
        let (mut reader, mut writer) = duplex(bytes.len().max(1));
        writer.write_all(bytes).await.unwrap();
        writer.shutdown().await.unwrap();
        read_request(&mut reader).await
    }

    async fn exchange(response: &[u8]) -> io::Result<String> {
        let (client, mut server) = duplex(response.len().max(16));
        let reply = async {
            let mut request = [0; 5];
            server.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"ping\n");
            server.write_all(response).await.unwrap();
            server.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::join!(send_request(client, "ping"), reply);
        result
    }

    #[tokio::test]
    async fn idle_request_times_out() {
        let (mut reader, _writer) = duplex(16);
        let error = timeout(
            TEST_TIMEOUT,
            read_request_with_timeout(&mut reader, SHORT_TIMEOUT),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn trickling_request_does_not_reset_deadline() {
        let (mut reader, mut writer) = duplex(32);
        let trickle = async {
            for _ in 0..8 {
                writer.write_all(b"a").await.unwrap();
                sleep(Duration::from_millis(25)).await;
            }
            writer.write_all(b"\n").await.unwrap();
        };
        let (result, ()) = timeout(TEST_TIMEOUT, async {
            tokio::join!(
                read_request_with_timeout(&mut reader, SHORT_TIMEOUT),
                trickle
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn oversized_request_is_rejected_without_waiting_for_newline_or_eof() {
        let (mut reader, mut writer) = duplex(MAX_REQUEST_BYTES + 1);
        writer
            .write_all(&vec![b'a'; MAX_REQUEST_BYTES + 1])
            .await
            .unwrap();
        let error = timeout(TEST_TIMEOUT, read_request(&mut reader))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn exactly_capped_request_is_accepted() {
        let mut bytes = vec![b'a'; MAX_REQUEST_BYTES];
        *bytes.last_mut().unwrap() = b'\n';
        assert_eq!(read_bytes(&bytes).await.unwrap().as_bytes(), bytes);
    }

    #[tokio::test]
    async fn partial_request_eof_is_invalid_data() {
        assert_eq!(
            read_bytes(b"partial").await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn clean_request_eof_is_unexpected_eof() {
        assert_eq!(
            read_bytes(b"").await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn invalid_utf8_request_is_rejected() {
        assert_eq!(
            read_bytes(b"\xff\n").await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn split_utf8_request_is_accepted() {
        let (mut reader, mut writer) = duplex(1);
        let write = async {
            for byte in b"\xe4\xb8\xad\n" {
                writer.write_all(&[*byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
        };
        let (result, ()) = tokio::join!(read_request(&mut reader), write);
        assert_eq!(result.unwrap().as_bytes(), b"\xe4\xb8\xad\n");
    }

    #[tokio::test]
    async fn request_returns_only_first_frame_and_ignores_invalid_trailing_bytes() {
        let mut bytes = b"first\nsecond\n".to_vec();
        bytes.extend(vec![0xff; MAX_REQUEST_BYTES + 1]);
        assert_eq!(read_bytes(&bytes).await.unwrap(), "first\n");
    }

    struct CountingReader {
        inner: tokio::io::DuplexStream,
        reads: usize,
    }

    impl AsyncRead for CountingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let result = Pin::new(&mut self.inner).poll_read(cx, buf);
            if result.is_ready() {
                self.reads += 1;
            }
            result
        }
    }

    #[tokio::test]
    async fn long_request_uses_block_reads() {
        let mut bytes = vec![b'a'; 32 * 1024];
        *bytes.last_mut().unwrap() = b'\n';
        let (inner, mut writer) = duplex(bytes.len());
        writer.write_all(&bytes).await.unwrap();
        let mut reader = CountingReader { inner, reads: 0 };
        assert_eq!(read_request(&mut reader).await.unwrap().as_bytes(), bytes);
        assert!(reader.reads <= 8, "too many read calls: {}", reader.reads);
    }

    #[tokio::test]
    async fn request_newline_at_chunk_boundaries_ends_first_frame() {
        for prefix_len in [
            READ_BUFFER_BYTES - 1,
            READ_BUFFER_BYTES,
            READ_BUFFER_BYTES + 1,
            2 * READ_BUFFER_BYTES - 1,
        ] {
            let mut expected = vec![b'a'; prefix_len];
            expected.push(b'\n');
            let mut bytes = expected.clone();
            bytes.extend_from_slice(b"ignored\xff\n");
            assert_eq!(read_bytes(&bytes).await.unwrap().as_bytes(), expected);
        }
    }

    #[tokio::test]
    async fn request_utf8_spanning_chunk_boundaries_is_accepted() {
        for prefix_len in [
            READ_BUFFER_BYTES - 2,
            READ_BUFFER_BYTES - 1,
            READ_BUFFER_BYTES,
        ] {
            let mut bytes = vec![b'a'; prefix_len];
            bytes.extend_from_slice(b"\xe4\xb8\xad\n");
            assert_eq!(read_bytes(&bytes).await.unwrap().as_bytes(), bytes);
        }
    }

    #[tokio::test]
    async fn stalled_response_writer_times_out() {
        let (mut writer, _reader) = duplex(1);
        let error = timeout(
            TEST_TIMEOUT,
            write_response_with_timeout(&mut writer, "reply\n", SHORT_TIMEOUT),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn disconnected_response_reader_returns_io_error() {
        let (mut writer, reader) = duplex(1);
        drop(reader);
        assert_eq!(
            write_response(&mut writer, "reply\n")
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn outgoing_response_is_not_limited_to_request_cap() {
        let bytes = "a".repeat(MAX_REQUEST_BYTES + 1);
        let (mut writer, mut reader) = duplex(128);
        let read = async {
            let mut output = vec![0; bytes.len()];
            reader.read_exact(&mut output).await.unwrap();
            output
        };
        let (result, output) = tokio::join!(write_response(&mut writer, &bytes), read);
        result.unwrap();
        assert_eq!(output, bytes.as_bytes());
    }

    #[tokio::test]
    async fn client_appends_only_missing_newline() {
        for request in ["ping", "ping\n"] {
            let (client, mut server) = duplex(32);
            let reply = async {
                let mut bytes = [0; 5];
                server.read_exact(&mut bytes).await.unwrap();
                assert_eq!(&bytes, b"ping\n");
                server.write_all(b"pong\n").await.unwrap();
                let mut trailing = Vec::new();
                server.read_to_end(&mut trailing).await.unwrap();
                assert!(trailing.is_empty());
            };
            let (result, ()) = timeout(TEST_TIMEOUT, async {
                tokio::join!(send_request(client, request), reply)
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap(), "pong\n");
        }
    }

    #[tokio::test]
    async fn client_rejects_oversized_outgoing_request_before_writing() {
        for request in [
            "a".repeat(MAX_REQUEST_BYTES),
            format!("{}\n", "a".repeat(MAX_REQUEST_BYTES)),
        ] {
            let (client, mut server) = duplex(16);
            assert_eq!(
                send_request(client, &request).await.unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            let mut bytes = Vec::new();
            server.read_to_end(&mut bytes).await.unwrap();
            assert!(bytes.is_empty());
        }
    }

    #[tokio::test]
    async fn client_accepts_exactly_capped_outgoing_request() {
        for terminated in [false, true] {
            let mut request = "a".repeat(MAX_REQUEST_BYTES - 1);
            if terminated {
                request.push('\n');
            }
            let (client, mut server) = duplex(MAX_REQUEST_BYTES);
            let reply = async {
                let mut bytes = vec![0; MAX_REQUEST_BYTES];
                server.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes.last(), Some(&b'\n'));
                server.write_all(b"ok\n").await.unwrap();
            };
            let (result, ()) = tokio::join!(send_request(client, &request), reply);
            assert_eq!(result.unwrap(), "ok\n");
        }
    }

    #[tokio::test]
    async fn client_accepts_response_larger_than_request_cap_up_to_response_cap() {
        let mut response = vec![b'a'; MAX_RESPONSE_BYTES];
        *response.last_mut().unwrap() = b'\n';
        assert_eq!(exchange(&response).await.unwrap().as_bytes(), response);
    }

    #[tokio::test]
    async fn client_rejects_oversized_response_without_waiting_for_termination() {
        let (client, mut server) = duplex(MAX_RESPONSE_BYTES + 1);
        let reply = tokio::spawn(async move {
            let mut request = [0; 5];
            server.read_exact(&mut request).await.unwrap();
            server
                .write_all(&vec![b'a'; MAX_RESPONSE_BYTES + 1])
                .await
                .unwrap();
            std::future::pending::<()>().await;
            drop(server);
        });
        let result = timeout(TEST_TIMEOUT, send_request(client, "ping")).await;
        reply.abort();
        let _ = reply.await;
        assert_eq!(
            result.unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn client_rejects_empty_response() {
        assert_eq!(
            exchange(b"").await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            exchange(b"\n").await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            exchange(b"\r\n").await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn client_rejects_partial_response_eof() {
        assert_eq!(
            exchange(b"partial").await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn client_validates_response_utf8() {
        assert_eq!(
            exchange(b"\xe4\xb8\xad\n").await.unwrap().as_bytes(),
            b"\xe4\xb8\xad\n"
        );
        assert_eq!(
            exchange(b"\xff\n").await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn client_returns_only_first_response_frame() {
        assert_eq!(exchange(b"first\nsecond\n").await.unwrap(), "first\n");
    }

    #[tokio::test]
    async fn idle_client_response_times_out() {
        let (client, _server) = duplex(16);
        let error = timeout(
            TEST_TIMEOUT,
            send_request_with_timeout(client, "ping", SHORT_TIMEOUT),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn trickling_client_response_does_not_reset_deadline() {
        let (client, mut server) = duplex(32);
        let reply = async {
            let mut request = [0; 5];
            server.read_exact(&mut request).await.unwrap();
            for _ in 0..8 {
                if server.write_all(b"a").await.is_err() {
                    return;
                }
                sleep(Duration::from_millis(25)).await;
            }
            let _ = server.write_all(b"\n").await;
        };
        let (result, ()) = timeout(TEST_TIMEOUT, async {
            tokio::join!(
                send_request_with_timeout(client, "ping", SHORT_TIMEOUT),
                reply
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn disconnected_client_peer_returns_io_error() {
        let (client, server) = duplex(16);
        drop(server);
        assert_eq!(
            send_request(client, "ping").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn stalled_client_request_writer_times_out() {
        let (client, _server) = duplex(1);
        let error = timeout(
            TEST_TIMEOUT,
            send_request_with_timeout(client, "ping", SHORT_TIMEOUT),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn client_write_and_read_share_one_deadline() {
        let (client, mut server) = duplex(1);
        let reply = async {
            sleep(Duration::from_millis(65)).await;
            let mut request = [0; 5];
            server.read_exact(&mut request).await.unwrap();
            sleep(Duration::from_millis(65)).await;
            let _ = server.write_all(b"ok\n").await;
        };
        let (result, ()) = timeout(TEST_TIMEOUT, async {
            tokio::join!(
                send_request_with_timeout(client, "ping", SHORT_TIMEOUT),
                reply
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    enum WriterBehavior {
        StalledFlush,
        StalledShutdown,
        FailedShutdown,
    }

    struct ControlledStream {
        inner: tokio::io::DuplexStream,
        behavior: WriterBehavior,
    }

    impl AsyncRead for ControlledStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for ControlledStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if matches!(self.behavior, WriterBehavior::StalledFlush) {
                Poll::Pending
            } else {
                Pin::new(&mut self.inner).poll_flush(cx)
            }
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.behavior {
                WriterBehavior::StalledShutdown => Poll::Pending,
                WriterBehavior::FailedShutdown => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "shutdown failed",
                ))),
                WriterBehavior::StalledFlush => Pin::new(&mut self.inner).poll_shutdown(cx),
            }
        }
    }

    #[tokio::test]
    async fn response_flush_is_included_in_timeout() {
        let (inner, _reader) = duplex(16);
        let mut writer = ControlledStream {
            inner,
            behavior: WriterBehavior::StalledFlush,
        };
        assert_eq!(
            timeout(
                TEST_TIMEOUT,
                write_response_with_timeout(&mut writer, "ok\n", SHORT_TIMEOUT)
            )
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[tokio::test]
    async fn client_flush_is_included_in_timeout() {
        let (inner, _server) = duplex(16);
        let client = ControlledStream {
            inner,
            behavior: WriterBehavior::StalledFlush,
        };
        assert_eq!(
            timeout(
                TEST_TIMEOUT,
                send_request_with_timeout(client, "ping", SHORT_TIMEOUT)
            )
            .await
            .unwrap()
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[tokio::test]
    async fn client_shutdown_is_bounded_and_best_effort() {
        for behavior in [
            WriterBehavior::StalledShutdown,
            WriterBehavior::FailedShutdown,
        ] {
            let (inner, mut server) = duplex(16);
            let client = ControlledStream { inner, behavior };
            let reply = async {
                let mut request = [0; 5];
                server.read_exact(&mut request).await.unwrap();
                server.write_all(b"ok\n").await.unwrap();
            };
            let (result, ()) = timeout(TEST_TIMEOUT, async {
                tokio::join!(
                    send_request_with_timeout(client, "ping", SHORT_TIMEOUT),
                    reply
                )
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap(), "ok\n");
        }
    }
}
