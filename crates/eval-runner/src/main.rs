//! `eval-runner serve | keygen | public-key` (ADR-0082). Under the restricted
//! SSH key only `serve` runs: it reads one policy request on stdin and prints
//! one signed receipt, or `{"error": "<code>"}` with exit status 1.
use std::io::Read;

fn main() {
    let result = run();
    match result {
        Ok(text) => println!("{text}"),
        Err(code) => {
            println!("{}", serde_json::json!({ "error": code }));
            std::process::exit(1);
        }
    }
}

fn run() -> Result<String, &'static str> {
    let store = eval_runner::store_root().ok_or("invalid_store")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve"] => {
            let mut request = Vec::new();
            std::io::stdin()
                .take(semantic::evaluation::locked::MAX_REQUEST_BYTES as u64 + 1)
                .read_to_end(&mut request)
                .map_err(|_| "invalid_request")?;
            let model = eval_runner::model(&store)?;
            let provider = llm::OllamaProvider::new(
                llm::OllamaProvider::DEFAULT_URL,
                model,
                std::time::Duration::from_secs(600),
            )
            .map_err(|_| "embedding_failed")?;
            let signed = eval_runner::serve(&store, &request, &provider)?;
            serde_json::to_string(&signed).map_err(|_| "store_io")
        }
        ["keygen"] => eval_runner::keygen(&store),
        ["public-key"] => eval_runner::public_key(&store),
        _ => Err("usage"),
    }
}
