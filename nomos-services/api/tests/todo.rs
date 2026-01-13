#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    thread::sleep,
    time::Duration,
};

use axum::{Router, routing};
use nomos_api::{ApiService, ApiServiceSettings, Backend};
use overwatch::{
    DynError, derive_services,
    overwatch::{OverwatchRunner, handle::OverwatchHandle},
};
use utoipa::{
    Modify, OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};
use utoipa_swagger_ui::SwaggerUi;

use crate::todo::Store;

#[derive_services]
pub struct NomosApi {
    http: ApiService<WebServer, RuntimeServiceId>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        todo::list_todos,
        todo::search_todos,
        todo::create_todo,
        todo::mark_done,
        todo::delete_todo,
    ),
    components(
        schemas(todo::Todo, todo::TodoError)
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "todo", description = "Todo items management API")
    )
)]
struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "api_key",
                SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("todo_apikey"))),
            );
        }
    }
}

pub struct WebServer {
    addr: SocketAddr,
}

#[async_trait::async_trait]
impl Backend<RuntimeServiceId> for WebServer {
    type Error = std::io::Error;

    type Settings = SocketAddr;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Self { addr: settings })
    }

    async fn wait_until_ready(
        &mut self,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Result<(), DynError> {
        Ok(())
    }

    async fn serve(self, _handle: OverwatchHandle<RuntimeServiceId>) -> Result<(), Self::Error> {
        let store = Arc::new(Store::default());
        let app = Router::new()
            .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
            .route(
                "/todo",
                routing::get(todo::list_todos).post(todo::create_todo),
            )
            .route("/todo/search", routing::get(todo::search_todos))
            .route(
                "/todo/:id",
                routing::put(todo::mark_done).delete(todo::delete_todo),
            )
            .with_state(store);

        let listener = tokio::net::TcpListener::bind(&self.addr)
            .await
            .expect("Failed to bind address");

        axum::serve(listener, app).await
    }
}

#[test]
fn test_todo() {
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 8080);
    let settings = NomosApiServiceSettings {
        http: ApiServiceSettings {
            backend_settings: addr,
        },
    };
    let app = OverwatchRunner::<NomosApi>::run(settings, None).unwrap();
    drop(
        app.runtime()
            .handle()
            .block_on(app.handle().start_all_services()),
    );

    sleep(Duration::from_secs(1));
    let client = reqwest::blocking::Client::new();

    let response = client
        .get(format!("http://{addr}/swagger-ui"))
        .send()
        .unwrap();

    assert!(response.status().is_success());

    drop(app.runtime().handle().block_on(app.handle().shutdown()));
    app.blocking_wait_finished();
}

mod todo {
    #![expect(
        unused_qualifications,
        reason = "utoipa::path macro prepends some paths which triggers this rustc lint."
    )]

    use std::sync::{Arc, Mutex};

    use axum::{
        Json,
        extract::{Path, Query, State},
        response::IntoResponse,
    };
    use hyper::{HeaderMap, StatusCode};
    use serde::{Deserialize, Serialize};
    use utoipa::{IntoParams, ToSchema};

    /// In-memory todo store
    pub type Store = Mutex<Vec<Todo>>;

    /// Item to do.
    #[derive(Serialize, Deserialize, ToSchema, Clone)]
    pub struct Todo {
        id: i32,
        #[schema(example = "Buy groceries")]
        value: String,
        done: bool,
    }

    /// Todo operation errors
    #[derive(Serialize, Deserialize, ToSchema)]
    pub enum TodoError {
        /// Todo already exists conflict.
        #[schema(example = "Todo already exists")]
        Conflict(String),
        /// Todo not found by id.
        #[schema(example = "id = 1")]
        NotFound(String),
        /// Todo operation unauthorized
        #[schema(example = "missing api key")]
        Unauthorized(String),
    }

    /// List all Todo items
    ///
    /// List all Todo items from in-memory storage.
    #[utoipa::path(
      get,
      path = "/todo",
      responses(
          (status = 200, description = "List all todos successfully", body = [Todo])
      )
  )]
    pub async fn list_todos(State(store): State<Arc<Store>>) -> Json<Vec<Todo>> {
        let todos = store.lock().unwrap().clone();

        Json(todos)
    }

    /// Todo search query
    #[derive(Deserialize, IntoParams)]
    pub struct TodoSearchQuery {
        /// Search by value. Search is incase sensitive.
        value: String,
        /// Search by `done` status.
        done: bool,
    }

    /// Search Todos by query params.
    ///
    /// Search `Todo`s by query params and return matching `Todo`s.
    #[utoipa::path(
      get,
      path = "/todo/search",
      params(
        TodoSearchQuery
      ),
      responses(
          (status = 200, description = "List matching todos by query", body = [Todo])
      )
  )]
    pub async fn search_todos(
        State(store): State<Arc<Store>>,
        query: Query<TodoSearchQuery>,
    ) -> Json<Vec<Todo>> {
        Json(
            store
                .lock()
                .unwrap()
                .iter()
                .filter(|todo| {
                    todo.value.to_lowercase() == query.value.to_lowercase()
                        && todo.done == query.done
                })
                .cloned()
                .collect(),
        )
    }

    /// Create new Todo
    ///
    /// Tries to create a new Todo item to in-memory storage or fails with 409
    /// conflict if already exists.
    #[utoipa::path(
      post,
      path = "/todo",
      request_body = Todo,
      responses(
          (status = 201, description = "Todo item created successfully", body = Todo),
          (status = 409, description = "Todo already exists", body = TodoError)
      )
  )]
    pub async fn create_todo(
        State(store): State<Arc<Store>>,
        Json(todo): Json<Todo>,
    ) -> impl IntoResponse {
        let mut todos = store.lock().unwrap();

        todos
            .iter_mut()
            .find(|existing_todo| existing_todo.id == todo.id)
            .map(|found| {
                (
                    StatusCode::CONFLICT,
                    Json(TodoError::Conflict(format!(
                        "todo already exists: {}",
                        found.id
                    ))),
                )
                    .into_response()
            })
            .unwrap_or_else(|| {
                todos.push(todo.clone());

                (StatusCode::CREATED, Json(todo)).into_response()
            })
    }

    /// Mark Todo item done by id
    ///
    /// Mark Todo item done by given id. Return only status 200 on success or
    /// 404 if Todo is not found.
    #[utoipa::path(
      put,
      path = "/todo/{id}",
      responses(
          (status = 200, description = "Todo marked done successfully"),
          (status = 404, description = "Todo not found")
      ),
      params(
          ("id" = i32, Path, description = "Todo database id")
      ),
      security(
          (), // <-- make optional authentication
          ("api_key" = [])
      )
  )]
    pub async fn mark_done(
        Path(id): Path<i32>,
        State(store): State<Arc<Store>>,
        headers: HeaderMap,
    ) -> StatusCode {
        match check_api_key(false, &headers) {
            Ok(()) => (),
            Err(_) => return StatusCode::UNAUTHORIZED,
        }

        let mut todos = store.lock().unwrap();

        todos
            .iter_mut()
            .find(|todo| todo.id == id)
            .map_or(StatusCode::NOT_FOUND, |todo| {
                todo.done = true;
                StatusCode::OK
            })
    }

    /// Delete Todo item by id
    ///
    /// Delete Todo item from in-memory storage by id. Returns either 200
    /// success of 404 with `TodoError` if Todo is not found.
    #[utoipa::path(
      delete,
      path = "/todo/{id}",
      responses(
          (status = 200, description = "Todo marked done successfully"),
          (status = 401, description = "Unauthorized to delete Todo", body = TodoError, example = json!(TodoError::Unauthorized(String::from("missing api key")))),
          (status = 404, description = "Todo not found", body = TodoError, example = json!(TodoError::NotFound(String::from("id = 1"))))
      ),
      params(
          ("id" = i32, Path, description = "Todo database id")
      ),
      security(
          ("api_key" = [])
      )
  )]
    pub async fn delete_todo(
        Path(id): Path<i32>,
        State(store): State<Arc<Store>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        match check_api_key(true, &headers) {
            Ok(()) => (),
            Err(error) => return error.into_response(),
        }

        let mut todos = store.lock().unwrap();

        let len = todos.len();

        todos.retain(|todo| todo.id != id);

        if todos.len() == len {
            (
                StatusCode::NOT_FOUND,
                Json(TodoError::NotFound(format!("id = {id}"))),
            )
                .into_response()
        } else {
            StatusCode::OK.into_response()
        }
    }

    // normally you should create a middleware for this but this is sufficient for
    // sake of example.
    fn check_api_key(
        require_api_key: bool,
        headers: &HeaderMap,
    ) -> Result<(), (StatusCode, Json<TodoError>)> {
        match headers.get("todo_apikey") {
            Some(header) if header != "utoipa-rocks" => Err((
                StatusCode::UNAUTHORIZED,
                Json(TodoError::Unauthorized(String::from("incorrect api key"))),
            )),
            None if require_api_key => Err((
                StatusCode::UNAUTHORIZED,
                Json(TodoError::Unauthorized(String::from("missing api key"))),
            )),
            _ => Ok(()),
        }
    }
}
