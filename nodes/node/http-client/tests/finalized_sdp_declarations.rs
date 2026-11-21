use lb_core::sdp::{DeclarationId, ServiceType};
use logos_blockchain_common_http_client::CommonHttpClient;
use reqwest::Url;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::mpsc,
};

/// Starts a server that answers every request with `404 Not Found`, which the
/// client reads as "not in finalized state", and reports the path of each
/// request it gets.
async fn not_found_server() -> (Url, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (paths, received_paths) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut buffer = [0; 1024];
            while !head.ends_with(b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                head.extend_from_slice(&buffer[..read]);
            }
            let head = String::from_utf8(head).unwrap();
            let path = head.split(' ').nth(1).unwrap().to_owned();
            paths.send(path).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    (
        Url::parse(&format!("http://{address}/")).unwrap(),
        received_paths,
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn a_finalized_declaration_is_requested_by_its_id() {
    runtime().block_on(async {
        let (base_url, mut received_paths) = not_found_server().await;
        let declaration_id = DeclarationId([0xab; 32]);

        let declaration = CommonHttpClient::new(None)
            .get_finalized_sdp_declaration(base_url, declaration_id)
            .await
            .unwrap();

        assert_eq!(declaration, None);
        assert_eq!(
            received_paths.recv().await.unwrap(),
            format!("/mantle/sdp/finalized/declaration/{declaration_id}")
        );
    });
}

#[test]
fn finalized_declarations_are_requested_by_service_type() {
    runtime().block_on(async {
        let (base_url, mut received_paths) = not_found_server().await;

        let declarations = CommonHttpClient::new(None)
            .get_finalized_sdp_declarations(base_url, ServiceType::BlendNetwork)
            .await
            .unwrap();

        assert_eq!(declarations, None);
        assert_eq!(
            received_paths.recv().await.unwrap(),
            format!(
                "/mantle/sdp/finalized/declarations/{}",
                u8::from(ServiceType::BlendNetwork)
            )
        );
    });
}
