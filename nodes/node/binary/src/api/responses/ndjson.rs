use axum::{
    body::Body,
    response::{IntoResponse as _, Response},
};
use futures::{Stream, StreamExt as _};
use http::StatusCode;
use lb_api_service::http::DynError;
use serde::Serialize;

pub fn from_stream<T>(stream: impl Stream<Item = T> + Send + 'static) -> Response
where
    T: Serialize,
{
    let stream = stream.map(|item| {
        let mut bytes = serde_json::to_vec(&item).map_err(|error| Box::new(error) as DynError)?;
        bytes.push(b'\n');
        Ok::<_, DynError>(bytes)
    });

    let stream_body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/x-ndjson")
        .body(stream_body)
        .unwrap()
        .into_response()
}

pub fn from_stream_result<T>(
    stream: impl Stream<Item = Result<T, DynError>> + Send + 'static,
) -> Response
where
    T: Serialize + Send + 'static,
{
    let stream = stream.filter_map(|item| futures::future::ready(item.ok()));
    from_stream(stream)
}

#[cfg(test)]
mod tests {
    use axum::body;
    use futures::stream;
    use serde::Serialize;

    use super::*;

    #[derive(Serialize)]
    struct TestData {
        value: i32,
    }

    #[tokio::test]
    async fn test_from_stream_result_ok() {
        let test_data = vec![TestData { value: 1 }, TestData { value: 2 }];
        let stream = stream::iter(test_data.into_iter().map(Ok::<_, DynError>));
        let response = from_stream_result(stream);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/x-ndjson"
        );

        let body_bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            body_bytes.iter().as_slice(),
            b"{\"value\":1}\n{\"value\":2}\n"
        );
    }

    #[tokio::test]
    async fn test_error_stream_result_err() {
        let error = "Test error";
        let error: DynError = std::io::Error::other(error).into();
        let stream = stream::iter(vec![Err::<TestData, DynError>(error)]);
        let response = from_stream_result(stream);

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body_bytes.iter().as_slice(), b"");
    }
}
