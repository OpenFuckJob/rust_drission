//! 异步页面（Tab）API

use crate::cdp::{AsyncCdpClient, CdpError};
use crate::listener::AsyncListener;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use super::Cookie;

/// 异步 CDP 执行器，用于后续 AsyncPage/AsyncElement/AsyncFrame 共享 session 调用逻辑。
#[async_trait]
pub trait AsyncCdpExecutor {
    async fn run_cdp(&self, method: &str, params: Option<Value>) -> Result<Value, CdpError>;
}

/// 单个异步 Tab 页面，对应 CDP 的一个 target session。
#[derive(Clone)]
pub struct AsyncPage {
    pub(crate) client: Arc<AsyncCdpClient>,
    pub(crate) session_id: String,
    pub(crate) target_id: String,
    pub(crate) browser_endpoint: Option<String>,
}

impl AsyncPage {
    pub(crate) fn new(
        client: Arc<AsyncCdpClient>,
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

    /// 打开网址（导航到 url）。与 DrissionPage 一致也可用 [AsyncPage::get]。
    pub async fn goto(&self, url: &str) -> Result<(), CdpError> {
        self.run_cdp("Page.enable", None).await?;
        let result = self
            .run_cdp("Page.navigate", Some(json!({ "url": url })))
            .await?;
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

    /// 访问网址（与 DrissionPage `get(url)` 一致，等价于 [AsyncPage::goto]）。
    pub async fn get(&self, url: &str) -> Result<(), CdpError> {
        self.goto(url).await
    }

    /// 刷新页面。
    pub async fn reload(&self) -> Result<(), CdpError> {
        self.run_cdp("Page.reload", None).await?;
        Ok(())
    }

    /// 页面标题。
    pub async fn title(&self) -> Result<String, CdpError> {
        let result = self.evaluate("document.title").await?;
        Ok(result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// 当前 URL。
    pub async fn url(&self) -> Result<String, CdpError> {
        let result = self.evaluate("window.location.href").await?;
        Ok(result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// 整页 HTML（document.documentElement.outerHTML）。
    pub async fn html(&self) -> Result<String, CdpError> {
        let result = self.evaluate("document.documentElement.outerHTML").await?;
        Ok(result
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// 执行 JS，返回 Runtime.evaluate 的 result。
    pub async fn run_js(&self, script: &str) -> Result<Value, CdpError> {
        self.evaluate(script).await
    }

    /// 执行 JS（可为 async），等待 Promise 解析后返回结果。
    pub async fn run_js_await(&self, script: &str) -> Result<Value, CdpError> {
        // 不发 Runtime.enable：与同步版一致，避免 console 事件触发计时检测
        let params = json!({
            "expression": script,
            "returnByValue": true,
            "awaitPromise": true,
            "userGesture": true
        });
        let result = self.run_cdp("Runtime.evaluate", Some(params)).await?;
        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 执行 JS，返回 Runtime.evaluate 的 result。
    pub async fn evaluate(&self, js: &str) -> Result<Value, CdpError> {
        let params = json!({
            "expression": js,
            "returnByValue": true,
            "userGesture": true
        });
        let result = self.run_cdp("Runtime.evaluate", Some(params)).await?;
        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 获取当前页面 URL 下的 cookies（可选传入 urls 过滤）。
    pub async fn cookies(&self, urls: Option<&[String]>) -> Result<Vec<Cookie>, CdpError> {
        self.run_cdp("Network.enable", None).await?;
        let params = match urls {
            Some(u) => json!({ "urls": u }),
            None => json!({}),
        };
        let result = self.run_cdp("Network.getCookies", Some(params)).await?;
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

    /// 设置 cookie（url 建议传入当前页 URL 以确定 domain/path）。
    pub async fn set_cookie(&self, cookie: &Cookie, url: Option<&str>) -> Result<(), CdpError> {
        self.run_cdp("Network.enable", None).await?;
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
        self.run_cdp("Network.setCookie", Some(params)).await?;
        Ok(())
    }

    /// 删除指定 name 的 cookie（可选 url 限定）。
    pub async fn delete_cookie(&self, name: &str, url: Option<&str>) -> Result<(), CdpError> {
        self.run_cdp("Network.enable", None).await?;
        let params = if let Some(u) = url {
            json!({ "name": name, "url": u })
        } else {
            json!({ "name": name })
        };
        self.run_cdp("Network.deleteCookies", Some(params)).await?;
        Ok(())
    }

    /// 关闭当前 Tab。
    pub async fn close(&self) -> Result<(), CdpError> {
        let params = json!({ "targetId": self.target_id });
        self.client.send("Target.closeTarget", Some(params)).await?;
        Ok(())
    }

    /// 启动异步请求/响应监听器。
    pub async fn listen(&self) -> Result<AsyncListener, CdpError> {
        AsyncListener::start(Arc::clone(&self.client), self.session_id.clone()).await
    }

    /// 启动监听并按 URL 过滤，只保留 URL 包含指定字符串的数据包。
    pub async fn listen_url(&self, url_contains: &str) -> Result<AsyncListener, CdpError> {
        let listener = self.listen().await?;
        Ok(listener.filter_url(url_contains.to_string()))
    }

    /// 启动监听并按资源类型过滤。
    pub async fn listen_resource_type(
        &self,
        resource_type: &str,
    ) -> Result<AsyncListener, CdpError> {
        let listener = self.listen().await?;
        Ok(listener.filter_resource_type(resource_type.to_string()))
    }

    /// 等待网络空闲：inflight 请求归零并持续 idle_for，或在 timeout 后返回超时。
    pub async fn wait_network_idle_for(
        &self,
        idle_for: Duration,
        total_timeout: Duration,
    ) -> Result<(), CdpError> {
        self.run_cdp("Network.enable", None).await?;
        let mut events = self.client.subscribe_events();
        let session_id = self.session_id.clone();
        let deadline = tokio::time::Instant::now() + total_timeout;
        let mut inflight = 0usize;
        let mut idle_since = Some(tokio::time::Instant::now());

        loop {
            if let Some(since) = idle_since {
                let elapsed = since.elapsed();
                if elapsed >= idle_for {
                    return Ok(());
                }
                let wait_for = idle_for - elapsed;
                tokio::select! {
                    event = events.recv() => {
                        update_inflight(event, &session_id, &mut inflight, &mut idle_since)?;
                    }
                    _ = tokio::time::sleep(wait_for) => return Ok(()),
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(CdpError::Timeout(format!("Timed out while waiting for network idle after {:?}", total_timeout)));
                    }
                }
            } else {
                tokio::select! {
                    event = events.recv() => {
                        update_inflight(event, &session_id, &mut inflight, &mut idle_since)?;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(CdpError::Timeout(format!("Timed out while waiting for network idle after {:?}", total_timeout)));
                    }
                }
            }
        }
    }

    /// 当前 Tab 所属浏览器的 HTTP 调试端点（如 http://127.0.0.1:9222）。
    pub fn browser_endpoint(&self) -> Option<&str> {
        self.browser_endpoint.as_deref()
    }

    /// 当前 Tab 的 target ID。
    pub fn tab_id(&self) -> &str {
        &self.target_id
    }
}

#[async_trait]
impl AsyncCdpExecutor for AsyncPage {
    async fn run_cdp(&self, method: &str, params: Option<Value>) -> Result<Value, CdpError> {
        self.client
            .send_with_session(method, params, Some(self.session_id.as_str()))
            .await
    }
}

fn update_inflight(
    event: Result<crate::cdp::CdpEvent, tokio::sync::broadcast::error::RecvError>,
    session_id: &str,
    inflight: &mut usize,
    idle_since: &mut Option<tokio::time::Instant>,
) -> Result<(), CdpError> {
    let event = match event {
        Ok(event) => event,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            return Err(CdpError::Recv(format!(
                "CDP event receiver lagged by {} events",
                n
            )));
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            return Err(CdpError::ChannelClosed("CDP event channel closed".into()));
        }
    };
    if event.session_id.as_deref() != Some(session_id) {
        return Ok(());
    }
    match event.method.as_str() {
        "Network.requestWillBeSent" => {
            *inflight += 1;
            *idle_since = None;
        }
        "Network.loadingFinished" | "Network.loadingFailed" => {
            *inflight = inflight.saturating_sub(1);
            if *inflight == 0 {
                *idle_since = Some(tokio::time::Instant::now());
            }
        }
        _ => {}
    }
    Ok(())
}
