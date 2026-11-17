//! The `http` crate's types, to and from haest's: for a host whose own stack
//! speaks them (an axum router, a hyper client). Behind the `http` feature.

use std::{
    pin::Pin,
    task::{Poll, ready},
};

use bytes::{Buf, Bytes};
use http_body::Body as HttpBody;

use crate::{Body, BoxError, HttpError, Method, Request, Response};

impl From<Method> for http::Method {
    fn from(method: Method) -> Self {
        match method {
            Method::Get => http::Method::GET,
            Method::Post => http::Method::POST,
            Method::Patch => http::Method::PATCH,
            Method::Delete => http::Method::DELETE,
        }
    }
}

/// The request as the `http` crate builds it: the method, the URL as its
/// URI, every header in order (one sent twice stays twice), the body.
///
/// # Errors
///
/// [`HttpError::Request`] when the URL is not a URI, or a header's name or
/// value cannot go on the wire.
impl TryFrom<Request> for http::Request<Bytes> {
    type Error = HttpError;

    fn try_from(req: Request) -> Result<Self, HttpError> {
        let Request {
            method,
            url,
            headers,
            body,
        } = req;
        let mut out = http::Request::builder()
            .method(http::Method::from(method))
            .uri(url);
        for (name, value) in headers {
            out = out.header(name, value);
        }
        out.body(body).map_err(|e| HttpError::Request(e.into()))
    }
}

/// The answer as haest reads it: the status, every header (a value that is
/// not UTF-8 read lossily, as [`ReqwestClient`](crate::ReqwestClient) reads
/// one), and the body's data frames as they arrive. Trailers are skipped; a
/// body error is an [`HttpError::Body`] chunk, which ends the body.
impl<B> From<http::Response<B>> for Response
where
    B: HttpBody + Send + 'static,
    B::Error: Into<BoxError>,
{
    fn from(res: http::Response<B>) -> Self {
        let (parts, body) = res.into_parts();
        let headers = parts
            .headers
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_owned(), value)
            })
            .collect();
        Response {
            status: parts.status.as_u16(),
            headers,
            body: data_frames(body),
        }
    }
}

/// `body`'s data frames, chunk by chunk.
fn data_frames<B>(body: B) -> Body
where
    B: HttpBody + Send + 'static,
    B::Error: Into<BoxError>,
{
    let mut body: Pin<Box<B>> = Box::pin(body);
    Body::from_stream(futures::stream::poll_fn(move |cx| {
        loop {
            let frame = match ready!(body.as_mut().poll_frame(cx)) {
                None => return Poll::Ready(None),
                Some(Err(e)) => return Poll::Ready(Some(Err(HttpError::Body(e.into())))),
                Some(Ok(frame)) => frame,
            };
            if let Ok(mut data) = frame.into_data() {
                return Poll::Ready(Some(Ok(data.copy_to_bytes(data.remaining()))));
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, task::Context};

    use futures::StreamExt;
    use http_body::Frame;

    use super::*;

    #[test]
    fn a_request_goes_over_as_it_was_built() {
        let req = Request::post("https://api.example.com/v1/x?y=1")
            .with_header("authorization", "Bearer k")
            .with_header("x-team", "a");
        let req = Request {
            body: Bytes::from_static(b"{}"),
            ..req
        };
        let out: http::Request<Bytes> = req.try_into().unwrap();
        assert_eq!(out.method(), http::Method::POST);
        assert_eq!(out.uri(), "https://api.example.com/v1/x?y=1");
        assert_eq!(out.headers()["authorization"], "Bearer k");
        assert_eq!(out.headers()["x-team"], "a");
        assert_eq!(out.body(), &Bytes::from_static(b"{}"));
        for (method, word) in [
            (Method::Get, "GET"),
            (Method::Post, "POST"),
            (Method::Patch, "PATCH"),
            (Method::Delete, "DELETE"),
        ] {
            assert_eq!(http::Method::from(method).as_str(), word);
        }
    }

    #[test]
    fn a_request_the_wire_cannot_carry_is_a_request_error() {
        let bad_name = Request::get("https://x/").with_header("bad name", "v");
        let bad_url = Request::get("not a uri at all");
        for req in [bad_name, bad_url] {
            let err = http::Request::<Bytes>::try_from(req).unwrap_err();
            assert!(matches!(err, HttpError::Request(_)), "{err}");
        }
    }

    /// Frames handed out one per poll: data, trailers, then an error or the end.
    struct Frames(VecDeque<Result<Frame<Bytes>, &'static str>>);

    impl HttpBody for Frames {
        type Data = Bytes;
        type Error = &'static str;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(self.0.pop_front())
        }
    }

    #[tokio::test]
    async fn a_response_streams_its_data_frames_and_skips_trailers() {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("x-checksum", "1".parse().unwrap());
        let body = Frames(VecDeque::from([
            Ok(Frame::data(Bytes::from("hel"))),
            Ok(Frame::trailers(trailers)),
            Ok(Frame::data(Bytes::from("lo"))),
        ]));
        let res = http::Response::builder()
            .status(201)
            .header("content-type", "text/plain")
            .header("x-raw", http::HeaderValue::from_bytes(b"caf\xe9").unwrap())
            .body(body)
            .unwrap();
        let res = Response::from(res);
        assert_eq!(res.status, 201);
        assert_eq!(res.header("content-type"), Some("text/plain"));
        assert_eq!(res.header("x-raw"), Some("caf\u{fffd}"));
        assert_eq!(res.body.bytes(64).await.unwrap(), Bytes::from("hello"));

        // Any body the `http` crate's world has: a String's error is Infallible.
        let whole: Response = http::Response::new(String::from("all at once")).into();
        assert_eq!(whole.body.bytes(64).await.unwrap(), "all at once");
    }

    #[tokio::test]
    async fn a_body_that_breaks_off_ends_with_a_body_error() {
        let body = Frames(VecDeque::from([
            Ok(Frame::data(Bytes::from("half"))),
            Err("reset"),
        ]));
        let mut chunks = Response::from(http::Response::new(body)).body;
        assert_eq!(chunks.next().await.unwrap().unwrap(), "half");
        let err = chunks.next().await.unwrap().unwrap_err();
        assert!(matches!(err, HttpError::Body(_)), "{err}");
    }
}
