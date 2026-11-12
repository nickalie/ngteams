//! Native response restrictions are parsed before a paused response is released to the renderer.
#[cfg(any(windows, test))]
use serde_json::{Value, json};

#[cfg(any(windows, test))]
const CSP: &str = "connect-src https:; worker-src 'none'; object-src 'none'; frame-src https:";
#[cfg(any(windows, target_os = "macos"))]
pub(crate) const INIT: &str = r"(() => {
    for (const name of ['RTCPeerConnection', 'webkitRTCPeerConnection', 'RTCDataChannel', 'WebTransport',
                        'WebSocket', 'WebSocketStream', 'Worker', 'SharedWorker']) {
        Object.defineProperty(globalThis, name, { value: undefined, writable: false, configurable: false });
    }
    if (globalThis.ServiceWorkerContainer) {
        Object.defineProperty(ServiceWorkerContainer.prototype, 'register', { value: undefined, writable: false, configurable: false });
    }
    Object.defineProperty(Navigator.prototype, 'sendBeacon', { value: undefined, writable: false, configurable: false });
})()";

#[cfg(any(windows, test))]
pub(crate) fn response_parameters(event: &Value) -> Option<(&'static str, Value)> {
    let id = event.get("requestId")?.as_str()?;
    let Some(status) = event.get("responseStatusCode").and_then(Value::as_i64) else {
        event.get("responseErrorReason")?.as_str()?;
        return Some(("Fetch.continueRequest", json!({"requestId":id})));
    };
    if !(100..=599).contains(&status) {
        return None;
    }
    let mut headers = event.get("responseHeaders")?.as_array()?.clone();
    headers.push(json!({"name":"Content-Security-Policy", "value":CSP}));
    headers.push(json!({"name":"X-DNS-Prefetch-Control", "value":"off"}));
    let mut reply = json!({"requestId":id, "responseCode":status, "responseHeaders":headers});
    // Chromium routes continueResponse through FulfillRequest, which rejects an explicitly empty phrase.
    // HTTP/2 carries none: omit it so Chromium supplies the status's standard phrase, preserving all headers.
    if let Some(phrase) = event
        .get("responseStatusText")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        reply["responsePhrase"] = phrase.into();
    }
    Some(("Fetch.continueResponse", reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_restrictions_preserve_server_headers_and_redirects() {
        for code in [200, 302, 307] {
            let source = json!({"requestId":"synthetic-1", "responseStatusCode":code, "responseStatusText":"Synthetic status",
                "responseHeaders":[{"name":"Set-Cookie","value":"made-up=value; Secure"},
                    {"name":"Location","value":"https://sso.example.test/next"},
                    {"name":"Content-Security-Policy","value":"script-src 'self'"}]});
            let (method, reply) = response_parameters(&source).unwrap();
            assert_eq!(method, "Fetch.continueResponse");
            assert_eq!(reply["responseCode"], code);
            assert_eq!(reply["responsePhrase"], "Synthetic status");
            let headers = reply["responseHeaders"].as_array().unwrap();
            assert_eq!(&headers[..3], source["responseHeaders"].as_array().unwrap());
            assert_eq!(headers[3]["value"], CSP);
            assert_eq!(headers[4]["value"], "off");
        }
    }

    #[test]
    fn an_empty_http2_phrase_uses_chromiums_default_and_preserves_status_and_headers() {
        for phrase in [None, Some("")] {
            let mut event = json!({"requestId":"synthetic-http2", "responseStatusCode":200,
                "responseHeaders":[{"name":"Content-Type","value":"text/html"}]});
            if let Some(phrase) = phrase {
                event["responseStatusText"] = phrase.into();
            }
            let (_, reply) = response_parameters(&event).unwrap();
            assert!(reply.get("responsePhrase").is_none());
            assert_eq!(reply["responseCode"], 200);
            assert_eq!(reply["responseHeaders"][0], event["responseHeaders"][0]);
            assert_eq!(reply["responseHeaders"][1]["value"], CSP);
        }
    }

    #[test]
    fn failed_responses_continue_without_fabricating_a_status_or_body() {
        let (method, reply) =
            response_parameters(&json!({"requestId":"synthetic-2", "responseErrorReason":"Aborted"})).unwrap();
        assert_eq!(method, "Fetch.continueRequest");
        assert_eq!(reply, json!({"requestId":"synthetic-2"}));
        for bad in [
            json!({}),
            json!({"requestId":1}),
            json!({"requestId":"x", "responseStatusCode":200}),
            json!({"requestId":"x", "responseStatusCode":200, "responseHeaders":{}}),
        ] {
            assert!(response_parameters(&bad).is_none());
        }
    }
}
