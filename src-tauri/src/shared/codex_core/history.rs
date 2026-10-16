use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;

pub(super) fn rpc_result(response: &Value) -> Result<&Value, String> {
    if let Some(error) = response.get("error") {
        return Err(error.get("message").and_then(Value::as_str)
            .unwrap_or("Codex app-server request failed").to_string());
    }
    response.get("result").filter(|result| result.is_object())
        .ok_or_else(|| "Codex app-server returned an invalid response".to_string())
}

// Preserve the existing app/daemon IPC contract while reading both legacy and
// paginated histories through the current protocol. Never return a partial
// snapshot as complete when a page fails.
pub(super) async fn load_turns<F, Fut>(thread_id: &str, mut request: F) -> Result<Vec<Value>, String>
where
    F: FnMut(Value) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let mut turns = Vec::new();
    let mut indices = HashMap::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor = None::<String>;
    loop {
        let response = request(json!({
            "threadId": thread_id, "cursor": cursor, "limit": 50,
            "sortDirection": "asc", "itemsView": "full"
        })).await?;
        let result = rpc_result(&response)?;
        let page = result.get("data").and_then(Value::as_array)
            .ok_or("Codex history page is missing data")?;
        for turn in page {
            if turn.get("itemsView").and_then(Value::as_str).is_some_and(|view| view != "full") {
                return Err("Codex returned incomplete turn items for a full history request".to_string());
            }
            let id = turn.get("id").and_then(Value::as_str).ok_or("Codex history turn is missing its id")?;
            if let Some(index) = indices.get(id).copied() {
                turns[index] = turn.clone();
            } else {
                indices.insert(id.to_string(), turns.len());
                turns.push(turn.clone());
            }
        }
        cursor = result.get("nextCursor").and_then(Value::as_str).map(str::to_string);
        let Some(next) = cursor.as_ref() else { break };
        if !seen_cursors.insert(next.clone()) {
            return Err("Codex history pagination repeated a cursor".to_string());
        }
    }
    Ok(turns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn loads_all_pages_in_order_and_reconciles_duplicate_anchors() {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
        let mut pages = VecDeque::from([
            json!({"result":{"data":[{"id":"a","items":[]},{"id":"b","items":[]}],"nextCursor":"next"}}),
            json!({"result":{"data":[{"id":"b","items":[{"id":"updated"}]},{"id":"c","items":[]}],"nextCursor":null}}),
        ]);
        let mut calls = Vec::new();
        let turns = load_turns("t", |params| {
            calls.push(params);
            std::future::ready(Ok(pages.pop_front().unwrap()))
        }).await.unwrap();
        assert_eq!(turns.iter().map(|t| t["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(turns[1]["items"][0]["id"], "updated");
        assert_eq!(calls[1]["cursor"], "next");
        assert_eq!(calls[0]["itemsView"], "full");
        assert_eq!(calls[0]["sortDirection"], "asc");
        });
    }

    #[test]
    fn failed_or_incomplete_pages_are_not_empty_successes() {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
        for page in [json!({"error":{"message":"history unavailable"}}), json!({"result":{}}),
            json!({"result":{"data":[{"id":"a","itemsView":"summary","items":[]}]}})] {
            assert!(load_turns("t", |_| std::future::ready(Ok(page.clone()))).await.is_err());
        }
        });
    }

    #[test]
    fn repeated_cursor_is_an_error() {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
        let page = json!({"result":{"data":[],"nextCursor":"same"}});
        assert!(load_turns("t", |_| std::future::ready(Ok(page.clone()))).await.unwrap_err().contains("repeated"));
        });
    }
}
