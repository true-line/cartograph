//! Analyst@20 ordering only: no field relocation, deletion or array sorting.
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;

const LEADING: &[&str] = &[
    "schema_version",
    "policy",
    "question",
    "completion_contract",
    "limits",
    "coverage",
    "coverage_qualification",
    "context_id",
    "graph_snapshot_id",
    "scope_snapshot_id",
    "inspection_requirements_hash",
    "prior_findings_as_t3_history",
    "coverage_details",
    "compact_context",
    "citations",
];
const HISTORY: &[&str] = &["contract", "prior_queries", "latest_query", "registry_hash"];
const CONTEXT: &[&str] = &["facts", "evidence_options", "query_history"];

pub(super) struct OrderedPrompt<'a>(pub &'a Value);
struct OrderedObject<'a>(&'a Value, &'a [&'a str], bool);
impl Serialize for OrderedPrompt<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        OrderedObject(self.0, LEADING, true).serialize(serializer)
    }
}
impl Serialize for OrderedObject<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Some(object) = self.0.as_object() else {
            return self.0.serialize(serializer);
        };
        let mut map = serializer.serialize_map(Some(object.len()))?;
        for key in self.1 {
            if let Some(value) = object.get(*key) {
                if self.2 && *key == "compact_context" {
                    map.serialize_entry(key, &OrderedObject(value, CONTEXT, false))?;
                } else if !self.2 && *key == "query_history" {
                    map.serialize_entry(key, &OrderedObject(value, HISTORY, false))?;
                } else {
                    map.serialize_entry(key, value)?;
                }
            }
        }
        // Preserve every unlisted field, including future additions, deterministically.
        for (key, value) in object {
            if !self.1.contains(&key.as_str()) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn analyst20_ordering_preserves_values_and_growing_prefix() {
        // AC-0566: nested controls, Unicode, unknown keys and arrays survive exactly.
        let mut value = serde_json::json!({"schema_version":9,"completion_contract":{"fixed":"λ\n"},
            "coverage_details":[{"fixed":"coverage"}],"compact_context":{"facts":[{"id":"a"}],
            "evidence_options":[],"query_history":{"registry_hash":"old"},"future":"retained"},
            "citations":[],"turn_progress":{"revision":1},"unknown":[null,true]});
        let first = serde_json::to_string(&OrderedPrompt(&value)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&first).unwrap(), value);
        value["turn_progress"]["revision"] = 2.into();
        let next = serde_json::to_string(&OrderedPrompt(&value)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&next).unwrap(), value);
        let boundary = first.find("\"turn_progress\"").unwrap();
        assert_eq!(&first[..boundary], &next[..boundary]);
        assert!(first.find("\"completion_contract\"") < first.find("\"compact_context\""));
        assert!(first.find("\"facts\"") < first.find("\"query_history\""));
    }
    #[test]
    fn analyst20_ordered_prompt_reaches_http_with_shared_prefix() {
        // AC-0566: synthetic loopback only; verifies real firewall/provider transport.
        use llm::bounded::{CallDirective, CompletionControl, CompletionLimits};
        use llm::{
            AnalysisTier, CompletionAction, CompletionPayload, ConsentGrant, EgressFirewall,
            EgressPolicy, LlmProvider, OllamaProvider,
        };
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};
        let mut observed = Vec::new();
        for revision in [1, 2] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    assert!(headers.len() < 16384);
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                let headers = String::from_utf8(headers).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                assert!(length < 131072);
                let mut body = vec![0; length];
                stream.read_exact(&mut body).unwrap();
                let reply = serde_json::json!({"model":"layout-fixture","message":{"role":"assistant","content":"{}"},
                    "done":true,"done_reason":"stop","prompt_eval_count":1,"eval_count":1}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
                serde_json::from_slice::<Value>(&body).unwrap()
            });
            let value = serde_json::json!({"schema_version":9,"completion_contract":{"fixed":"bounded evidence only"},
                "coverage_details":[{"inventory":"fixed"}],"compact_context":{"facts":[{"id":"a"}],
                "evidence_options":[],"query_history":{"contract":"history","prior_queries":[],"latest_query":null,"registry_hash":"fixed"}},
                "citations":[],"turn_progress":{"revision":revision}});
            let mut payload = CompletionPayload {
                system: crate::investigation::SpecialistId::DomainAnalystV20.prompt(),
                prompt: String::new(),
                spans: vec![],
                response_schema: None,
            };
            super::super::wire::replace_prompt(&mut payload, &value, 131072).unwrap();
            let expected = payload.prompt.clone();
            let action = CompletionAction {
                action_id: format!("layout-{revision}"),
                tier: AnalysisTier::Agentic,
                payload,
            };
            let provider =
                OllamaProvider::new(&endpoint, "layout-fixture", Duration::from_secs(5)).unwrap();
            let firewall = EgressFirewall::new(EgressPolicy::default());
            let prepared = firewall
                .prepare_bounded(&provider, &action, &CompletionLimits::default())
                .unwrap();
            let grant = ConsentGrant::from_preview(&prepared.preview().egress);
            let authorized = firewall
                .authorize_bounded(&provider, &prepared, Some(&grant))
                .unwrap();
            provider
                .complete_bounded(
                    &authorized,
                    &CompletionControl {
                        deadline: Instant::now() + Duration::from_secs(5),
                        check: &|| CallDirective::Continue,
                        dispatching: None,
                    },
                )
                .unwrap();
            let wire = server.join().unwrap();
            let user = wire["messages"][1]["content"].as_str().unwrap();
            assert_eq!(user, format!("{expected}\n\nEvidence spans (JSON):\n[]"));
            observed.push(user.to_owned());
        }
        let boundary = observed[0].find("\"turn_progress\"").unwrap();
        assert_eq!(&observed[0][..boundary], &observed[1][..boundary]);
        assert_ne!(observed[0], observed[1]);
    }
}
