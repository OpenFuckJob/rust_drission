//! CDP DOM 与 Runtime 调用封装

use crate::cdp::{CdpClient, CdpError};
use serde_json::{json, Value};

fn is_transient_node_error(e: &CdpError) -> bool {
    match e {
        CdpError::Protocol { message, .. } => {
            message.contains("No node with given id")
                || message.contains("Could not find node with given id")
                || message.contains("Could not find object with given id")
                || message.contains("Object has been collected")
        }
        _ => false,
    }
}

/// 启用 DOM 并返回文档根 nodeId
pub fn get_document_root(client: &CdpClient, session_id: &str) -> Result<i64, CdpError> {
    client.send_with_session("DOM.enable", None, Some(session_id))?;
    let result = client.send_with_session("DOM.getDocument", None, Some(session_id))?;
    let root = result
        .get("root")
        .and_then(|v| v.get("nodeId"))
        .and_then(Value::as_i64)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.getDocument did not return root.nodeId".into(),
        })?;
    Ok(root)
}

/// 在指定节点下用 CSS 选择器查单个节点，返回 nodeId
pub fn query_selector(
    client: &CdpClient,
    session_id: &str,
    node_id: i64,
    selector: &str,
) -> Result<Option<i64>, CdpError> {
    let params = json!({ "nodeId": node_id, "selector": selector });
    let result = client.send_with_session("DOM.querySelector", Some(params), Some(session_id))?;
    let node_id = result.get("nodeId").and_then(Value::as_i64);
    Ok(node_id.filter(|&id| id != 0))
}

/// 在指定节点下用 CSS 选择器查多个节点，返回 nodeId 列表（通过 evaluate + requestNode）
pub fn query_selector_all(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<Vec<i64>, CdpError> {
    // 在页面中执行 document.querySelectorAll(selector)，返回 NodeList 的 objectId
    let escaped = serde_json::to_string(selector).map_err(CdpError::Json)?;
    let expr = format!("document.querySelectorAll({})", escaped);
    let params = json!({
        "expression": expr,
        "returnByValue": false
    });
    let result = client.send_with_session("Runtime.evaluate", Some(params), Some(session_id))?;
    let obj = result.get("result").ok_or_else(|| CdpError::Protocol {
        id: None,
        code: -1,
        message: "Runtime.evaluate did not return a result payload".into(),
    })?;
    let object_id = obj.get("objectId").and_then(Value::as_str);
    let Some(object_id) = object_id else {
        return Ok(Vec::new());
    };
    // Array.from(this) 得到元素数组
    let params = json!({
        "functionDeclaration": "function(){ return Array.from(this); }",
        "objectId": object_id
    });
    let result =
        client.send_with_session("Runtime.callFunctionOn", Some(params), Some(session_id))?;
    let result_val = result.get("result").ok_or_else(|| CdpError::Protocol {
        id: None,
        code: -1,
        message: "Runtime.callFunctionOn did not return a result payload".into(),
    })?;
    let list_obj_id = result_val.get("objectId").and_then(Value::as_str);
    let Some(list_obj_id) = list_obj_id else {
        return Ok(Vec::new());
    };
    // 取 length
    let params = json!({
        "functionDeclaration": "function(){ return this.length; }",
        "objectId": list_obj_id
    });
    let result =
        client.send_with_session("Runtime.callFunctionOn", Some(params), Some(session_id))?;
    let len = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let mut node_ids = Vec::with_capacity(len);
    for i in 0..len {
        let params = json!({
            "functionDeclaration": "function(i){ return this[i]; }",
            "objectId": list_obj_id,
            "arguments": [{"value": i}]
        });
        let result = match client.send_with_session(
            "Runtime.callFunctionOn",
            Some(params),
            Some(session_id),
        ) {
            Ok(v) => v,
            Err(e) if is_transient_node_error(&e) => continue,
            Err(e) => return Err(e),
        };
        let elem = result
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(Value::as_str);
        if let Some(oid) = elem {
            let params = json!({ "objectId": oid });
            let res =
                match client.send_with_session("DOM.requestNode", Some(params), Some(session_id)) {
                    Ok(v) => v,
                    Err(e) if is_transient_node_error(&e) => continue,
                    Err(e) => return Err(e),
                };
            if let Some(nid) = res.get("nodeId").and_then(Value::as_i64) {
                node_ids.push(nid);
            }
        }
    }
    Ok(node_ids)
}

/// 获取 iframe/frame 元素的 contentDocument 的 nodeId（仅同源可用，跨域返回 None）。
pub fn get_iframe_content_document_node_id(
    client: &CdpClient,
    session_id: &str,
    iframe_node_id: i64,
) -> Result<Option<i64>, CdpError> {
    let object_id = resolve_node_to_object_id(client, session_id, iframe_node_id)?;
    let params = json!({
        "functionDeclaration": "function(){ return this.contentDocument; }",
        "objectId": object_id
    });
    let result =
        client.send_with_session("Runtime.callFunctionOn", Some(params), Some(session_id))?;
    let result_val = result.get("result").ok_or_else(|| CdpError::Protocol {
        id: None,
        code: -1,
        message: "Runtime.callFunctionOn did not return a result payload".into(),
    })?;
    // 跨域 iframe 时 contentDocument 为 null，subtype 为 "null"
    let obj_id = result_val.get("objectId").and_then(Value::as_str);
    let Some(obj_id) = obj_id else {
        return Ok(None);
    };
    let params = json!({ "objectId": obj_id });
    let res = client.send_with_session("DOM.requestNode", Some(params), Some(session_id))?;
    let node_id = res
        .get("nodeId")
        .and_then(Value::as_i64)
        .filter(|&id| id != 0);
    Ok(node_id)
}

/// 在指定文档根节点下执行 querySelectorAll，返回 nodeId 列表（用于 iframe 内查找）。
pub fn query_selector_all_under_root(
    client: &CdpClient,
    session_id: &str,
    root_node_id: i64,
    selector: &str,
) -> Result<Vec<i64>, CdpError> {
    let object_id = resolve_node_to_object_id(client, session_id, root_node_id)?;
    let params = json!({
        "functionDeclaration": format!("function(sel){{ return Array.from(this.querySelectorAll(sel)); }}"),
        "objectId": object_id,
        "arguments": [{"value": selector}]
    });
    let result =
        client.send_with_session("Runtime.callFunctionOn", Some(params), Some(session_id))?;
    let result_val = result.get("result").ok_or_else(|| CdpError::Protocol {
        id: None,
        code: -1,
        message: "Runtime.callFunctionOn did not return a result payload".into(),
    })?;
    let list_obj_id = result_val.get("objectId").and_then(Value::as_str);
    let Some(list_obj_id) = list_obj_id else {
        return Ok(Vec::new());
    };
    let params = json!({
        "functionDeclaration": "function(){ return this.length; }",
        "objectId": list_obj_id
    });
    let result =
        client.send_with_session("Runtime.callFunctionOn", Some(params), Some(session_id))?;
    let len = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let mut node_ids = Vec::with_capacity(len);
    for i in 0..len {
        let params = json!({
            "functionDeclaration": "function(i){ return this[i]; }",
            "objectId": list_obj_id,
            "arguments": [{"value": i}]
        });
        let result = match client.send_with_session(
            "Runtime.callFunctionOn",
            Some(params),
            Some(session_id),
        ) {
            Ok(v) => v,
            Err(e) if is_transient_node_error(&e) => continue,
            Err(e) => return Err(e),
        };
        let elem = result
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(Value::as_str);
        if let Some(oid) = elem {
            let params = json!({ "objectId": oid });
            let res =
                match client.send_with_session("DOM.requestNode", Some(params), Some(session_id)) {
                    Ok(v) => v,
                    Err(e) if is_transient_node_error(&e) => continue,
                    Err(e) => return Err(e),
                };
            if let Some(nid) = res.get("nodeId").and_then(Value::as_i64) {
                node_ids.push(nid);
            }
        }
    }
    Ok(node_ids)
}

/// 在主文档及所有同源 iframe 的 document 中执行 querySelectorAll，返回 (nodeId, objectId) 列表。
/// objectId 为取 nodeId 时持有的引用，iframe 内节点优先用其做 callFunctionOn，避免 nodeId 失效。
pub fn query_selector_all_including_same_origin_frames(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<Vec<(i64, String)>, CdpError> {
    client.send_with_session("DOM.enable", None, Some(session_id))?;
    let escaped = serde_json::to_string(selector).map_err(CdpError::Json)?;
    // 递归收集 document + 所有同源 iframe 的 contentDocument 中匹配元素
    let expr = format!(
        r#"(function(selector){{
            var results = [];
            function collect(root){{
                if(!root || !root.querySelectorAll) return;
                try{{
                    var list = root.querySelectorAll(selector);
                    for(var i=0;i<list.length;i++) results.push(list[i]);
                    var frames = root.querySelectorAll('iframe,frame');
                    for(var j=0;j<frames.length;j++){{
                        try{{
                            var doc = frames[j].contentDocument;
                            if(doc) collect(doc);
                        }}catch(e){{}}
                    }}
                }}catch(e){{}}
            }}
            collect(document);
            return results;
        }})({})"#,
        escaped
    );
    let params = json!({
        "expression": expr,
        "returnByValue": false
    });
    let result = client.send_with_session("Runtime.evaluate", Some(params), Some(session_id))?;
    let obj = result.get("result").ok_or_else(|| CdpError::Protocol {
        id: None,
        code: -1,
        message: "Runtime.evaluate did not return a result payload".into(),
    })?;
    let list_obj_id = obj.get("objectId").and_then(Value::as_str);
    let Some(list_obj_id) = list_obj_id else {
        return Ok(Vec::new());
    };
    let params = json!({
        "functionDeclaration": "function(){ return this.length; }",
        "objectId": list_obj_id
    });
    let result =
        client.send_with_session("Runtime.callFunctionOn", Some(params), Some(session_id))?;
    let len = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let mut pairs = Vec::with_capacity(len);
    for i in 0..len {
        let params = json!({
            "functionDeclaration": "function(i){ return this[i]; }",
            "objectId": list_obj_id,
            "arguments": [{"value": i}]
        });
        let result = match client.send_with_session(
            "Runtime.callFunctionOn",
            Some(params),
            Some(session_id),
        ) {
            Ok(v) => v,
            Err(e) if is_transient_node_error(&e) => continue,
            Err(e) => return Err(e),
        };
        let elem = result
            .get("result")
            .and_then(|r| r.get("objectId"))
            .and_then(Value::as_str);
        if let Some(oid) = elem {
            let params = json!({ "objectId": oid });
            let res =
                match client.send_with_session("DOM.requestNode", Some(params), Some(session_id)) {
                    Ok(v) => v,
                    Err(e) if is_transient_node_error(&e) => continue,
                    Err(e) => return Err(e),
                };
            if let Some(nid) = res.get("nodeId").and_then(Value::as_i64) {
                pairs.push((nid, oid.to_string()));
            }
        }
    }
    Ok(pairs)
}

/// 获取节点 HTML
pub fn get_outer_html(
    client: &CdpClient,
    session_id: &str,
    node_id: i64,
) -> Result<String, CdpError> {
    let params = json!({ "nodeId": node_id });
    let result = client.send_with_session("DOM.getOuterHTML", Some(params), Some(session_id))?;
    result
        .get("outerHTML")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.getOuterHTML did not return outerHTML".into(),
        })
}

/// 在整棵 DOM 树（含 iframe、shadow DOM）中搜索，返回 (searchId, resultCount)。
/// query 可为 CSS 选择器或 XPath 表达式（与 DevTools 元素搜索一致）。
pub fn perform_search(
    client: &CdpClient,
    session_id: &str,
    query: &str,
    include_user_agent_shadow_dom: bool,
) -> Result<(String, u32), CdpError> {
    client.send_with_session("DOM.enable", None, Some(session_id))?;
    let params = json!({
        "query": query,
        "includeUserAgentShadowDOM": include_user_agent_shadow_dom
    });
    let result = client.send_with_session("DOM.performSearch", Some(params), Some(session_id))?;
    let search_id = result
        .get("searchId")
        .and_then(Value::as_str)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.performSearch did not return searchId".into(),
        })?
        .to_string();
    let result_count = result
        .get("resultCount")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    Ok((search_id, result_count))
}

/// 从 performSearch 的会话中取回 nodeId 列表
pub fn get_search_results(
    client: &CdpClient,
    session_id: &str,
    search_id: &str,
    from_index: u32,
    to_index: u32,
) -> Result<Vec<i64>, CdpError> {
    if from_index >= to_index {
        return Ok(Vec::new());
    }
    let params = json!({
        "searchId": search_id,
        "fromIndex": from_index,
        "toIndex": to_index
    });
    let result =
        client.send_with_session("DOM.getSearchResults", Some(params), Some(session_id))?;
    let node_ids = result
        .get("nodeIds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_i64())
                .filter(|&id| id != 0)
                .collect()
        })
        .unwrap_or_default();
    Ok(node_ids)
}

/// 丢弃搜索会话，释放资源
pub fn discard_search_results(
    client: &CdpClient,
    session_id: &str,
    search_id: &str,
) -> Result<(), CdpError> {
    let params = json!({ "searchId": search_id });
    client.send_with_session("DOM.discardSearchResults", Some(params), Some(session_id))?;
    Ok(())
}

/// nodeId -> objectId（用于 callFunctionOn）
pub fn resolve_node_to_object_id(
    client: &CdpClient,
    session_id: &str,
    node_id: i64,
) -> Result<String, CdpError> {
    let params = json!({ "nodeId": node_id });
    let result = client.send_with_session("DOM.resolveNode", Some(params), Some(session_id))?;
    result
        .get("object")
        .and_then(|o| o.get("objectId"))
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.resolveNode did not return object.objectId".into(),
        })
}

/// 由 nodeId 取 backendNodeId（与 DrissionPage 一致，用于元素稳定引用，nodeId 失效时可用 backend 刷新）
pub fn get_backend_node_id(
    client: &CdpClient,
    session_id: &str,
    node_id: i64,
) -> Result<i64, CdpError> {
    client.send_with_session("DOM.enable", None, Some(session_id))?;
    let params = json!({ "nodeId": node_id });
    let result = client.send_with_session("DOM.describeNode", Some(params), Some(session_id))?;
    result
        .get("node")
        .and_then(|n| n.get("backendNodeId"))
        .and_then(Value::as_i64)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.describeNode did not return node.backendNodeId".into(),
        })
}

/// 由 backendNodeId 直接取 objectId（与 Python _get_obj_id(backend_id) 一致，nodeId 失效时用）
pub fn resolve_backend_to_object_id(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Result<String, CdpError> {
    let params = json!({ "backendNodeId": backend_node_id });
    let result = client.send_with_session("DOM.resolveNode", Some(params), Some(session_id))?;
    result
        .get("object")
        .and_then(|o| o.get("objectId"))
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.resolveNode(backendNodeId) did not return object.objectId".into(),
        })
}

/// 由 backendNodeId 取当前 nodeId（DOM 更新后 nodeId 会变，用 backend 可重新解析）
pub fn get_node_id_from_backend(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Result<i64, CdpError> {
    client.send_with_session("DOM.enable", None, Some(session_id))?;
    let params = json!({ "backendNodeId": backend_node_id });
    let result = client.send_with_session("DOM.describeNode", Some(params), Some(session_id))?;
    result
        .get("node")
        .and_then(|n| n.get("nodeId"))
        .and_then(Value::as_i64)
        .ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: "DOM.describeNode did not return node.nodeId".into(),
        })
}
