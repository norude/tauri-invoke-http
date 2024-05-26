// Copyright 2019-2021 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use {
  serde::Deserialize,
  serde_json::Value as JsonValue,
  std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, Mutex},
  },
  tauri::{
    ipc::{CallbackFn, InvokeBody, InvokeResponder, InvokeResponse},
    webview::InvokeRequest,
    AppHandle, Manager, Runtime, Url,
  },
  tiny_http::{Header, Method, Request, Response},
};
#[derive(Debug, Deserialize)]
pub struct RecievedMessage {
  pub cmd: String,
  pub callback: CallbackFn,
  pub error: CallbackFn,
  pub payload: JsonValue,
}
fn cors<R: std::io::Read>(request: &Request, r: &mut Response<R>, allowed_origins: &[String]) {
  if allowed_origins.iter().any(|s| s == "*") {
    r.add_header(Header::from_str("Access-Control-Allow-Origin: *").unwrap());
  } else if let Some(origin) = request.headers().iter().find(|h| h.field.equiv("Origin")) {
    if allowed_origins.iter().any(|o| o == &origin.value) {
      r.add_header(
        Header::from_str(&format!("Access-Control-Allow-Origin: {}", origin.value)).unwrap(),
      );
    }
  }
  r.add_header(Header::from_str("Access-Control-Allow-Headers: *").unwrap());
  r.add_header(Header::from_str("Access-Control-Allow-Methods: POST, OPTIONS").unwrap());
}

pub struct Invoke {
  allowed_origins: Vec<String>,
  port: u16,
  requests: Arc<Mutex<HashMap<u32, Request>>>,
}

impl Invoke {
  pub fn new<I: Into<String>, O: IntoIterator<Item = I>>(allowed_origins: O) -> Self {
    let port = portpicker::pick_unused_port().expect("failed to get unused port for invoke");
    let requests = Arc::new(Mutex::new(HashMap::new()));
    Self {
      allowed_origins: allowed_origins.into_iter().map(|o| o.into()).collect(),
      port,
      requests,
    }
  }

  pub fn start<R: Runtime>(&self, app: AppHandle<R>) {
    let server = tiny_http::Server::http(format!("localhost:{}", self.port)).unwrap();
    let requests = self.requests.clone();
    let allowed_origins = self.allowed_origins.clone();
    std::thread::spawn(move || {
      for mut request in server.incoming_requests() {
        let requests = requests.clone();
        let allowed_origins = allowed_origins.clone();
        if request.method() == &Method::Options {
          let mut r = Response::empty(200u16);
          cors(&request, &mut r, &allowed_origins);
          request.respond(r).unwrap();
          continue;
        }
        let url = request.url().to_string();
        let pieces = url.split('/').collect::<Vec<_>>();
        let window_label = pieces[1];

        if let Some(window) = app.get_webview_window(window_label) {
          let content_type = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Content-Type"))
            .map(|h| h.value.to_string())
            .unwrap_or_else(|| "application/json".into());

          let payload: InvokeRequest = if content_type == "application/json" {
            let mut content = String::new();
            request.as_reader().read_to_string(&mut content).unwrap();
            let origin = request
              .headers()
              .iter()
              .find(|h| h.field.equiv("Origin"))
              .map(|h| h.value.to_string())
              .expect("Invalid IPC request - No Origin");
            let message: RecievedMessage = serde_json::from_str(&content).unwrap();
            InvokeRequest {
              cmd: message.cmd,
              callback: message.callback,
              error: message.error,
              url: Url::parse(&origin).expect("invalid IPC request URL"),
              body: InvokeBody::Json(message.payload),
              headers: (&request
                .headers()
                .iter()
                .map(|h| (h.field.to_string(), h.value.to_string()))
                .collect::<HashMap<_, _>>())
                .try_into()
                .unwrap_or_default(),
              invoke_key: format!("FIXME: {}:{}:", file!(), line!()), //FIXME
            }
          } else {
            unimplemented!()
          };
          let req_key = payload.callback.0;
          requests.lock().unwrap().insert(req_key, request);
          window.on_message(
            payload,
            Box::new(move |_webview, _cmd, response, callback, _error| {
              let request = requests.lock().unwrap().remove(&callback.0).unwrap();
              let response = match response {
                InvokeResponse::Ok(r) => Ok(r),
                InvokeResponse::Err(e) => Err(e),
              };
              let status: u16 = if response.is_ok() { 200 } else { 400 };

              let mut r = match response {
                Ok(tauri::ipc::InvokeBody::Json(r)) => {
                  Response::from_string(serde_json::to_string(&r).unwrap())
                }
                Ok(tauri::ipc::InvokeBody::Raw(r)) => Response::from_data(r),
                Err(tauri::ipc::InvokeError(e)) => {
                  Response::from_string(serde_json::to_string(&e).unwrap())
                }
              }
              .with_status_code(status);
              cors(&request, &mut r, &allowed_origins);

              request.respond(r).unwrap();
            }),
          );
        } else {
          let mut r = Response::empty(404u16);
          cors(&request, &mut r, &allowed_origins);
          request.respond(r).unwrap();
        }
      }
    });
  }

  pub fn responder<R: Runtime>(&self) -> Box<InvokeResponder<R>> {
    let requests = self.requests.clone();
    let allowed_origins = self.allowed_origins.clone();
    Box::new(move |_webview, _cmd, response, callback, _error| {
      let request = requests.lock().unwrap().remove(&callback.0).unwrap();
      let response = match response {
        InvokeResponse::Ok(r) => Ok(r),
        InvokeResponse::Err(e) => Err(e),
      };
      let status: u16 = if response.is_ok() { 200 } else { 400 };

      let mut r = match response {
        Ok(tauri::ipc::InvokeBody::Json(r)) => {
          Response::from_string(serde_json::to_string(&r).unwrap())
        }
        Ok(tauri::ipc::InvokeBody::Raw(r)) => Response::from_data(r.clone()),
        Err(tauri::ipc::InvokeError(e)) => {
          Response::from_string(serde_json::to_string(&e).unwrap())
        }
      }
      .with_status_code(status);
      cors(&request, &mut r, &allowed_origins);

      request.respond(r).unwrap();
    })
  }

  pub fn initialization_script(&self) -> String {
    format!(
      "
        Object.defineProperty(__TAURI_INTERNALS__, 'postMessage', {{
          value: (message) => {{
            const request = new XMLHttpRequest();
            request.addEventListener('load', function () {{
              let arg
              let success = this.status === 200
              try {{
                arg = JSON.parse(this.response)
              }} catch (e) {{
                arg = e
                success = false
              }}
              window[`_${{success ? message.callback : message.error}}`](arg)
            }})
            request.open('POST', 'http://localhost:{}/' + window.__TAURI_INTERNALS__.metadata.currentWindow.label, true)
            request.setRequestHeader('Content-Type', 'application/json')
            request.send(JSON.stringify(message))
          }}
        }})
    ",
      self.port
    )
  }
}
