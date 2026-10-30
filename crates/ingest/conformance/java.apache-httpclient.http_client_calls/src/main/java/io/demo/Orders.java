package io.demo;

import org.apache.http.client.methods.HttpGet;
import org.apache.http.client.methods.HttpPost;
import org.apache.http.impl.client.CloseableHttpClient;

class Orders {
    private final CloseableHttpClient client;

    Orders(CloseableHttpClient client) {
        this.client = client;
    }

    void sync() throws Exception {
        client.execute(new HttpGet("https://api.example.com/orders"));
        client.execute(new HttpPost("https://api.example.com/orders"));
    }
}
