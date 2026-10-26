use reqwest::Client;

pub async fn user(client: &Client, id: u32) {
    let _ = client.get("https://api.example.com/users").send().await;
    let _ = client.delete(format!("https://api.example.com/users/{id}")).send().await;
}

pub async fn create() {
    let client = Client::builder().build().unwrap();
    let _ = client.post("https://api.example.com/users").send().await;
}
