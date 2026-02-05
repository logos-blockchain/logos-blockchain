use reqwest::{Client, Response};
use serde::{Serialize, de::DeserializeOwned};

async fn deserialize_response<Config: DeserializeOwned>(
    response: Response,
) -> Result<Config, String> {
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read response body: {error}"))?;
    let mut json_deserializer = serde_json::Deserializer::from_str(&body);
    serde_path_to_error::deserialize(&mut json_deserializer)
        .map_err(|error| format!("Failed to deserialize body: {error}, raw body: {body}"))
}

pub async fn get_config<Config, Payload>(payload: &Payload, url: &str) -> Result<Config, String>
where
    Config: DeserializeOwned,
    Payload: Serialize + Sync,
{
    let client = Client::new();

    let response = client
        .post(url)
        .json(payload)
        .send()
        .await
        .map_err(|err| format!("Failed to send IP announcement: {err}"))?;

    if !response.status().is_success() {
        return Err(format!("Server error: {:?}", response.status()));
    }

    deserialize_response(response).await
}
