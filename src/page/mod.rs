//! 页面（Tab）API：导航、DOM、元素、等待等

mod async_page;

use crate::cdp::{CdpClient, CdpError};
use crate::dom::{
    discard_search_results, get_backend_node_id, get_document_root, get_search_results,
    perform_search, query_selector, query_selector_all_including_same_origin_frames,
};
use crate::element::Element;
use crate::frame::Frame;
use crate::listener::Listener;
use crate::locator::Locator;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub use async_page::{AsyncCdpExecutor, AsyncPage};

/// Cookie 表示（README §19）
#[derive(Debug, Clone)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
}

/// 单个 Tab 页面，对应 CDP 的一个 target session
#[derive(Clone)]
pub struct Page {
    pub(crate) client: Arc<CdpClient>,
    pub(crate) session_id: String,
    pub(crate) target_id: String,
    /// 浏览器 HTTP 调试地址，用于 Listener 建立独立连接
    pub(crate) browser_endpoint: Option<String>,
}

impl Page {
    pub(crate) fn new(
        client: Arc<CdpClient>,
        session_id: String,
        target_id: String,
        browser_endpoint: Option<String>,
    ) -> Self {
        Self {
            client,
            session_id,
            target_id,
            browser_endpoint,
        }
    }

    /// 打开网址（导航到 url）。与 DrissionPage 一致也可用 [Page::get]。
    pub fn goto(&self, url: &str) -> Result<(), CdpError> {
        self.client
            .send_with_session("Page.enable", None, Some(self.session_id.as_str()))?;
        let params = json!({ "url": url });
        let result = self.client.send_with_session(
            "Page.navigate",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        if let Some(err) = result.get("errorText").and_then(Value::as_str) {
            if !err.is_empty() {
                return Err(CdpError::Protocol {
                    id: None,
                    code: -1,
                    message: format!("Page.navigate failed: {}", err),
                });
            }
        }
        Ok(())
    }

    /// 访问网址（与 DrissionPage `get(url)` 一致，等价于 [Page::goto]）
    pub fn get(&self, url: &str) -> Result<(), CdpError> {
        self.goto(url)
    }

    /// 刷新页面（与 DrissionPage `refresh()` 一致，等价于 [Page::reload]）
    pub fn refresh(&self) -> Result<(), CdpError> {
        self.reload()
    }

    /// 刷新页面
    pub fn reload(&self) -> Result<(), CdpError> {
        self.client
            .send_with_session("Page.reload", None, Some(self.session_id.as_str()))?;
        Ok(())
    }

    /// 后退
    pub fn back(&self) -> Result<(), CdpError> {
        let result = self.client.send_with_session(
            "Page.getNavigationHistory",
            None,
            Some(self.session_id.as_str()),
        )?;
        let entries = result
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: "No navigation history is available".into(),
            })?;
        let current = result
            .get("currentIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if current == 0 {
            return Ok(());
        }
        let entry_id = entries
            .get(current - 1)
            .and_then(|e| e.get("id"))
            .and_then(Value::as_i64)
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: "No previous page is available in the navigation history".into(),
            })?;
        let params = json!({ "entryId": entry_id });
        self.client.send_with_session(
            "Page.navigateToHistoryEntry",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(())
    }

    /// 前进
    pub fn forward(&self) -> Result<(), CdpError> {
        let result = self.client.send_with_session(
            "Page.getNavigationHistory",
            None,
            Some(self.session_id.as_str()),
        )?;
        let entries = result
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: "No navigation history is available".into(),
            })?;
        let current = result
            .get("currentIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if current + 1 >= entries.len() {
            return Ok(());
        }
        let entry_id = entries
            .get(current + 1)
            .and_then(|e| e.get("id"))
            .and_then(Value::as_i64)
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: "No next page is available in the navigation history".into(),
            })?;
        let params = json!({ "entryId": entry_id });
        self.client.send_with_session(
            "Page.navigateToHistoryEntry",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(())
    }

    /// 页面标题
    pub fn title(&self) -> Result<String, CdpError> {
        let result = self.evaluate("document.title")?;
        Ok(result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// 当前 URL
    pub fn url(&self) -> Result<String, CdpError> {
        let result = self.evaluate("window.location.href")?;
        Ok(result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// 整页 HTML（document.documentElement.outerHTML）
    pub fn html(&self) -> Result<String, CdpError> {
        let result = self.evaluate("document.documentElement.outerHTML")?;
        Ok(result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// 执行 JS，返回 Runtime.evaluate 的 result（与 DrissionPage `run_js(script)` 一致）
    pub fn run_js(&self, script: &str) -> Result<Value, CdpError> {
        self.evaluate(script)
    }

    /// 执行 JS（可为 async），等待 Promise 解析后返回结果；适用于 fetch 等异步表达式
    pub fn run_js_await(&self, script: &str) -> Result<Value, CdpError> {
        // 不发 Runtime.enable：console 事件经 CDP 上网会触发 disable-devtool 计时检测
        let params = json!({
            "expression": script,
            "returnByValue": true,
            "awaitPromise": true,
            "userGesture": true
        });
        let result = self.client.send_with_session(
            "Runtime.evaluate",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 执行 JS，返回 Runtime.evaluate 的 result
    pub fn evaluate(&self, js: &str) -> Result<Value, CdpError> {
        // 同上：Runtime.evaluate 不依赖 Runtime.enable
        let params = json!({
            "expression": js,
            "returnByValue": true,
            "userGesture": true
        });
        let result = self.client.send_with_session(
            "Runtime.evaluate",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 发送 CDP Input 鼠标事件（真实事件、isTrusted 为 true），用于滑块等仅响应真实用户操作的组件。
    /// `event_type`: "mousePressed" | "mouseReleased" | "mouseMoved"
    /// `x`, `y`: 视口坐标（与 getBoundingClientRect 一致）
    pub fn dispatch_mouse_event(
        &self,
        event_type: &str,
        x: f64,
        y: f64,
        button: Option<&str>,
        click_count: Option<u32>,
    ) -> Result<(), CdpError> {
        self.client
            .send_with_session("Input.enable", None, Some(self.session_id.as_str()))?;
        let mut params = json!({
            "type": event_type,
            "x": x,
            "y": y
        });
        if let (Some(b), Some(c)) = (button, click_count) {
            params["button"] = json!(b);
            params["clickCount"] = json!(c);
        }
        self.client.send_with_session(
            "Input.dispatchMouseEvent",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(())
    }

    /// 按定位器查单个元素（与 DrissionPage `ele(locator)` 一致，等价于 [Page::element]）
    pub fn ele(&self, locator: &str) -> Result<Option<Element>, CdpError> {
        self.element(locator)
    }

    /// 按定位器查多个元素（与 DrissionPage `eles(locator)` 一致，等价于 [Page::elements]）
    pub fn eles(&self, locator: &str) -> Result<Vec<Element>, CdpError> {
        self.elements(locator)
    }

    /// 按定位器查单个元素（含 iframe 内；与 DrissionPage 一致使用 DOM.performSearch）
    pub fn element(&self, locator: &str) -> Result<Option<Element>, CdpError> {
        self._element_inner(locator)
            .map_err(|e| e.with_context(locator))
    }

    /// element 内部实现
    fn _element_inner(&self, locator: &str) -> Result<Option<Element>, CdpError> {
        let loc = Locator::parse(locator).map_err(|_| CdpError::Protocol {
            id: None,
            code: -1,
            message: format!(
                "Invalid locator: {}. Please check the locator syntax.",
                locator
            ),
        })?;
        // CSS 查询优先走 Runtime.querySelectorAll(主文档+同源 iframe)，直接拿 objectId，降低动态 DOM 下 nodeId 失效概率。
        if let Some(selector) = loc.to_css_selector() {
            let pairs = query_selector_all_including_same_origin_frames(
                &self.client,
                &self.session_id,
                &selector,
            )?;
            if let Some((id, oid)) = pairs.into_iter().next() {
                let b = get_backend_node_id(&self.client, &self.session_id, id).ok();
                return Ok(Some(Element::new_with_object_id(
                    Arc::clone(&self.client),
                    self.session_id.clone(),
                    id,
                    Some(oid),
                    b,
                )));
            }
            // 同源 iframe 扫描无结果时，回退到主文档 root 下 DOM.querySelector（兼容极端场景）。
            // 回退失败不应导致查找报错，因为主扫描已正确返回无结果。
            if let Ok(root) = get_document_root(&self.client, &self.session_id) {
                if let Ok(Some(id)) =
                    query_selector(&self.client, &self.session_id, root, &selector)
                {
                    let b = get_backend_node_id(&self.client, &self.session_id, id).ok();
                    return Ok(Some(Element::new_with_backend(
                        Arc::clone(&self.client),
                        self.session_id.clone(),
                        id,
                        b,
                    )));
                }
            }
            return Ok(None);
        }
        if let Some(query) = loc.to_search_query() {
            let (search_id, result_count) =
                perform_search(&self.client, &self.session_id, &query, true)?;
            if result_count > 0 {
                let node_ids =
                    get_search_results(&self.client, &self.session_id, &search_id, 0, 1)?;
                discard_search_results(&self.client, &self.session_id, &search_id)?;
                if let Some(id) = node_ids.into_iter().next() {
                    return Ok(Some(Element::new(
                        Arc::clone(&self.client),
                        self.session_id.clone(),
                        id,
                    )));
                }
            } else {
                discard_search_results(&self.client, &self.session_id, &search_id)?;
            }
            return Ok(None);
        }
        if let Some(xpath) = loc.to_xpath_expression() {
            let xpath_escaped = serde_json::to_string(&xpath).map_err(CdpError::Json)?;
            let params = json!({
                "expression": format!("document.evaluate({}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue", xpath_escaped),
                "returnByValue": false
            });
            let result = self.client.send_with_session(
                "Runtime.evaluate",
                Some(params),
                Some(self.session_id.as_str()),
            )?;
            let obj_id = result
                .get("result")
                .and_then(|r| r.get("objectId"))
                .and_then(Value::as_str);
            if let Some(oid) = obj_id {
                let params = json!({ "objectId": oid });
                let res = self.client.send_with_session(
                    "DOM.requestNode",
                    Some(params),
                    Some(self.session_id.as_str()),
                )?;
                if let Some(nid) = res.get("nodeId").and_then(Value::as_i64) {
                    let b = get_backend_node_id(&self.client, &self.session_id, nid).ok();
                    return Ok(Some(Element::new_with_object_id(
                        Arc::clone(&self.client),
                        self.session_id.clone(),
                        nid,
                        Some(oid.to_string()),
                        b,
                    )));
                }
            }
            Ok(None)
        } else {
            Ok(None)
        }
    }

    /// 按定位器查多个元素（含 iframe 内；与 DrissionPage 一致使用 DOM.performSearch）
    pub fn elements(&self, locator: &str) -> Result<Vec<Element>, CdpError> {
        self._elements_inner(locator)
            .map_err(|e| e.with_context(locator))
    }

    /// elements 内部实现
    fn _elements_inner(&self, locator: &str) -> Result<Vec<Element>, CdpError> {
        let loc = Locator::parse(locator).map_err(|_| CdpError::Protocol {
            id: None,
            code: -1,
            message: format!(
                "Invalid locator: {}. Please check the locator syntax.",
                locator
            ),
        })?;
        // CSS 查询优先走 Runtime.querySelectorAll(主文档+同源 iframe)，返回 nodeId+objectId 组合。
        if let Some(selector) = loc.to_css_selector() {
            let pairs = query_selector_all_including_same_origin_frames(
                &self.client,
                &self.session_id,
                &selector,
            )?;
            let backends: Vec<Option<i64>> = pairs
                .iter()
                .map(|(nid, _)| get_backend_node_id(&self.client, &self.session_id, *nid).ok())
                .collect();
            return Ok(pairs
                .into_iter()
                .zip(backends)
                .map(|((id, oid), b)| {
                    Element::new_with_object_id(
                        Arc::clone(&self.client),
                        self.session_id.clone(),
                        id,
                        Some(oid),
                        b,
                    )
                })
                .collect());
        }
        if let Some(query) = loc.to_search_query() {
            let (search_id, result_count) =
                perform_search(&self.client, &self.session_id, &query, true)?;
            let node_ids = if result_count > 0 {
                get_search_results(&self.client, &self.session_id, &search_id, 0, result_count)?
            } else {
                Vec::new()
            };
            discard_search_results(&self.client, &self.session_id, &search_id)?;
            if !node_ids.is_empty() {
                let backends: Vec<Option<i64>> = node_ids
                    .iter()
                    .map(|&nid| get_backend_node_id(&self.client, &self.session_id, nid).ok())
                    .collect();
                return Ok(node_ids
                    .into_iter()
                    .zip(backends)
                    .map(|(id, b)| {
                        Element::new_with_backend(
                            Arc::clone(&self.client),
                            self.session_id.clone(),
                            id,
                            b,
                        )
                    })
                    .collect());
            }
        }
        if let Some(xpath) = loc.to_xpath_expression() {
            let xpath_escaped = serde_json::to_string(&xpath).map_err(CdpError::Json)?;
            let expr = format!(
                "var r=document.evaluate({}, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null); var a=[]; for(var i=0;i<r.snapshotLength;i++) a.push(r.snapshotItem(i)); a",
                xpath_escaped
            );
            let params = json!({ "expression": expr, "returnByValue": false });
            let result = self.client.send_with_session(
                "Runtime.evaluate",
                Some(params),
                Some(self.session_id.as_str()),
            )?;
            let obj_id = result
                .get("result")
                .and_then(|r| r.get("objectId"))
                .and_then(Value::as_str);
            let mut node_ids = Vec::new();
            if let Some(oid) = obj_id {
                let params = json!({ "functionDeclaration": "function(){ return this.length; }", "objectId": oid });
                let res = self.client.send_with_session(
                    "Runtime.callFunctionOn",
                    Some(params),
                    Some(self.session_id.as_str()),
                )?;
                let len = res
                    .get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                for i in 0..len {
                    let params = json!({ "functionDeclaration": "function(i){ return this[i]; }", "objectId": oid, "arguments": [{"value": i}] });
                    let res = self.client.send_with_session(
                        "Runtime.callFunctionOn",
                        Some(params),
                        Some(self.session_id.as_str()),
                    )?;
                    let eid = res
                        .get("result")
                        .and_then(|r| r.get("objectId"))
                        .and_then(Value::as_str);
                    if let Some(eid) = eid {
                        let params = json!({ "objectId": eid });
                        let node_res = self.client.send_with_session(
                            "DOM.requestNode",
                            Some(params),
                            Some(self.session_id.as_str()),
                        )?;
                        if let Some(nid) = node_res.get("nodeId").and_then(Value::as_i64) {
                            node_ids.push(nid);
                        }
                    }
                }
            }
            let backends: Vec<Option<i64>> = node_ids
                .iter()
                .map(|&nid| get_backend_node_id(&self.client, &self.session_id, nid).ok())
                .collect();
            Ok(node_ids
                .into_iter()
                .zip(backends)
                .map(|(id, b)| {
                    Element::new_with_backend(
                        Arc::clone(&self.client),
                        self.session_id.clone(),
                        id,
                        b,
                    )
                })
                .collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// 按定位器获取一个 iframe，作为 [Frame] 可在其内查找元素（与 DrissionPage `get_frame` 一致）。仅同源 iframe 可转为 Frame。
    pub fn get_frame(&self, locator: &str) -> Result<Option<Frame>, CdpError> {
        let ele = self.element(locator)?;
        match ele {
            Some(el) if el.is_frame()? => el.into_frame(),
            _ => Ok(None),
        }
    }

    /// 按定位器获取所有同源 iframe（默认 `tag:iframe`），与 DrissionPage `get_frames` 一致。
    pub fn get_frames(&self, locator: Option<&str>) -> Result<Vec<Frame>, CdpError> {
        let loc = locator.unwrap_or("tag:iframe");
        let elements = self.elements(loc)?;
        let mut frames = Vec::new();
        for e in elements {
            if e.is_frame()? {
                if let Ok(Some(f)) = e.into_frame() {
                    frames.push(f);
                }
            }
        }
        Ok(frames)
    }

    /// 点击定位器匹配的第一个元素
    pub fn click(&self, locator: &str) -> Result<(), CdpError> {
        let el = self.element(locator)?.ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: format!("Element not found for locator: {}", locator),
        })?;
        el.click()
    }

    /// 向定位器匹配的第一个元素输入文本
    pub fn input(&self, locator: &str, text: &str) -> Result<(), CdpError> {
        let el = self.element(locator)?.ok_or_else(|| CdpError::Protocol {
            id: None,
            code: -1,
            message: format!("Element not found for locator: {}", locator),
        })?;
        el.input(text)
    }

    /// 等待定位器匹配到元素，超时返回 Ok(None)
    /// 等待定位器匹配到元素（DOM 存在）
    pub fn wait(&self, locator: &str, timeout: Duration) -> Result<Option<Element>, CdpError> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if let Some(el) = self.element(locator)? {
                return Ok(Some(el));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(None)
    }

    /// 等待定位器匹配到元素（默认 30 秒）
    pub fn wait_element(&self, locator: &str) -> Result<Option<Element>, CdpError> {
        self.wait(locator, Duration::from_secs(30))
    }

    /// 等待元素可见（存在且 is_displayed）
    pub fn wait_visible(&self, locator: &str, timeout: Duration) -> Result<Element, CdpError> {
        let el = self
            .wait(locator, timeout)?
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: format!(
                    "Timed out while waiting for element to become visible: {}",
                    locator
                ),
            })?;
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if el.is_displayed().unwrap_or(false) {
                return Ok(el);
            }
            std::thread::sleep(Duration::from_millis(200));
            if let Some(e) = self.element(locator)? {
                if e.is_displayed().unwrap_or(false) {
                    return Ok(e);
                }
            }
        }
        Err(CdpError::Protocol {
            id: None,
            code: -1,
            message: format!(
                "Timed out while waiting for element to become visible: {}",
                locator
            ),
        })
    }

    /// 等待元素隐藏或不存在
    pub fn wait_hidden(&self, locator: &str, timeout: Duration) -> Result<(), CdpError> {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            match self.element(locator)? {
                None => return Ok(()),
                Some(el) => {
                    if !el.is_displayed().unwrap_or(true) {
                        return Ok(());
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(CdpError::Protocol {
            id: None,
            code: -1,
            message: format!(
                "Timed out while waiting for element to become hidden: {}",
                locator
            ),
        })
    }

    /// 等待网络空闲（简化实现：固定等待）
    pub fn wait_network_idle(&self) -> Result<(), CdpError> {
        self.client
            .send_with_session("Network.enable", None, Some(self.session_id.as_str()))?;
        std::thread::sleep(Duration::from_millis(500));
        Ok(())
    }

    /// 获取当前页面 URL 下的 cookies（可选传入 urls 过滤）
    pub fn cookies(&self, urls: Option<&[String]>) -> Result<Vec<Cookie>, CdpError> {
        self.client
            .send_with_session("Network.enable", None, Some(self.session_id.as_str()))?;
        let params = match urls {
            Some(u) => json!({ "urls": u }),
            None => json!({}),
        };
        let result = self.client.send_with_session(
            "Network.getCookies",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        let list = result
            .get("cookies")
            .and_then(Value::as_array)
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: "Network.getCookies did not return any cookies".into(),
            })?;
        let cookies = list
            .iter()
            .filter_map(|c| {
                let name = c.get("name")?.as_str()?.to_string();
                let value = c.get("value")?.as_str()?.to_string();
                let domain = c.get("domain").and_then(Value::as_str).map(String::from);
                let path = c.get("path").and_then(Value::as_str).map(String::from);
                Some(Cookie {
                    name,
                    value,
                    domain,
                    path,
                })
            })
            .collect();
        Ok(cookies)
    }

    /// 设置 cookie（url 建议传入当前页 URL 以确定 domain/path）
    pub fn set_cookie(&self, cookie: &Cookie, url: Option<&str>) -> Result<(), CdpError> {
        self.client
            .send_with_session("Network.enable", None, Some(self.session_id.as_str()))?;
        let mut params = json!({ "name": cookie.name, "value": cookie.value });
        if let Some(u) = url {
            params["url"] = json!(u);
        }
        if let Some(ref d) = cookie.domain {
            params["domain"] = json!(d);
        }
        if let Some(ref p) = cookie.path {
            params["path"] = json!(p);
        }
        self.client.send_with_session(
            "Network.setCookie",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(())
    }

    /// 删除指定 name 的 cookie（可选 url 限定）
    pub fn delete_cookie(&self, name: &str, url: Option<&str>) -> Result<(), CdpError> {
        self.client
            .send_with_session("Network.enable", None, Some(self.session_id.as_str()))?;
        let params = if let Some(u) = url {
            json!({ "name": name, "url": u })
        } else {
            json!({ "name": name })
        };
        self.client.send_with_session(
            "Network.deleteCookies",
            Some(params),
            Some(self.session_id.as_str()),
        )?;
        Ok(())
    }

    /// 截屏并保存到 path
    pub fn screenshot(&self, path: &str) -> Result<(), CdpError> {
        let result = self.client.send_with_session(
            "Page.captureScreenshot",
            Some(json!({ "format": "png" })),
            Some(self.session_id.as_str()),
        )?;
        let data_b64 =
            result
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| CdpError::Protocol {
                    id: None,
                    code: -1,
                    message: "Page.captureScreenshot did not return image data".into(),
                })?;
        let data = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data_b64)
            .map_err(|e| CdpError::Protocol {
                id: None,
                code: -1,
                message: format!("Failed to decode base64 data: {}", e),
            })?;
        std::fs::write(path, data).map_err(|e| CdpError::Protocol {
            id: None,
            code: -1,
            message: format!("Failed to write the file: {}", e),
        })?;
        Ok(())
    }

    /// 关闭当前 Tab
    pub fn close(&self) -> Result<(), CdpError> {
        let params = json!({ "targetId": self.target_id });
        self.client.send("Target.closeTarget", Some(params))?;
        Ok(())
    }

    /// 启动请求/响应监听器（独立 CDP 连接，接收 Network 事件并收集请求/响应数据）
    /// 需要当前 Page 来自 [Browser::connect] / [Browser::launch] / [Browser::new_tab]，以便有 `browser_endpoint`
    pub fn listen(&self) -> Result<Listener, CdpError> {
        let endpoint = self
            .browser_endpoint
            .as_deref()
            .ok_or_else(|| CdpError::Protocol {
                id: None,
                code: -1,
                message: "Unable to start the listener because browser_endpoint is missing. Get the Page from Browser::new_tab or Browser::tabs first.".into(),
            })?;
        Listener::start(endpoint, &self.target_id)
    }

    /// 启动监听并按 URL 过滤，只保留 URL 包含指定字符串的数据包。
    pub fn listen_url(&self, url_contains: &str) -> Result<Listener, CdpError> {
        let listener = self.listen()?;
        Ok(Listener::filter_url(listener, url_contains.to_string()))
    }

    /// 启动监听并按资源类型过滤（如 "XHR", "Fetch", "Document", "Script" 等）。
    pub fn listen_resource_type(&self, resource_type: &str) -> Result<Listener, CdpError> {
        let listener = self.listen()?;
        Ok(Listener::filter_resource_type(
            listener,
            resource_type.to_string(),
        ))
    }

    /// 当前 Tab 的 target ID（与 DrissionPage `tab_id` 一致）
    pub fn tab_id(&self) -> &str {
        &self.target_id
    }

    /// 获取当前焦点元素（activeElement）
    pub fn active_ele(&self) -> Result<Option<Element>, CdpError> {
        let result = self.evaluate("document.activeElement")?;
        let obj_id = result.get("objectId").and_then(Value::as_str);
        if let Some(oid) = obj_id {
            let params = json!({ "objectId": oid });
            let res = self.client.send_with_session(
                "DOM.requestNode",
                Some(params),
                Some(self.session_id.as_str()),
            )?;
            if let Some(nid) = res.get("nodeId").and_then(Value::as_i64) {
                return Ok(Some(Element::new(
                    Arc::clone(&self.client),
                    self.session_id.clone(),
                    nid,
                )));
            }
        }
        Ok(None)
    }

    /// 页面滚动到指定坐标（与 DrissionPage `scroll(x, y)` 一致）
    pub fn scroll(&self, x: i64, y: i64) -> Result<(), CdpError> {
        let script = format!(
            "window.scrollTo({}, {}); window.scrollX; window.scrollY;",
            x, y
        );
        self.evaluate(&script)?;
        Ok(())
    }

    /// 页面滚动到顶部
    pub fn scroll_to_top(&self) -> Result<(), CdpError> {
        self.evaluate("window.scrollTo(0, 0);")?;
        Ok(())
    }

    /// 页面滚动到底部
    pub fn scroll_to_bottom(&self) -> Result<(), CdpError> {
        self.evaluate("window.scrollTo(0, document.body.scrollHeight);")?;
        Ok(())
    }

    /// 按 delta 滚动（相对于当前位置）
    pub fn scroll_by(&self, delta_x: i64, delta_y: i64) -> Result<(), CdpError> {
        let script = format!(
            "window.scrollBy({}, {}); window.scrollX; window.scrollY;",
            delta_x, delta_y
        );
        self.evaluate(&script)?;
        Ok(())
    }

    /// 获取页面视口矩形（scrollWidth, scrollHeight, viewport width/height）
    pub fn rect(&self) -> Result<Value, CdpError> {
        self.evaluate(
            "{ scrollWidth: document.documentElement.scrollWidth, scrollHeight: document.documentElement.scrollHeight, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight, scrollX: window.scrollX, scrollY: window.scrollY }",
        )
    }

    /// 停止页面加载（与 DrissionPage `stop_loading()` 一致）
    pub fn stop_loading(&self) -> Result<(), CdpError> {
        self.client
            .send_with_session("Page.stopLoading", None, Some(self.session_id.as_str()))?;
        Ok(())
    }

    /// 处理 JavaScript 弹窗（alert/confirm/prompt）
    /// `accept` - 是否接受（true=确认，false=取消）
    /// `prompt_text` - 若弹窗是 prompt，可传入输入文本
    pub fn handle_alert(&self, accept: bool, prompt_text: Option<&str>) -> Result<(), CdpError> {
        self.client.send_with_session(
            "Page.handleJavaScriptDialog",
            Some(json!({
                "accept": accept,
                "promptText": prompt_text.unwrap_or("")
            })),
            Some(self.session_id.as_str()),
        )?;
        Ok(())
    }

    /// 等待 JavaScript 弹窗出现并处理
    /// `accept` - 是否接受
    /// `prompt_text` - prompt 输入文本（可选）
    /// `timeout` - 超时时间
    pub fn wait_alert(
        &self,
        accept: bool,
        prompt_text: Option<&str>,
        timeout: Duration,
    ) -> Result<bool, CdpError> {
        let deadline = std::time::Instant::now() + timeout;
        // 轮询直到弹窗出现或超时
        loop {
            if std::time::Instant::now() > deadline {
                return Ok(false);
            }
            // 尝试发送 handleJavaScriptDialog，如果当前没有弹窗会返回错误
            match self.client.send_with_session(
                "Page.handleJavaScriptDialog",
                Some(json!({
                    "accept": accept,
                    "promptText": prompt_text.unwrap_or("")
                })),
                Some(self.session_id.as_str()),
            ) {
                Ok(_) => return Ok(true),
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// 读取 SessionStorage（不传 key 则返回全部）
    pub fn session_storage(&self, key: Option<&str>) -> Result<Option<String>, CdpError> {
        let script = match key {
            Some(k) => format!("sessionStorage.getItem('{}');", serde_json::to_string(k).map_err(CdpError::Json)?),
            None => "JSON.stringify(Object.keys(sessionStorage).reduce((acc,k) => {{ acc[k] = sessionStorage.getItem(k); return acc; }}, {{}}));".to_string(),
        };
        let result = self.evaluate(&script)?;
        let s = result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("null");
        if s == "null" {
            Ok(None)
        } else {
            Ok(Some(s.to_string()))
        }
    }

    /// 写入 SessionStorage
    pub fn set_session_storage(&self, key: &str, value: &str) -> Result<(), CdpError> {
        let k = serde_json::to_string(key).map_err(CdpError::Json)?;
        let v = serde_json::to_string(value).map_err(CdpError::Json)?;
        self.evaluate(&format!("sessionStorage.setItem({}, {}); null", k, v))?;
        Ok(())
    }

    /// 删除 SessionStorage 条目
    pub fn delete_session_storage(&self, key: Option<&str>) -> Result<(), CdpError> {
        let script = match key {
            Some(k) => format!("sessionStorage.removeItem('{}'); null", k),
            None => "sessionStorage.clear(); null".to_string(),
        };
        self.evaluate(&script)?;
        Ok(())
    }

    /// 读取 LocalStorage（不传 key 则返回全部）
    pub fn local_storage(&self, key: Option<&str>) -> Result<Option<String>, CdpError> {
        let script = match key {
            Some(k) => format!("localStorage.getItem({});", serde_json::to_string(k).map_err(CdpError::Json)?),
            None => "JSON.stringify(Object.keys(localStorage).reduce((acc,k) => {{ acc[k] = localStorage.getItem(k); return acc; }}, {{}}));".to_string(),
        };
        let result = self.evaluate(&script)?;
        let s = result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("null");
        if s == "null" {
            Ok(None)
        } else {
            Ok(Some(s.to_string()))
        }
    }

    /// 写入 LocalStorage
    pub fn set_local_storage(&self, key: &str, value: &str) -> Result<(), CdpError> {
        let k = serde_json::to_string(key).map_err(CdpError::Json)?;
        let v = serde_json::to_string(value).map_err(CdpError::Json)?;
        self.evaluate(&format!("localStorage.setItem({}, {}); null", k, v))?;
        Ok(())
    }

    /// 删除 LocalStorage 条目
    pub fn delete_local_storage(&self, key: Option<&str>) -> Result<(), CdpError> {
        let script = match key {
            Some(k) => format!("localStorage.removeItem('{}'); null", k),
            None => "localStorage.clear(); null".to_string(),
        };
        self.evaluate(&script)?;
        Ok(())
    }

    /// 清除多种缓存（与 DrissionPage `clear_cache` 一致）
    /// `clear_session_storage` - 清除 SessionStorage
    /// `clear_local_storage` - 清除 LocalStorage
    /// `clear_cookies` - 清除当前页 cookies
    /// `clear_cache` - 清除 CDP 缓存
    pub fn clear_cache(
        &self,
        clear_session_storage: bool,
        clear_local_storage: bool,
        clear_cookies: bool,
        clear_cache: bool,
    ) -> Result<(), CdpError> {
        if clear_session_storage {
            let _ = self.delete_session_storage(None);
        }
        if clear_local_storage {
            let _ = self.delete_local_storage(None);
        }
        if clear_cookies {
            let _ = self.client.send_with_session(
                "Network.clearBrowserCookies",
                None,
                Some(self.session_id.as_str()),
            );
        }
        if clear_cache {
            self.client.send_with_session(
                "Network.clearBrowserCache",
                None,
                Some(self.session_id.as_str()),
            )?;
        }
        Ok(())
    }

    /// 直接发送 CDP 命令并返回结果（与 DrissionPage `run_cdp` 一致）
    pub fn run_cdp(&self, method: &str, params: Option<Value>) -> Result<Value, CdpError> {
        self.client
            .send_with_session(method, params, Some(self.session_id.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_construction() {
        let c = Cookie {
            name: "session".into(),
            value: "abc123".into(),
            domain: Some("example.com".into()),
            path: Some("/".into()),
        };
        assert_eq!(c.name, "session");
        assert_eq!(c.value, "abc123");
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.path.as_deref(), Some("/"));
    }

    #[test]
    fn cookie_minimal() {
        let c = Cookie {
            name: "n".into(),
            value: "v".into(),
            domain: None,
            path: None,
        };
        assert_eq!(c.name, "n");
        assert!(c.domain.is_none());
    }
}
