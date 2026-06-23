use std::path::Path;

use ai_file_search_indexer::FileIndexStore;
use ai_file_search_protocol::{Request, Response};
use serde_json::json;

#[must_use]
pub fn handle_json_line(index_path: &Path, line: &str) -> Response {
    let request = match Request::from_json_line(line) {
        Ok(request) => request,
        Err(error) => return Response::error(0, format!("invalid request: {error}")),
    };

    match request.method.as_str() {
        "stats" => stats(index_path, request.id),
        "search" => search(index_path, &request),
        method => Response::error(request.id, format!("unknown method: {method}")),
    }
}

fn stats(index_path: &Path, id: u64) -> Response {
    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(id, format!("index open failed: {error}")),
    };

    Response::success(
        id,
        json!({
            "files": store.file_count(),
            "total_bytes": store.total_size_bytes(),
        }),
    )
}

fn search(index_path: &Path, request: &Request) -> Response {
    let Some(query) = request.params.get("query").and_then(|query| query.as_str()) else {
        return Response::error(request.id, "missing string param: query");
    };
    let limit = request
        .params
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(20);

    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(request.id, format!("index open failed: {error}")),
    };
    let files = store
        .search_by_name(query)
        .into_iter()
        .take(limit)
        .map(|file| {
            json!({
                "path": file.relative_path.as_normalized(),
                "size_bytes": file.size_bytes,
                "modified_unix_seconds": file.modified_unix_seconds,
            })
        })
        .collect::<Vec<_>>();

    Response::success(request.id, json!({ "files": files }))
}
