use ai_file_search_protocol::{Request, Response};
use serde_json::json;

#[test]
fn parses_json_rpc_request() {
    let request = Request::from_json_line(
        r#"{"id":7,"method":"search","params":{"query":"report","limit":20}}"#,
    )
    .expect("request should parse");

    assert_eq!(request.id, 7);
    assert_eq!(request.method, "search");
    assert_eq!(request.params["query"], "report");
    assert_eq!(request.params["limit"], 20);
}

#[test]
fn formats_success_response_as_single_json_line() {
    let response = Response::success(7, json!({"files":[]}));

    assert_eq!(
        response.to_json_line(),
        "{\"id\":7,\"result\":{\"files\":[]}}\n"
    );
}

#[test]
fn formats_error_response_as_single_json_line() {
    let response = Response::error(7, "unknown method");

    assert_eq!(
        response.to_json_line(),
        "{\"id\":7,\"error\":{\"message\":\"unknown method\"}}\n"
    );
}
